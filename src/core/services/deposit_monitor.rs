use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::Utc;
use fedimint_client::ClientHandleArc;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_wallet_client::{DepositStateV2, WalletClientModule};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio::time::interval;
use tracing::{debug, error, info, instrument, warn};

use crate::core::multimint::MultiMint;
use crate::events::{EventBus, FmcdEvent};

/// Efficient subscription manager for deposit operations to avoid recreating
/// subscriptions
type DepositSubscription = tokio::sync::mpsc::UnboundedReceiver<DepositStateV2>;
type SubscriptionMap = HashMap<OperationId, DepositSubscription>;

/// Information about an active deposit operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositInfo {
    pub operation_id: OperationId,
    pub federation_id: FederationId,
    pub address: String,
    pub correlation_id: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
}

/// Configuration for the deposit monitor service
#[derive(Debug, Clone)]
pub struct DepositMonitorConfig {
    /// How often to poll for deposit updates (default: 30 seconds)
    pub poll_interval: Duration,
    /// Maximum number of operations to monitor simultaneously per federation
    pub max_operations_per_federation: usize,
    /// How long to monitor an operation before giving up (default: 24 hours)
    pub operation_timeout: Duration,
}

impl Default for DepositMonitorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(30),
            max_operations_per_federation: 1000,
            operation_timeout: Duration::from_secs(24 * 60 * 60), // 24 hours
        }
    }
}

/// Service that monitors deposit addresses for incoming deposits
#[derive(Debug)]
pub struct DepositMonitor {
    event_bus: Arc<EventBus>,
    multimint: Arc<MultiMint>,
    config: DepositMonitorConfig,
    active_deposits: Arc<RwLock<HashMap<OperationId, DepositInfo>>>,
    /// Persistent subscriptions to avoid recreating them on each poll
    subscriptions: Arc<RwLock<SubscriptionMap>>,
    shutdown_tx: Arc<Mutex<Option<broadcast::Sender<()>>>>,
}

impl DepositMonitor {
    /// Create a new deposit monitor
    pub fn new(
        event_bus: Arc<EventBus>,
        multimint: Arc<MultiMint>,
        config: DepositMonitorConfig,
    ) -> Self {
        Self {
            event_bus,
            multimint,
            config,
            active_deposits: Arc::new(RwLock::new(HashMap::new())),
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the deposit monitoring service
    #[instrument(skip(self))]
    pub async fn start(&self) -> Result<()> {
        let (shutdown_tx, _) = broadcast::channel(1);
        {
            let mut tx_guard = self.shutdown_tx.lock().await;
            *tx_guard = Some(shutdown_tx.clone());
        }

        info!(
            poll_interval_secs = self.config.poll_interval.as_secs(),
            max_operations_per_federation = self.config.max_operations_per_federation,
            operation_timeout_hours = self.config.operation_timeout.as_secs() / 3600,
            "Starting deposit monitor service"
        );

        // Clone necessary data for the monitoring task
        let event_bus = self.event_bus.clone();
        let multimint = self.multimint.clone();
        let active_deposits = self.active_deposits.clone();
        let subscriptions = self.subscriptions.clone();
        let poll_interval = self.config.poll_interval;
        let operation_timeout = self.config.operation_timeout;

        // Spawn the monitoring task
        tokio::spawn(async move {
            let mut shutdown_rx = shutdown_tx.subscribe();
            let mut poll_timer = interval(poll_interval);

            loop {
                tokio::select! {
                    _ = poll_timer.tick() => {
                        if let Err(e) = Self::poll_deposits_impl(
                            &event_bus,
                            &multimint,
                            &active_deposits,
                            &subscriptions,
                            operation_timeout,
                        ).await {
                            error!(error = ?e, "Error during deposit polling");
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Deposit monitor received shutdown signal");
                        break;
                    }
                }
            }

            info!("Deposit monitor service stopped");
        });

        Ok(())
    }

    /// Stop the deposit monitoring service
    pub async fn stop(&self) -> Result<()> {
        let tx_guard = self.shutdown_tx.lock().await;
        if let Some(shutdown_tx) = tx_guard.as_ref() {
            let _ = shutdown_tx.send(());
        }
        Ok(())
    }

    /// Add a new deposit operation to monitor
    #[instrument(skip(self), fields(operation_id = ?deposit.operation_id))]
    pub async fn add_deposit(&self, deposit: DepositInfo) -> Result<()> {
        let operation_id = deposit.operation_id;
        let federation_id = deposit.federation_id;

        // Check federation limit
        {
            let deposits = self.active_deposits.read().await;
            let federation_count = deposits
                .values()
                .filter(|d| d.federation_id == federation_id)
                .count();

            if federation_count >= self.config.max_operations_per_federation {
                warn!(
                    federation_id = %federation_id,
                    current_count = federation_count,
                    max_count = self.config.max_operations_per_federation,
                    "Federation has reached maximum deposit operations limit"
                );
                return Err(anyhow!(
                    "Federation {} has reached maximum deposit operations limit ({})",
                    federation_id,
                    self.config.max_operations_per_federation
                ));
            }
        }

        // Add to active deposits
        {
            let mut deposits = self.active_deposits.write().await;
            deposits.insert(operation_id, deposit.clone());
        }

        info!(
            operation_id = ?operation_id,
            federation_id = %federation_id,
            address = %deposit.address,
            "Added deposit operation to monitor"
        );

        Ok(())
    }

    /// Remove a deposit operation from monitoring
    #[instrument(skip(self))]
    pub async fn remove_deposit(&self, operation_id: &OperationId) -> Option<DepositInfo> {
        let mut deposits = self.active_deposits.write().await;
        let removed = deposits.remove(operation_id);

        if let Some(ref deposit) = removed {
            info!(
                operation_id = ?operation_id,
                federation_id = %deposit.federation_id,
                "Removed deposit operation from monitoring"
            );
        }

        removed
    }

    /// Get statistics about active deposits
    pub async fn get_stats(&self) -> DepositMonitorStats {
        let deposits = self.active_deposits.read().await;
        let mut federation_counts: HashMap<FederationId, usize> = HashMap::new();

        for deposit in deposits.values() {
            *federation_counts.entry(deposit.federation_id).or_insert(0) += 1;
        }

        DepositMonitorStats {
            total_active_deposits: deposits.len(),
            federation_counts,
        }
    }

    /// Static implementation of deposit polling for use in spawned tasks
    async fn poll_deposits_impl(
        event_bus: &Arc<EventBus>,
        multimint: &Arc<MultiMint>,
        active_deposits: &Arc<RwLock<HashMap<OperationId, DepositInfo>>>,
        subscriptions: &Arc<RwLock<SubscriptionMap>>,
        operation_timeout: Duration,
    ) -> Result<()> {
        let deposits_to_check = {
            let deposits = active_deposits.read().await;
            deposits.clone()
        };

        if deposits_to_check.is_empty() {
            debug!("No active deposits to monitor");
            return Ok(());
        }

        debug!(
            active_deposits = deposits_to_check.len(),
            "Polling active deposits for updates with efficient subscription management"
        );

        // Group deposits by federation for efficient batch processing
        let mut deposits_by_federation: HashMap<FederationId, HashMap<OperationId, DepositInfo>> =
            HashMap::new();

        for (operation_id, deposit_info) in deposits_to_check {
            deposits_by_federation
                .entry(deposit_info.federation_id)
                .or_default()
                .insert(operation_id, deposit_info);
        }

        let now = Utc::now();
        let mut all_completed_operations = Vec::new();
        let mut all_timed_out_operations = Vec::new();

        // Process deposits efficiently by federation to reuse subscriptions
        for (federation_id, federation_deposits) in deposits_by_federation {
            // Check timeouts first
            let mut valid_deposits = HashMap::new();
            for (operation_id, deposit_info) in federation_deposits {
                if now
                    .signed_duration_since(deposit_info.created_at)
                    .to_std()
                    .unwrap_or_default()
                    <= operation_timeout
                {
                    valid_deposits.insert(operation_id, deposit_info);
                } else {
                    all_timed_out_operations.push(operation_id);
                }
            }

            if valid_deposits.is_empty() {
                continue;
            }

            // Get client for this federation
            let client = match multimint.get(&federation_id).await {
                Some(client) => client,
                None => {
                    warn!(federation_id = %federation_id, "Federation client not available");
                    continue;
                }
            };

            // Use efficient subscription-based checking
            if let Ok(completed) = Self::check_deposit_statuses_impl(
                &client,
                federation_id,
                &valid_deposits,
                subscriptions,
            )
            .await
            {
                for (operation_id, deposit_result) in completed {
                    if let Some(deposit_info) = valid_deposits.get(&operation_id) {
                        // Emit event
                        let event = FmcdEvent::DepositDetected {
                            operation_id: format!("{:?}", operation_id),
                            federation_id: deposit_info.federation_id.to_string(),
                            address: deposit_info.address.clone(),
                            amount_sat: deposit_result.amount_sat,
                            txid: deposit_result.txid,
                            correlation_id: deposit_info.correlation_id.clone(),
                            timestamp: Utc::now(),
                        };

                        if let Err(e) = event_bus.publish(event).await {
                            error!(operation_id = ?operation_id, error = ?e, "Failed to publish event");
                        } else {
                            info!(operation_id = ?operation_id, "Deposit detected");
                        }
                        all_completed_operations.push(operation_id);
                    }
                }
            }
        }

        // Remove completed and timed out operations
        if !all_completed_operations.is_empty() || !all_timed_out_operations.is_empty() {
            let mut deposits = active_deposits.write().await;
            let mut subscriptions = subscriptions.write().await;

            for operation_id in &all_completed_operations {
                deposits.remove(operation_id);
                subscriptions.remove(operation_id);
            }
            for operation_id in &all_timed_out_operations {
                deposits.remove(operation_id);
                subscriptions.remove(operation_id);
                warn!(
                    operation_id = ?operation_id,
                    "Deposit operation timed out and was removed from monitoring"
                );
            }

            info!(
                completed = all_completed_operations.len(),
                timed_out = all_timed_out_operations.len(),
                "Cleaned up deposit operations"
            );
        }

        Ok(())
    }

    /// Static implementation of deposit status checking for use in spawned
    /// tasks
    async fn check_deposit_statuses_impl(
        client: &ClientHandleArc,
        federation_id: FederationId,
        operations: &HashMap<OperationId, DepositInfo>,
        subscriptions: &Arc<RwLock<SubscriptionMap>>,
    ) -> Result<Vec<(OperationId, DepositResult)>> {
        let mut completed_deposits = Vec::new();
        let mut subscriptions = subscriptions.write().await;

        // Check each operation
        for operation_id in operations.keys() {
            // Try to get or create subscription for this operation
            let has_subscription = subscriptions.contains_key(operation_id);

            if !has_subscription {
                // Create new subscription only if we don't have one
                if let Ok(subscription) =
                    Self::create_deposit_subscription(client, *operation_id).await
                {
                    subscriptions.insert(*operation_id, subscription);
                    debug!(
                        operation_id = ?operation_id,
                        federation_id = %federation_id,
                        "Created new deposit subscription"
                    );
                } else {
                    warn!(
                        operation_id = ?operation_id,
                        federation_id = %federation_id,
                        "Failed to create deposit subscription"
                    );
                    continue;
                }
            }

            // Check for updates on existing subscription
            if let Some(subscription) = subscriptions.get_mut(operation_id) {
                // Non-blocking check for updates
                match subscription.try_recv() {
                    Ok(update) => {
                        match update {
                            DepositStateV2::Confirmed {
                                btc_deposited,
                                btc_out_point,
                            }
                            | DepositStateV2::Claimed {
                                btc_deposited,
                                btc_out_point,
                            } => {
                                completed_deposits.push((
                                    *operation_id,
                                    DepositResult {
                                        amount_sat: btc_deposited.to_sat(),
                                        txid: btc_out_point.txid.to_string(),
                                    },
                                ));

                                debug!(
                                    operation_id = ?operation_id,
                                    federation_id = %federation_id,
                                    amount_sat = btc_deposited.to_sat(),
                                    "Deposit completed successfully"
                                );
                            }
                            DepositStateV2::Failed(reason) => {
                                warn!(
                                    operation_id = ?operation_id,
                                    federation_id = %federation_id,
                                    reason = %reason,
                                    "Deposit failed, removing from monitoring"
                                );
                                // Will be removed from active_deposits by
                                // caller
                            }
                            _ => {
                                // Still waiting (WaitingForTransaction, etc.)
                                debug!(
                                    operation_id = ?operation_id,
                                    federation_id = %federation_id,
                                    state = ?update,
                                    "Deposit still in progress"
                                );
                            }
                        }
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        // No updates available, continue monitoring
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        // Subscription disconnected, remove it so it can be recreated
                        warn!(
                            operation_id = ?operation_id,
                            federation_id = %federation_id,
                            "Deposit subscription disconnected, will recreate on next poll"
                        );
                        subscriptions.remove(operation_id);
                    }
                }
            }
        }

        // Clean up subscriptions for operations that are no longer being monitored
        let active_operation_ids: std::collections::HashSet<_> = operations.keys().collect();
        subscriptions.retain(|op_id, _| active_operation_ids.contains(op_id));

        Ok(completed_deposits)
    }

    /// Create a new subscription for a deposit operation
    async fn create_deposit_subscription(
        client: &ClientHandleArc,
        operation_id: OperationId,
    ) -> Result<DepositSubscription> {
        let wallet_module = client
            .get_first_module::<WalletClientModule>()
            .map_err(|e| anyhow!("Failed to get wallet module: {}", e))?;

        let subscription = wallet_module.subscribe_deposit(operation_id).await?;

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        // Spawn a task to forward subscription updates to the channel
        let mut stream = subscription.into_stream();
        tokio::spawn(async move {
            while let Some(update) = stream.next().await {
                if tx.send(update).is_err() {
                    // Receiver dropped, stop forwarding
                    break;
                }
            }
        });

        Ok(rx)
    }
}

/// Result of a successful deposit detection
#[derive(Debug, Clone)]
struct DepositResult {
    amount_sat: u64,
    txid: String,
}

/// Statistics about the deposit monitor
#[derive(Debug, Clone, Serialize)]
pub struct DepositMonitorStats {
    pub total_active_deposits: usize,
    pub federation_counts: HashMap<FederationId, usize>,
}
