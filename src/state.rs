use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use fedimint_client::ClientHandleArc;
use fedimint_core::config::{FederationId, FederationIdPrefix};

use crate::core::multimint::MultiMint;
use crate::core::services::{BalanceMonitor, DepositMonitor, PaymentLifecycleManager};
use crate::core::FmcdCore;
use crate::error::AppError;
use crate::events::EventBus;
use crate::webhooks::WebhookConfig;

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
#[derive(Clone)]
pub struct AppState {
    pub core: Arc<FmcdCore>,
}

impl AppState {
    /// Create AppState with FmcdCore (preferred method)
    pub async fn new_with_core(core: FmcdCore) -> Result<Self> {
        let core = Arc::new(core);
        Ok(Self { core })
    }

    pub async fn new(fm_db_path: PathBuf) -> Result<Self> {
        Self::new_with_config(fm_db_path, WebhookConfig::default()).await
    }

    pub async fn new_with_config(
        fm_db_path: PathBuf,
        webhook_config: WebhookConfig,
    ) -> Result<Self> {
        let core = FmcdCore::new_with_config(fm_db_path, webhook_config).await?;
        Self::new_with_core(core).await
    }

    /// Initialize AppState with custom event bus configuration and webhook
    /// config
    pub async fn new_with_event_bus_config(
        fm_db_path: PathBuf,
        _event_bus_capacity: usize,
        _enable_debug_logging: bool,
        webhook_config: WebhookConfig,
    ) -> Result<Self> {
        // For now, use the default FmcdCore configuration
        // TODO: In a future phase, we could extend FmcdCore to support custom event bus
        // config
        let core = FmcdCore::new_with_config(fm_db_path, webhook_config).await?;
        Self::new_with_core(core).await
    }

    // Helper function to get a specific client from the state or default
    pub async fn get_client(
        &self,
        federation_id: FederationId,
    ) -> Result<ClientHandleArc, AppError> {
        self.core.get_client(federation_id).await
    }

    pub async fn get_client_by_prefix(
        &self,
        federation_id_prefix: &FederationIdPrefix,
    ) -> Result<ClientHandleArc, AppError> {
        self.core.get_client_by_prefix(federation_id_prefix).await
    }

    pub fn uptime(&self) -> std::time::Duration {
        self.core.uptime()
    }

    /// Start the monitoring services (deposit, balance, and payment lifecycle
    /// monitors)
    pub async fn start_monitoring_services(&self) -> Result<()> {
        self.core.start_monitoring_services().await
    }

    /// Stop the monitoring services (deposit and balance monitors)
    pub async fn stop_monitoring_services(&self) -> Result<()> {
        self.core.stop_monitoring_services().await
    }

    // Convenience accessors for backward compatibility
    pub fn multimint(&self) -> &MultiMint {
        &self.core.multimint
    }

    pub fn start_time(&self) -> Instant {
        self.core.start_time
    }

    pub fn event_bus(&self) -> &Arc<EventBus> {
        &self.core.event_bus
    }

    pub fn deposit_monitor(&self) -> &Option<Arc<DepositMonitor>> {
        &self.core.deposit_monitor
    }

    pub fn balance_monitor(&self) -> &Option<Arc<BalanceMonitor>> {
        &self.core.balance_monitor
    }

    pub fn payment_lifecycle_manager(&self) -> &Option<Arc<PaymentLifecycleManager>> {
        &self.core.payment_lifecycle_manager
    }
}
