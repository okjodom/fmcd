use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use fedimint_client::ClientHandleArc;
use fedimint_core::config::FederationId;
use serde::Serialize;
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio::time::interval;
use tracing::{debug, error, info, instrument, warn};

use crate::core::multimint::MultiMint;
use crate::events::{EventBus, FmcdEvent};

/// Configuration for the balance monitor service
#[derive(Debug, Clone)]
pub struct BalanceMonitorConfig {
    /// How often to check federation balances (default: 60 seconds)
    pub check_interval: Duration,
    /// Minimum balance change in msats to emit an event (default: 1000 msats =
    /// 1 sat)
    pub min_change_threshold_msats: u64,
}

impl Default for BalanceMonitorConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(60),
            min_change_threshold_msats: 1000, // 1 sat
        }
    }
}

/// Information about a federation's balance
#[derive(Debug, Clone, Serialize)]
pub struct FederationBalance {
    pub federation_id: FederationId,
    pub balance_msat: u64,
    pub last_updated: chrono::DateTime<Utc>,
}

/// Service that monitors federation balances for changes
#[derive(Debug)]
pub struct BalanceMonitor {
    event_bus: Arc<EventBus>,
    multimint: Arc<MultiMint>,
    config: BalanceMonitorConfig,
    last_balances: Arc<RwLock<HashMap<FederationId, u64>>>,
    shutdown_tx: Arc<Mutex<Option<broadcast::Sender<()>>>>,
}

impl BalanceMonitor {
    /// Create a new balance monitor
    pub fn new(
        event_bus: Arc<EventBus>,
        multimint: Arc<MultiMint>,
        config: BalanceMonitorConfig,
    ) -> Self {
        Self {
            event_bus,
            multimint,
            config,
            last_balances: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the balance monitoring service
    #[instrument(skip(self))]
    pub async fn start(&self) -> Result<()> {
        let (shutdown_tx, _) = broadcast::channel(1);
        {
            let mut tx_guard = self.shutdown_tx.lock().await;
            *tx_guard = Some(shutdown_tx.clone());
        }

        info!(
            check_interval_secs = self.config.check_interval.as_secs(),
            min_change_threshold_msats = self.config.min_change_threshold_msats,
            "Starting balance monitor service"
        );

        // Clone necessary data for the monitoring task
        let event_bus = self.event_bus.clone();
        let multimint = self.multimint.clone();
        let last_balances = self.last_balances.clone();
        let check_interval = self.config.check_interval;
        let min_change_threshold = self.config.min_change_threshold_msats;

        // Spawn the monitoring task
        tokio::spawn(async move {
            let mut shutdown_rx = shutdown_tx.subscribe();
            let mut check_timer = interval(check_interval);

            loop {
                tokio::select! {
                    _ = check_timer.tick() => {
                        if let Err(e) = Self::check_balances(
                            &event_bus,
                            &multimint,
                            &last_balances,
                            min_change_threshold,
                        ).await {
                            error!(error = ?e, "Error during balance checking");
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Balance monitor received shutdown signal");
                        break;
                    }
                }
            }

            info!("Balance monitor service stopped");
        });

        Ok(())
    }

    /// Stop the balance monitoring service
    pub async fn stop(&self) -> Result<()> {
        let tx_guard = self.shutdown_tx.lock().await;
        if let Some(shutdown_tx) = tx_guard.as_ref() {
            let _ = shutdown_tx.send(());
        }
        Ok(())
    }

    /// Get current balance information for all federations
    pub async fn get_balances(&self) -> Result<Vec<FederationBalance>> {
        let mut balances = Vec::new();
        let federation_ids = self.multimint.get_federation_ids().await;

        for federation_id in federation_ids {
            if let Some(client) = self.multimint.get(&federation_id).await {
                match client.get_balance_err().await {
                    Ok(balance) => {
                        balances.push(FederationBalance {
                            federation_id,
                            balance_msat: balance.msats,
                            last_updated: Utc::now(),
                        });
                    }
                    Err(error) => {
                        warn!(
                            federation_id = %federation_id,
                            error = ?error,
                            "Failed to get federation balance"
                        );
                    }
                }
            }
        }

        Ok(balances)
    }

    /// Get statistics about the balance monitor
    pub async fn get_stats(&self) -> BalanceMonitorStats {
        let last_balances = self.last_balances.read().await;
        let total_balance_msat = last_balances.values().sum();

        BalanceMonitorStats {
            monitored_federations: last_balances.len(),
            total_balance_msat,
            last_check_count: last_balances.len(),
        }
    }

    /// Internal method to check all federation balances
    #[instrument(skip(event_bus, multimint, last_balances))]
    async fn check_balances(
        event_bus: &Arc<EventBus>,
        multimint: &Arc<MultiMint>,
        last_balances: &Arc<RwLock<HashMap<FederationId, u64>>>,
        min_change_threshold: u64,
    ) -> Result<()> {
        let federation_ids = multimint.get_federation_ids().await;

        if federation_ids.is_empty() {
            debug!("No federations to monitor for balance changes");
            return Ok(());
        }

        debug!(
            federations_count = federation_ids.len(),
            "Checking federation balances for changes"
        );

        let mut balance_changes = Vec::new();

        for federation_id in federation_ids {
            if let Some(client) = multimint.get(&federation_id).await {
                match Self::check_federation_balance(
                    &client,
                    federation_id,
                    last_balances,
                    min_change_threshold,
                )
                .await
                {
                    Ok(Some(balance_change)) => {
                        balance_changes.push(balance_change);
                    }
                    Ok(None) => {
                        // No significant change
                        debug!(
                            federation_id = %federation_id,
                            "No significant balance change detected"
                        );
                    }
                    Err(e) => {
                        error!(
                            federation_id = %federation_id,
                            error = ?e,
                            "Error checking federation balance"
                        );
                    }
                }
            } else {
                warn!(
                    federation_id = %federation_id,
                    "Federation client not available for balance check"
                );
            }
        }

        // Emit events for balance changes
        for balance_change in balance_changes {
            let event = FmcdEvent::FederationBalanceUpdated {
                federation_id: balance_change.federation_id.to_string(),
                balance_msat: balance_change.new_balance_msat,
                correlation_id: None, // Balance monitoring is not request-driven
                timestamp: Utc::now(),
            };

            if let Err(e) = event_bus.publish(event).await {
                error!(
                    federation_id = %balance_change.federation_id,
                    error = ?e,
                    "Failed to publish balance updated event"
                );
            } else {
                info!(
                    federation_id = %balance_change.federation_id,
                    old_balance_msat = balance_change.old_balance_msat,
                    new_balance_msat = balance_change.new_balance_msat,
                    change_msat = balance_change.change_msat,
                    "Balance change detected and event published"
                );
            }
        }

        Ok(())
    }

    /// Check balance for a specific federation
    async fn check_federation_balance(
        client: &ClientHandleArc,
        federation_id: FederationId,
        last_balances: &Arc<RwLock<HashMap<FederationId, u64>>>,
        min_change_threshold: u64,
    ) -> Result<Option<BalanceChange>> {
        // Get current balance
        let current_balance_msat = client.get_balance_err().await?.msats;

        // Get last known balance
        let last_balance_msat = {
            let balances = last_balances.read().await;
            balances.get(&federation_id).copied()
        };

        let balance_change = match last_balance_msat {
            Some(last_balance) => {
                let change = if current_balance_msat > last_balance {
                    current_balance_msat - last_balance
                } else {
                    last_balance - current_balance_msat
                };

                if change >= min_change_threshold {
                    Some(BalanceChange {
                        federation_id,
                        old_balance_msat: last_balance,
                        new_balance_msat: current_balance_msat,
                        change_msat: change,
                    })
                } else {
                    None
                }
            }
            None => {
                // First time seeing this federation, always emit event
                Some(BalanceChange {
                    federation_id,
                    old_balance_msat: 0,
                    new_balance_msat: current_balance_msat,
                    change_msat: current_balance_msat,
                })
            }
        };

        // Update stored balance
        {
            let mut balances = last_balances.write().await;
            balances.insert(federation_id, current_balance_msat);
        }

        debug!(
            federation_id = %federation_id,
            current_balance_msat = current_balance_msat,
            last_balance_msat = last_balance_msat.unwrap_or(0),
            change_detected = balance_change.is_some(),
            "Federation balance checked"
        );

        Ok(balance_change)
    }
}

/// Represents a detected balance change
#[derive(Debug, Clone)]
struct BalanceChange {
    federation_id: FederationId,
    old_balance_msat: u64,
    new_balance_msat: u64,
    change_msat: u64,
}

/// Statistics about the balance monitor
#[derive(Debug, Clone, Serialize)]
pub struct BalanceMonitorStats {
    pub monitored_federations: usize,
    pub total_balance_msat: u64,
    pub last_check_count: usize,
}
