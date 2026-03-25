use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::Utc;
use fedimint_client::ClientHandleArc;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_core::Amount;
use fedimint_ln_client::{LightningClientModule, LnPayState, LnReceiveState};
use fedimint_wallet_client::{DepositStateV2, WalletClientModule, WithdrawState};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio::time::interval;
use tracing::{debug, error, info, instrument, warn};

use crate::core::multimint::MultiMint;
use crate::events::{EventBus, FmcdEvent};

/// Type of payment operation
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PaymentType {
    LightningReceive,
    LightningPay,
    OnchainDeposit,
    OnchainWithdraw,
}

/// Information about a payment operation being tracked
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentOperation {
    pub operation_id: OperationId,
    pub federation_id: FederationId,
    pub payment_type: PaymentType,
    pub amount_msat: Option<Amount>,
    pub created_at: chrono::DateTime<Utc>,
    pub metadata: Option<serde_json::Value>,
    pub correlation_id: Option<String>,
    /// Track if we've already attempted to claim the ecash
    pub claim_attempted: bool,
    /// Track if ecash was successfully claimed
    pub ecash_claimed: bool,
}

/// Configuration for the payment lifecycle manager
#[derive(Debug, Clone)]
pub struct PaymentLifecycleConfig {
    /// How often to poll for operation updates (default: 5 seconds)
    pub poll_interval: Duration,
    /// How long to monitor an operation before giving up (default: 24 hours)
    pub operation_timeout: Duration,
    /// Maximum number of operations to monitor simultaneously per federation
    pub max_operations_per_federation: usize,
    /// How long to wait for ecash claiming after payment is received (default:
    /// 30 seconds)
    pub claim_timeout: Duration,
}

impl Default for PaymentLifecycleConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(24 * 60 * 60), // 24 hours
            max_operations_per_federation: 1000,
            claim_timeout: Duration::from_secs(30),
        }
    }
}

/// Service that manages the complete lifecycle of payment operations
#[derive(Debug)]
pub struct PaymentLifecycleManager {
    event_bus: Arc<EventBus>,
    multimint: Arc<MultiMint>,
    config: PaymentLifecycleConfig,
    active_operations: Arc<RwLock<HashMap<OperationId, PaymentOperation>>>,
    shutdown_tx: Arc<Mutex<Option<broadcast::Sender<()>>>>,
}

impl PaymentLifecycleManager {
    /// Create a new payment lifecycle manager
    pub fn new(
        event_bus: Arc<EventBus>,
        multimint: Arc<MultiMint>,
        config: PaymentLifecycleConfig,
    ) -> Self {
        Self {
            event_bus,
            multimint,
            config,
            active_operations: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the payment lifecycle management service
    #[instrument(skip(self))]
    pub async fn start(&self) -> Result<()> {
        let (shutdown_tx, _) = broadcast::channel(1);
        {
            let mut tx_guard = self.shutdown_tx.lock().await;
            *tx_guard = Some(shutdown_tx.clone());
        }

        info!(
            poll_interval_secs = self.config.poll_interval.as_secs(),
            operation_timeout_hours = self.config.operation_timeout.as_secs() / 3600,
            claim_timeout_secs = self.config.claim_timeout.as_secs(),
            "Starting payment lifecycle manager service"
        );

        // First, recover any pending operations from all federations
        if let Err(e) = self.recover_pending_operations().await {
            error!(error = ?e, "Failed to recover pending operations on startup");
        }

        // Clone necessary data for the monitoring task
        let event_bus = self.event_bus.clone();
        let multimint = self.multimint.clone();
        let active_operations = self.active_operations.clone();
        let poll_interval = self.config.poll_interval;
        let operation_timeout = self.config.operation_timeout;
        let claim_timeout = self.config.claim_timeout;

        // Spawn the monitoring task
        tokio::spawn(async move {
            let mut shutdown_rx = shutdown_tx.subscribe();
            let mut poll_timer = interval(poll_interval);

            loop {
                tokio::select! {
                    _ = poll_timer.tick() => {
                        if let Err(e) = Self::process_all_operations(
                            &event_bus,
                            &multimint,
                            &active_operations,
                            operation_timeout,
                            claim_timeout,
                        ).await {
                            error!(error = ?e, "Error during payment lifecycle processing");
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Payment lifecycle manager received shutdown signal");
                        break;
                    }
                }
            }

            info!("Payment lifecycle manager service stopped");
        });

        Ok(())
    }

    /// Stop the payment lifecycle management service
    pub async fn stop(&self) -> Result<()> {
        let tx_guard = self.shutdown_tx.lock().await;
        if let Some(shutdown_tx) = tx_guard.as_ref() {
            let _ = shutdown_tx.send(());
        }
        Ok(())
    }

    /// Add a new Lightning receive operation to track
    #[instrument(skip(self))]
    pub async fn track_lightning_receive(
        &self,
        operation_id: OperationId,
        federation_id: FederationId,
        amount_msat: Amount,
        metadata: Option<serde_json::Value>,
        correlation_id: Option<String>,
    ) -> Result<()> {
        let operation = PaymentOperation {
            operation_id,
            federation_id,
            payment_type: PaymentType::LightningReceive,
            amount_msat: Some(amount_msat),
            created_at: Utc::now(),
            metadata,
            correlation_id,
            claim_attempted: false,
            ecash_claimed: false,
        };

        self.add_operation(operation).await
    }

    /// Add a new Lightning pay operation to track
    #[instrument(skip(self))]
    pub async fn track_lightning_pay(
        &self,
        operation_id: OperationId,
        federation_id: FederationId,
        amount_msat: Amount,
        metadata: Option<serde_json::Value>,
    ) -> Result<()> {
        let operation = PaymentOperation {
            operation_id,
            federation_id,
            payment_type: PaymentType::LightningPay,
            amount_msat: Some(amount_msat),
            created_at: Utc::now(),
            metadata,
            correlation_id: None,
            claim_attempted: false,
            ecash_claimed: false,
        };

        self.add_operation(operation).await
    }

    /// Add a new onchain deposit operation to track
    #[instrument(skip(self))]
    pub async fn track_onchain_deposit(
        &self,
        operation_id: OperationId,
        federation_id: FederationId,
        correlation_id: Option<String>,
    ) -> Result<()> {
        let operation = PaymentOperation {
            operation_id,
            federation_id,
            payment_type: PaymentType::OnchainDeposit,
            amount_msat: None, // Will be determined when deposit is confirmed
            created_at: Utc::now(),
            metadata: None,
            correlation_id,
            claim_attempted: false,
            ecash_claimed: false,
        };

        self.add_operation(operation).await
    }

    /// Add a new onchain withdrawal operation to track
    #[instrument(skip(self))]
    pub async fn track_onchain_withdraw(
        &self,
        operation_id: OperationId,
        federation_id: FederationId,
        amount_sat: u64,
    ) -> Result<()> {
        let operation = PaymentOperation {
            operation_id,
            federation_id,
            payment_type: PaymentType::OnchainWithdraw,
            amount_msat: Some(Amount::from_sats(amount_sat)),
            created_at: Utc::now(),
            metadata: None,
            correlation_id: None,
            claim_attempted: false,
            ecash_claimed: false,
        };

        self.add_operation(operation).await
    }

    /// Add an operation to track
    async fn add_operation(&self, operation: PaymentOperation) -> Result<()> {
        let operation_id = operation.operation_id;
        let federation_id = operation.federation_id;
        let payment_type = operation.payment_type.clone();

        // Check federation limit
        {
            let operations = self.active_operations.read().await;
            let federation_count = operations
                .values()
                .filter(|op| op.federation_id == federation_id)
                .count();

            if federation_count >= self.config.max_operations_per_federation {
                return Err(anyhow!(
                    "Federation {} has reached maximum operations limit ({})",
                    federation_id,
                    self.config.max_operations_per_federation
                ));
            }
        }

        // Add to active operations
        {
            let mut operations = self.active_operations.write().await;
            operations.insert(operation_id, operation);
        }

        info!(
            operation_id = ?operation_id,
            federation_id = %federation_id,
            payment_type = ?payment_type,
            "Added payment operation to lifecycle tracking"
        );

        Ok(())
    }

    /// Recover pending operations from all federations on startup
    #[instrument(skip(self))]
    async fn recover_pending_operations(&self) -> Result<()> {
        info!("Recovering pending operations from all federations");

        let clients = self.multimint.clients.lock().await.clone();
        let mut total_recovered = 0;

        for (federation_id, client) in clients.iter() {
            match self
                .recover_federation_operations(client, *federation_id)
                .await
            {
                Ok(count) => {
                    if count > 0 {
                        info!(
                            federation_id = %federation_id,
                            recovered_operations = count,
                            "Recovered pending operations from federation"
                        );
                        total_recovered += count;
                    }
                }
                Err(e) => {
                    error!(
                        federation_id = %federation_id,
                        error = ?e,
                        "Failed to recover operations from federation"
                    );
                }
            }
        }

        info!(
            total_recovered_operations = total_recovered,
            "Completed recovery of pending operations"
        );

        Ok(())
    }

    /// Recover pending operations from a specific federation
    async fn recover_federation_operations(
        &self,
        client: &ClientHandleArc,
        federation_id: FederationId,
    ) -> Result<usize> {
        let operations = client
            .operation_log()
            .paginate_operations_rev(100, None) // Get last 100 operations
            .await;

        let mut recovered_count = 0;

        for (key, value) in operations {
            let operation_id = key.operation_id;
            let operation_kind = value.operation_module_kind();
            let created_at = chrono::DateTime::<Utc>::from(key.creation_time);

            // Check if operation is still within timeout window
            let age = Utc::now().signed_duration_since(created_at);
            if age.to_std().unwrap_or_default() > self.config.operation_timeout {
                debug!(
                    operation_id = ?operation_id,
                    age_hours = age.num_hours(),
                    "Skipping expired operation during recovery"
                );
                continue;
            }

            // Determine payment type based on operation kind and metadata
            // Also check the operation type string in metadata
            let meta = value.meta::<serde_json::Value>();
            let variant = meta.get("variant").and_then(|v| v.as_str());
            let op_type = meta.get("type").and_then(|v| v.as_str());

            let payment_type = match operation_kind.as_ref() {
                "ln" => {
                    // Check multiple metadata fields to determine operation type
                    if variant == Some("receive")
                        || op_type == Some("ln_receive")
                        || meta.get("invoice").is_some()
                    {
                        Some(PaymentType::LightningReceive)
                    } else if variant == Some("pay")
                        || op_type == Some("ln_pay")
                        || meta.get("payment_hash").is_some()
                    {
                        Some(PaymentType::LightningPay)
                    } else {
                        // Log unrecognized Lightning operation for debugging
                        debug!(
                            operation_id = ?operation_id,
                            metadata = ?meta,
                            "Unrecognized Lightning operation type during recovery"
                        );
                        None
                    }
                }
                "wallet" => {
                    // Check multiple metadata fields to determine operation type
                    if variant == Some("deposit")
                        || op_type == Some("deposit")
                        || meta.get("address").is_some()
                    {
                        Some(PaymentType::OnchainDeposit)
                    } else if variant == Some("withdraw")
                        || op_type == Some("withdraw")
                        || meta.get("recipient").is_some()
                    {
                        Some(PaymentType::OnchainWithdraw)
                    } else {
                        // Log unrecognized wallet operation for debugging
                        debug!(
                            operation_id = ?operation_id,
                            metadata = ?meta,
                            "Unrecognized wallet operation type during recovery"
                        );
                        None
                    }
                }
                _ => None,
            };

            // Skip operations that have completed outcomes ONLY if they're truly complete
            // For receive operations, we still want to track them if ecash hasn't been
            // claimed
            if let Some(_outcome) = value.outcome::<serde_json::Value>() {
                // Check if this is a successful outcome that means ecash was already claimed
                let is_truly_complete = match &payment_type {
                    Some(PaymentType::LightningReceive) | Some(PaymentType::OnchainDeposit) => {
                        // For receive operations, check if the outcome indicates successful
                        // claiming - any non-null outcome means the operation is complete
                        true
                    }
                    _ => true, // For other operations, having an outcome means they're complete
                };

                if is_truly_complete {
                    debug!(
                        operation_id = ?operation_id,
                        payment_type = ?payment_type,
                        "Skipping completed operation during recovery"
                    );
                    continue;
                }
            }

            if let Some(payment_type) = payment_type {
                let operation = PaymentOperation {
                    operation_id,
                    federation_id,
                    payment_type: payment_type.clone(),
                    amount_msat: value
                        .meta::<serde_json::Value>()
                        .get("amount_msat")
                        .and_then(|v| v.as_u64())
                        .map(Amount::from_msats),
                    created_at,
                    metadata: Some(value.meta()),
                    correlation_id: None,
                    claim_attempted: false,
                    ecash_claimed: false,
                };

                // Add to active operations
                let mut operations = self.active_operations.write().await;
                operations.insert(operation_id, operation);
                recovered_count += 1;

                debug!(
                    operation_id = ?operation_id,
                    payment_type = ?payment_type,
                    "Recovered pending operation"
                );
            }
        }

        Ok(recovered_count)
    }

    /// Process all active operations
    async fn process_all_operations(
        event_bus: &Arc<EventBus>,
        multimint: &Arc<MultiMint>,
        active_operations: &Arc<RwLock<HashMap<OperationId, PaymentOperation>>>,
        operation_timeout: Duration,
        claim_timeout: Duration,
    ) -> Result<()> {
        let operations_to_process = {
            let operations = active_operations.read().await;
            operations.clone()
        };

        if operations_to_process.is_empty() {
            return Ok(());
        }

        debug!(
            active_operations = operations_to_process.len(),
            "Processing active payment operations"
        );

        let now = Utc::now();
        let mut completed_operations = Vec::new();
        let mut timed_out_operations = Vec::new();

        // Group operations by federation for efficient processing
        let mut operations_by_federation: HashMap<FederationId, Vec<PaymentOperation>> =
            HashMap::new();

        for (operation_id, operation) in operations_to_process {
            // Check timeout
            if now
                .signed_duration_since(operation.created_at)
                .to_std()
                .unwrap_or_default()
                > operation_timeout
            {
                timed_out_operations.push(operation_id);
                continue;
            }

            operations_by_federation
                .entry(operation.federation_id)
                .or_default()
                .push(operation);
        }

        // Process operations by federation
        for (federation_id, federation_operations) in operations_by_federation {
            let client = match multimint.get(&federation_id).await {
                Some(client) => client,
                None => {
                    warn!(federation_id = %federation_id, "Federation client not available");
                    continue;
                }
            };

            for operation in federation_operations {
                match operation.payment_type {
                    PaymentType::LightningReceive => {
                        let mut operation_mut = operation.clone();
                        if let Err(e) = Self::process_lightning_receive(
                            &client,
                            &mut operation_mut,
                            event_bus,
                            claim_timeout,
                        )
                        .await
                        {
                            error!(
                                operation_id = ?operation_mut.operation_id,
                                error = ?e,
                                "Failed to process Lightning receive"
                            );
                        }

                        // Update the operation in active_operations with the new state
                        let mut operations = active_operations.write().await;
                        if let Some(active_op) = operations.get_mut(&operation_mut.operation_id) {
                            active_op.claim_attempted = operation_mut.claim_attempted;
                            active_op.ecash_claimed = operation_mut.ecash_claimed;
                        }
                        drop(operations);

                        if operation_mut.ecash_claimed {
                            completed_operations.push(operation_mut.operation_id);
                        }
                    }
                    PaymentType::LightningPay => {
                        if let Err(e) =
                            Self::process_lightning_pay(&client, &operation, event_bus).await
                        {
                            error!(
                                operation_id = ?operation.operation_id,
                                error = ?e,
                                "Failed to process Lightning pay"
                            );
                        }
                    }
                    PaymentType::OnchainDeposit => {
                        let mut operation_mut = operation.clone();
                        if let Err(e) = Self::process_onchain_deposit(
                            &client,
                            &mut operation_mut,
                            event_bus,
                            claim_timeout,
                        )
                        .await
                        {
                            error!(
                                operation_id = ?operation_mut.operation_id,
                                error = ?e,
                                "Failed to process onchain deposit"
                            );
                        }

                        // Update the operation in active_operations with the new state
                        let mut operations = active_operations.write().await;
                        if let Some(active_op) = operations.get_mut(&operation_mut.operation_id) {
                            active_op.claim_attempted = operation_mut.claim_attempted;
                            active_op.ecash_claimed = operation_mut.ecash_claimed;
                            active_op.amount_msat = operation_mut.amount_msat;
                        }
                        drop(operations);

                        if operation_mut.ecash_claimed {
                            completed_operations.push(operation_mut.operation_id);
                        }
                    }
                    PaymentType::OnchainWithdraw => {
                        if let Err(e) =
                            Self::process_onchain_withdraw(&client, &operation, event_bus).await
                        {
                            error!(
                                operation_id = ?operation.operation_id,
                                error = ?e,
                                "Failed to process onchain withdrawal"
                            );
                        }
                    }
                }
            }
        }

        // Remove completed and timed out operations
        if !completed_operations.is_empty() || !timed_out_operations.is_empty() {
            let mut operations = active_operations.write().await;

            for operation_id in &completed_operations {
                operations.remove(operation_id);
                info!(operation_id = ?operation_id, "Payment operation completed successfully");
            }

            for operation_id in &timed_out_operations {
                operations.remove(operation_id);
                warn!(operation_id = ?operation_id, "Payment operation timed out");
            }
        }

        Ok(())
    }

    /// Process a Lightning receive operation - most importantly, claim the
    /// ecash!
    async fn process_lightning_receive(
        client: &ClientHandleArc,
        operation: &mut PaymentOperation,
        event_bus: &Arc<EventBus>,
        _claim_timeout: Duration,
    ) -> Result<()> {
        let lightning_module = client.get_first_module::<LightningClientModule>()?;

        // Subscribe to updates for this operation
        let mut updates = lightning_module
            .subscribe_ln_receive(operation.operation_id)
            .await?
            .into_stream();

        // Process all available states to get to the current state
        let mut last_state = None;
        let timeout_future = tokio::time::sleep(Duration::from_millis(100));
        tokio::pin!(timeout_future);

        loop {
            tokio::select! {
                update = updates.next() => {
                    match update {
                        Some(state) => {
                            last_state = Some(state);
                            continue;
                        }
                        None => break,
                    }
                }
                _ = &mut timeout_future => {
                    break;
                }
            }
        }

        let current_state = match last_state {
            Some(state) => state,
            None => {
                debug!(
                    operation_id = ?operation.operation_id,
                    "No state updates available for Lightning receive"
                );
                return Ok(());
            }
        };

        match current_state {
            LnReceiveState::Claimed => {
                // CRITICAL INSIGHT: When LnReceiveState::Claimed is reached,
                // the ecash notes have ALREADY been issued to the wallet!
                // The "Claimed" state means the full payment flow is complete:
                // 1. Gateway received the Lightning payment
                // 2. Federation issued ecash notes
                // 3. Notes are now in the client's wallet

                if !operation.ecash_claimed {
                    info!(
                        operation_id = ?operation.operation_id,
                        amount_msat = ?operation.amount_msat,
                        "Lightning payment successfully claimed - ecash notes received in wallet!"
                    );

                    // Mark as successfully claimed
                    operation.ecash_claimed = true;
                    operation.claim_attempted = true;

                    // Publish success event
                    if let Some(amount) = operation.amount_msat {
                        let event = FmcdEvent::InvoicePaid {
                            operation_id: format!("{:?}", operation.operation_id),
                            federation_id: operation.federation_id.to_string(),
                            amount_msat: amount.msats,
                            correlation_id: operation.correlation_id.clone(),
                            timestamp: Utc::now(),
                        };
                        let _ = event_bus.publish(event).await;
                    }

                    // Verify balance was actually updated
                    let balance = client.get_balance_err().await?;
                    info!(
                        operation_id = ?operation.operation_id,
                        balance_msat = balance.msats,
                        "Wallet balance after Lightning receive"
                    );
                }
            }
            LnReceiveState::Canceled { reason } => {
                warn!(
                    operation_id = ?operation.operation_id,
                    reason = %reason,
                    "Lightning receive canceled"
                );
                operation.claim_attempted = true;
            }
            LnReceiveState::WaitingForPayment { .. } => {
                debug!(
                    operation_id = ?operation.operation_id,
                    "Lightning invoice waiting for payment"
                );
            }
            _ => {
                debug!(
                    operation_id = ?operation.operation_id,
                    state = ?current_state,
                    "Lightning receive in intermediate state"
                );
            }
        }

        Ok(())
    }

    /// Process a Lightning pay operation
    async fn process_lightning_pay(
        client: &ClientHandleArc,
        operation: &PaymentOperation,
        event_bus: &Arc<EventBus>,
    ) -> Result<()> {
        let lightning_module = client.get_first_module::<LightningClientModule>()?;

        // Check current state
        let mut updates = lightning_module
            .subscribe_ln_pay(operation.operation_id)
            .await?
            .into_stream();

        let current_state = match updates.next().await {
            Some(state) => state,
            None => return Ok(()),
        };

        match current_state {
            LnPayState::Success { preimage } => {
                info!(
                    operation_id = ?operation.operation_id,
                    "Lightning payment succeeded"
                );

                // Publish success event
                if let Some(amount) = operation.amount_msat {
                    let event = FmcdEvent::PaymentSucceeded {
                        operation_id: format!("{:?}", operation.operation_id),
                        federation_id: operation.federation_id.to_string(),
                        amount_msat: amount.msats,
                        preimage,
                        timestamp: Utc::now(),
                    };
                    let _ = event_bus.publish(event).await;
                }
            }
            LnPayState::Refunded { gateway_error } => {
                warn!(
                    operation_id = ?operation.operation_id,
                    error = %gateway_error,
                    "Lightning payment refunded"
                );

                // Publish refund event
                let event = FmcdEvent::PaymentRefunded {
                    operation_id: format!("{:?}", operation.operation_id),
                    federation_id: operation.federation_id.to_string(),
                    reason: gateway_error.to_string(),
                    timestamp: Utc::now(),
                };
                let _ = event_bus.publish(event).await;
            }
            _ => {
                // Still in progress
                debug!(
                    operation_id = ?operation.operation_id,
                    state = ?current_state,
                    "Lightning payment still in progress"
                );
            }
        }

        Ok(())
    }

    /// Process an onchain deposit operation - claim ecash after confirmation!
    async fn process_onchain_deposit(
        client: &ClientHandleArc,
        operation: &mut PaymentOperation,
        event_bus: &Arc<EventBus>,
        _claim_timeout: Duration,
    ) -> Result<()> {
        let wallet_module = client.get_first_module::<WalletClientModule>()?;

        // Subscribe to updates for this operation
        let mut updates = wallet_module
            .subscribe_deposit(operation.operation_id)
            .await?
            .into_stream();

        // Process all available states to get to the current state
        let mut last_state = None;
        let timeout_future = tokio::time::sleep(Duration::from_millis(100));
        tokio::pin!(timeout_future);

        loop {
            tokio::select! {
                update = updates.next() => {
                    match update {
                        Some(state) => {
                            last_state = Some(state);
                            continue;
                        }
                        None => break,
                    }
                }
                _ = &mut timeout_future => {
                    break;
                }
            }
        }

        let current_state = match last_state {
            Some(state) => state,
            None => {
                debug!(
                    operation_id = ?operation.operation_id,
                    "No state updates available for onchain deposit"
                );
                return Ok(());
            }
        };

        match current_state {
            DepositStateV2::Claimed {
                btc_deposited,
                btc_out_point,
            } => {
                // CRITICAL INSIGHT: When DepositStateV2::Claimed is reached,
                // the ecash notes have ALREADY been issued to the wallet!
                // The "Claimed" state means the full deposit flow is complete:
                // 1. Bitcoin transaction confirmed on-chain
                // 2. Federation verified the deposit
                // 3. Ecash notes issued and now in the client's wallet

                if !operation.ecash_claimed {
                    info!(
                        operation_id = ?operation.operation_id,
                        amount_sat = btc_deposited.to_sat(),
                        txid = %btc_out_point.txid,
                        "Onchain deposit successfully claimed - ecash notes received in wallet!"
                    );

                    // Mark as successfully claimed
                    operation.ecash_claimed = true;
                    operation.claim_attempted = true;

                    // Update the operation amount now that we know it
                    operation.amount_msat = Some(Amount::from_sats(btc_deposited.to_sat()));

                    // Publish success event
                    let event = FmcdEvent::DepositClaimed {
                        operation_id: format!("{:?}", operation.operation_id),
                        federation_id: operation.federation_id.to_string(),
                        amount_sat: btc_deposited.to_sat(),
                        txid: btc_out_point.txid.to_string(),
                        correlation_id: operation.correlation_id.clone(),
                        timestamp: Utc::now(),
                    };
                    let _ = event_bus.publish(event).await;

                    // Verify balance was actually updated
                    let balance = client.get_balance_err().await?;
                    info!(
                        operation_id = ?operation.operation_id,
                        balance_msat = balance.msats,
                        "Wallet balance after onchain deposit"
                    );
                }
            }
            DepositStateV2::Confirmed {
                btc_deposited,
                btc_out_point,
            } => {
                // Deposit is confirmed but not yet claimed
                // The federation is still processing it
                info!(
                    operation_id = ?operation.operation_id,
                    amount_sat = btc_deposited.to_sat(),
                    txid = %btc_out_point.txid,
                    "Onchain deposit confirmed, waiting for federation to issue ecash"
                );

                // Update amount now that we know it
                operation.amount_msat = Some(Amount::from_sats(btc_deposited.to_sat()));
            }
            DepositStateV2::Failed(reason) => {
                error!(
                    operation_id = ?operation.operation_id,
                    reason = %reason,
                    "Onchain deposit failed"
                );
                operation.claim_attempted = true;
            }
            DepositStateV2::WaitingForTransaction => {
                debug!(
                    operation_id = ?operation.operation_id,
                    "Waiting for onchain transaction"
                );
            }
            DepositStateV2::WaitingForConfirmation {
                btc_deposited,
                btc_out_point,
            } => {
                debug!(
                    operation_id = ?operation.operation_id,
                    amount_sat = btc_deposited.to_sat(),
                    txid = %btc_out_point.txid,
                    "Waiting for onchain confirmations"
                );
            }
        }

        Ok(())
    }

    /// Process an onchain withdrawal operation
    async fn process_onchain_withdraw(
        client: &ClientHandleArc,
        operation: &PaymentOperation,
        event_bus: &Arc<EventBus>,
    ) -> Result<()> {
        let wallet_module = client.get_first_module::<WalletClientModule>()?;

        // Check current state
        let mut updates = wallet_module
            .subscribe_withdraw_updates(operation.operation_id)
            .await?
            .into_stream();

        let current_state = match updates.next().await {
            Some(state) => state,
            None => return Ok(()),
        };

        match current_state {
            WithdrawState::Succeeded(txid) => {
                info!(
                    operation_id = ?operation.operation_id,
                    txid = %txid,
                    "Onchain withdrawal succeeded"
                );

                // Publish success event
                if let Some(amount) = operation.amount_msat {
                    let event = FmcdEvent::WithdrawalSucceeded {
                        operation_id: format!("{:?}", operation.operation_id),
                        federation_id: operation.federation_id.to_string(),
                        amount_sat: amount.sats_round_down(),
                        txid: txid.to_string(),
                        timestamp: Utc::now(),
                    };
                    let _ = event_bus.publish(event).await;
                }
            }
            WithdrawState::Failed(reason) => {
                error!(
                    operation_id = ?operation.operation_id,
                    reason = %reason,
                    "Onchain withdrawal failed"
                );

                // Publish failure event
                let event = FmcdEvent::WithdrawalFailed {
                    operation_id: format!("{:?}", operation.operation_id),
                    federation_id: operation.federation_id.to_string(),
                    reason,
                    correlation_id: None,
                    timestamp: Utc::now(),
                };
                let _ = event_bus.publish(event).await;
            }
            _ => {
                // Still in progress
                debug!(
                    operation_id = ?operation.operation_id,
                    state = ?current_state,
                    "Onchain withdrawal still in progress"
                );
            }
        }

        Ok(())
    }

    /// Get statistics about active operations
    pub async fn get_stats(&self) -> PaymentLifecycleStats {
        let operations = self.active_operations.read().await;
        let mut by_type: HashMap<PaymentType, usize> = HashMap::new();
        let mut by_federation: HashMap<FederationId, usize> = HashMap::new();

        for operation in operations.values() {
            *by_type.entry(operation.payment_type.clone()).or_insert(0) += 1;
            *by_federation.entry(operation.federation_id).or_insert(0) += 1;
        }

        PaymentLifecycleStats {
            total_active_operations: operations.len(),
            operations_by_type: by_type,
            operations_by_federation: by_federation,
        }
    }
}

/// Statistics about the payment lifecycle manager
#[derive(Debug, Clone, Serialize)]
pub struct PaymentLifecycleStats {
    pub total_active_operations: usize,
    pub operations_by_type: HashMap<PaymentType, usize>,
    pub operations_by_federation: HashMap<FederationId, usize>,
}
