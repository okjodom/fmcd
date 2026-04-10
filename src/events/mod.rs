use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub mod handlers;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FmcdEvent {
    // Payment events
    PaymentInitiated {
        payment_id: String,
        federation_id: String,
        amount_msat: u64,
        invoice: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    PaymentSucceeded {
        operation_id: String,
        federation_id: String,
        amount_msat: u64,
        fee_msat: Option<u64>,
        preimage: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    PaymentRefunded {
        operation_id: String,
        federation_id: String,
        reason: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    PaymentFailed {
        payment_id: String,
        federation_id: String,
        reason: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },

    // Invoice events
    InvoiceCreated {
        invoice_id: String,
        federation_id: String,
        amount_msat: u64,
        invoice: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    InvoicePaid {
        operation_id: String,
        federation_id: String,
        amount_msat: u64,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    InvoiceExpired {
        invoice_id: String,
        federation_id: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },

    // Federation events
    FederationConnected {
        federation_id: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    FederationDisconnected {
        federation_id: String,
        reason: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    FederationBalanceUpdated {
        federation_id: String,
        balance_msat: u64,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },

    // Onchain events
    DepositAddressGenerated {
        operation_id: String,
        federation_id: String,
        address: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    DepositDetected {
        operation_id: String,
        federation_id: String,
        address: String,
        amount_sat: u64,
        txid: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    DepositClaimed {
        operation_id: String,
        federation_id: String,
        amount_sat: u64,
        txid: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    WithdrawalInitiated {
        operation_id: String,
        federation_id: String,
        address: String,
        amount_sat: u64,
        fee_sat: u64,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    WithdrawalSucceeded {
        operation_id: String,
        federation_id: String,
        amount_sat: u64,
        txid: String,
        timestamp: DateTime<Utc>,
    },
    WithdrawalFailed {
        operation_id: String,
        federation_id: String,
        reason: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },

    // Gateway events
    GatewaySelected {
        gateway_id: String,
        federation_id: String,
        payment_id: Option<String>,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    GatewayUnavailable {
        gateway_id: String,
        federation_id: String,
        reason: String,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },

    // Database events
    DatabaseQueryExecuted {
        operation: String,
        key_prefix: String,
        duration_ms: u128,
        success: bool,
        error_message: Option<String>,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },

    // Authentication events
    AuthenticationAttempt {
        user_id: Option<String>,
        ip_address: String,
        endpoint: String,
        success: bool,
        reason: Option<String>,
        correlation_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
}

impl FmcdEvent {
    /// Generate a unique event ID
    pub fn event_id(&self) -> String {
        Uuid::new_v4().to_string()
    }

    /// Get the event timestamp
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            FmcdEvent::PaymentInitiated { timestamp, .. } => *timestamp,
            FmcdEvent::PaymentSucceeded { timestamp, .. } => *timestamp,
            FmcdEvent::PaymentRefunded { timestamp, .. } => *timestamp,
            FmcdEvent::PaymentFailed { timestamp, .. } => *timestamp,
            FmcdEvent::InvoiceCreated { timestamp, .. } => *timestamp,
            FmcdEvent::InvoicePaid { timestamp, .. } => *timestamp,
            FmcdEvent::InvoiceExpired { timestamp, .. } => *timestamp,
            FmcdEvent::FederationConnected { timestamp, .. } => *timestamp,
            FmcdEvent::FederationDisconnected { timestamp, .. } => *timestamp,
            FmcdEvent::FederationBalanceUpdated { timestamp, .. } => *timestamp,
            FmcdEvent::DepositAddressGenerated { timestamp, .. } => *timestamp,
            FmcdEvent::DepositDetected { timestamp, .. } => *timestamp,
            FmcdEvent::DepositClaimed { timestamp, .. } => *timestamp,
            FmcdEvent::WithdrawalInitiated { timestamp, .. } => *timestamp,
            FmcdEvent::WithdrawalSucceeded { timestamp, .. } => *timestamp,
            FmcdEvent::WithdrawalFailed { timestamp, .. } => *timestamp,
            FmcdEvent::GatewaySelected { timestamp, .. } => *timestamp,
            FmcdEvent::GatewayUnavailable { timestamp, .. } => *timestamp,
            FmcdEvent::DatabaseQueryExecuted { timestamp, .. } => *timestamp,
            FmcdEvent::AuthenticationAttempt { timestamp, .. } => *timestamp,
        }
    }

    /// Get the correlation ID if present
    pub fn correlation_id(&self) -> Option<&String> {
        match self {
            FmcdEvent::PaymentInitiated { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::PaymentSucceeded { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::PaymentRefunded { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::PaymentFailed { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::InvoiceCreated { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::InvoicePaid { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::InvoiceExpired { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::FederationConnected { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::FederationDisconnected { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::FederationBalanceUpdated { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::DepositAddressGenerated { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::DepositDetected { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::DepositClaimed { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::WithdrawalInitiated { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::WithdrawalSucceeded { .. } => None,
            FmcdEvent::WithdrawalFailed { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::GatewaySelected { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::GatewayUnavailable { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::DatabaseQueryExecuted { correlation_id, .. } => correlation_id.as_ref(),
            FmcdEvent::AuthenticationAttempt { correlation_id, .. } => correlation_id.as_ref(),
        }
    }

    /// Get the event type as a string
    pub fn event_type(&self) -> &'static str {
        match self {
            FmcdEvent::PaymentInitiated { .. } => "payment_initiated",
            FmcdEvent::PaymentSucceeded { .. } => "payment_succeeded",
            FmcdEvent::PaymentRefunded { .. } => "payment_refunded",
            FmcdEvent::PaymentFailed { .. } => "payment_failed",
            FmcdEvent::InvoiceCreated { .. } => "invoice_created",
            FmcdEvent::InvoicePaid { .. } => "invoice_paid",
            FmcdEvent::InvoiceExpired { .. } => "invoice_expired",
            FmcdEvent::FederationConnected { .. } => "federation_connected",
            FmcdEvent::FederationDisconnected { .. } => "federation_disconnected",
            FmcdEvent::FederationBalanceUpdated { .. } => "federation_balance_updated",
            FmcdEvent::DepositAddressGenerated { .. } => "deposit_address_generated",
            FmcdEvent::DepositDetected { .. } => "deposit_detected",
            FmcdEvent::DepositClaimed { .. } => "deposit_claimed",
            FmcdEvent::WithdrawalInitiated { .. } => "withdrawal_initiated",
            FmcdEvent::WithdrawalSucceeded { .. } => "withdrawal_succeeded",
            FmcdEvent::WithdrawalFailed { .. } => "withdrawal_failed",
            FmcdEvent::GatewaySelected { .. } => "gateway_selected",
            FmcdEvent::GatewayUnavailable { .. } => "gateway_unavailable",
            FmcdEvent::DatabaseQueryExecuted { .. } => "database_query_executed",
            FmcdEvent::AuthenticationAttempt { .. } => "authentication_attempt",
        }
    }
}

/// Trait for handling events asynchronously
#[async_trait]
pub trait EventHandler: Send + Sync {
    /// Handle an event
    async fn handle(&self, event: FmcdEvent) -> anyhow::Result<()>;

    /// Get the name of this handler for identification
    fn name(&self) -> &str;

    /// Whether this handler should block event publishing on failure
    fn is_critical(&self) -> bool {
        false
    }
}

/// Event bus for distributing events to multiple handlers
pub struct EventBus {
    sender: broadcast::Sender<FmcdEvent>,
    handlers: Arc<RwLock<Vec<Arc<dyn EventHandler>>>>,
    max_capacity: usize,
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("max_capacity", &self.max_capacity)
            .field(
                "handlers_count",
                &self.handlers.try_read().map(|h| h.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl EventBus {
    /// Create a new event bus with the specified capacity
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            handlers: Arc::new(RwLock::new(Vec::new())),
            max_capacity: capacity,
        }
    }

    /// Register an event handler
    pub async fn register_handler(&self, handler: Arc<dyn EventHandler>) {
        let mut handlers = self.handlers.write().await;
        let handler_name = handler.name().to_string();
        handlers.push(handler);
        info!(
            handler_name = %handler_name,
            total_handlers = handlers.len(),
            "Event handler registered successfully"
        );
    }

    /// Publish an event to all registered handlers
    pub async fn publish(&self, event: FmcdEvent) -> anyhow::Result<()> {
        let event_id = event.event_id();
        let event_type = event.event_type();
        let correlation_id = event.correlation_id().cloned();
        let timestamp = event.timestamp();

        debug!(
            event_id = %event_id,
            event_type = %event_type,
            correlation_id = ?correlation_id,
            timestamp = %timestamp,
            "Publishing event"
        );

        // Send to broadcast channel for real-time subscribers (non-blocking)
        match self.sender.send(event.clone()) {
            Ok(subscriber_count) => {
                debug!(
                    event_id = %event_id,
                    event_type = %event_type,
                    subscriber_count = subscriber_count,
                    "Event broadcast to subscribers"
                );
            }
            Err(broadcast::error::SendError(_)) => {
                // No active receivers, this is not an error
                debug!(
                    event_id = %event_id,
                    event_type = %event_type,
                    "Event published but no active subscribers"
                );
            }
        }

        // Process registered handlers
        let handlers = self.handlers.read().await;
        if handlers.is_empty() {
            warn!(
                event_id = %event_id,
                event_type = %event_type,
                "No event handlers registered"
            );
            return Ok(());
        }

        // Track critical handlers for potential blocking
        let mut critical_handler_futures = Vec::new();
        let mut non_critical_handlers = 0;

        for handler in handlers.iter() {
            let handler_clone = handler.clone();
            let event_clone = event.clone();
            let event_id_clone = event_id.clone();

            if handler.is_critical() {
                // Critical handlers: await them to ensure they complete
                critical_handler_futures.push(async move {
                    let handler_name = handler_clone.name();
                    match handler_clone.handle(event_clone).await {
                        Ok(()) => {
                            debug!(
                                event_id = %event_id_clone,
                                handler_name = %handler_name,
                                "Critical event handler completed successfully"
                            );
                        }
                        Err(e) => {
                            error!(
                                event_id = %event_id_clone,
                                handler_name = %handler_name,
                                error = ?e,
                                "Critical event handler failed"
                            );
                        }
                    }
                });
            } else {
                // Non-critical handlers: spawn them in the background
                non_critical_handlers += 1;
                tokio::spawn(async move {
                    let handler_name = handler_clone.name();
                    match handler_clone.handle(event_clone).await {
                        Ok(()) => {
                            debug!(
                                event_id = %event_id_clone,
                                handler_name = %handler_name,
                                "Event handler completed successfully"
                            );
                        }
                        Err(e) => {
                            error!(
                                event_id = %event_id_clone,
                                handler_name = %handler_name,
                                error = ?e,
                                "Event handler failed"
                            );
                        }
                    }
                });
            }
        }

        // Wait for critical handlers to complete
        for future in critical_handler_futures {
            future.await;
        }

        debug!(
            event_id = %event_id,
            event_type = %event_type,
            total_handlers = handlers.len(),
            critical_handlers = handlers.iter().filter(|h| h.is_critical()).count(),
            non_critical_handlers = non_critical_handlers,
            "Event processing initiated"
        );

        Ok(())
    }

    /// Subscribe to the event stream for real-time event processing
    pub fn subscribe(&self) -> broadcast::Receiver<FmcdEvent> {
        self.sender.subscribe()
    }

    /// Get the current number of registered handlers
    pub async fn handler_count(&self) -> usize {
        self.handlers.read().await.len()
    }

    /// Get statistics about the event bus
    pub async fn stats(&self) -> EventBusStats {
        let handlers = self.handlers.read().await;
        EventBusStats {
            capacity: self.max_capacity,
            handler_count: handlers.len(),
            critical_handler_count: handlers.iter().filter(|h| h.is_critical()).count(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EventBusStats {
    pub capacity: usize,
    pub handler_count: usize,
    pub critical_handler_count: usize,
}

#[cfg(test)]
mod tests;
