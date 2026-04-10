use async_trait::async_trait;
use tracing::{debug, error, info, warn};

use crate::events::{EventHandler, FmcdEvent};
use crate::observability::sanitization::{sanitize_invoice, sanitize_preimage};

/// Event handler that logs all events with appropriate levels and sanitization
pub struct LoggingEventHandler {
    include_debug_events: bool,
}

impl LoggingEventHandler {
    pub fn new(include_debug_events: bool) -> Self {
        Self {
            include_debug_events,
        }
    }
}

#[async_trait]
impl EventHandler for LoggingEventHandler {
    async fn handle(&self, event: FmcdEvent) -> anyhow::Result<()> {
        match event {
            FmcdEvent::PaymentInitiated {
                payment_id,
                federation_id,
                amount_msat,
                invoice,
                correlation_id,
                timestamp,
            } => {
                info!(
                    event_type = "payment_initiated",
                    payment_id = %payment_id,
                    federation_id = %federation_id,
                    amount_msat = amount_msat,
                    invoice = %sanitize_invoice(&invoice),
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Payment initiated"
                );
            }
            FmcdEvent::PaymentSucceeded {
                operation_id,
                federation_id,
                amount_msat,
                fee_msat,
                preimage,
                correlation_id,
                timestamp,
            } => {
                info!(
                    event_type = "payment_succeeded",
                    operation_id = %operation_id,
                    federation_id = %federation_id,
                    amount_msat = amount_msat,
                    fee_msat = ?fee_msat,
                    preimage = %sanitize_preimage(&preimage),
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Payment succeeded"
                );
            }
            FmcdEvent::PaymentFailed {
                payment_id,
                federation_id,
                reason,
                correlation_id,
                timestamp,
            } => {
                warn!(
                    event_type = "payment_failed",
                    payment_id = %payment_id,
                    federation_id = %federation_id,
                    reason = %reason,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Payment failed"
                );
            }
            FmcdEvent::InvoiceCreated {
                invoice_id,
                federation_id,
                amount_msat,
                invoice,
                correlation_id,
                timestamp,
            } => {
                info!(
                    event_type = "invoice_created",
                    invoice_id = %invoice_id,
                    federation_id = %federation_id,
                    amount_msat = amount_msat,
                    invoice = %sanitize_invoice(&invoice),
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Invoice created"
                );
            }
            FmcdEvent::InvoicePaid {
                operation_id,
                federation_id,
                amount_msat,
                correlation_id,
                timestamp,
            } => {
                info!(
                    event_type = "invoice_paid",
                    operation_id = %operation_id,
                    federation_id = %federation_id,
                    amount_msat = amount_msat,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Invoice paid"
                );
            }
            FmcdEvent::InvoiceExpired {
                invoice_id,
                federation_id,
                correlation_id,
                timestamp,
            } => {
                warn!(
                    event_type = "invoice_expired",
                    invoice_id = %invoice_id,
                    federation_id = %federation_id,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Invoice expired"
                );
            }
            FmcdEvent::FederationConnected {
                federation_id,
                correlation_id,
                timestamp,
            } => {
                info!(
                    event_type = "federation_connected",
                    federation_id = %federation_id,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Federation connected"
                );
            }
            FmcdEvent::FederationDisconnected {
                federation_id,
                reason,
                correlation_id,
                timestamp,
            } => {
                warn!(
                    event_type = "federation_disconnected",
                    federation_id = %federation_id,
                    reason = %reason,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Federation disconnected"
                );
            }
            FmcdEvent::FederationBalanceUpdated {
                federation_id,
                balance_msat,
                correlation_id,
                timestamp,
            } => {
                if self.include_debug_events {
                    debug!(
                        event_type = "federation_balance_updated",
                        federation_id = %federation_id,
                        balance_msat = balance_msat,
                        correlation_id = ?correlation_id,
                        timestamp = %timestamp,
                        "Federation balance updated"
                    );
                }
            }
            FmcdEvent::GatewaySelected {
                gateway_id,
                federation_id,
                payment_id,
                correlation_id,
                timestamp,
            } => {
                info!(
                    event_type = "gateway_selected",
                    gateway_id = %gateway_id,
                    federation_id = %federation_id,
                    payment_id = ?payment_id,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Gateway selected for payment"
                );
            }
            FmcdEvent::GatewayUnavailable {
                gateway_id,
                federation_id,
                reason,
                correlation_id,
                timestamp,
            } => {
                warn!(
                    event_type = "gateway_unavailable",
                    gateway_id = %gateway_id,
                    federation_id = %federation_id,
                    reason = %reason,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Gateway unavailable"
                );
            }
            FmcdEvent::DatabaseQueryExecuted {
                operation,
                key_prefix,
                duration_ms,
                success,
                error_message,
                correlation_id,
                timestamp,
            } => {
                if success {
                    if duration_ms > 100 {
                        // Warn on slow queries (>100ms)
                        warn!(
                            event_type = "database_query_slow",
                            operation = %operation,
                            key_prefix = %key_prefix,
                            duration_ms = duration_ms,
                            correlation_id = ?correlation_id,
                            timestamp = %timestamp,
                            "Slow database query detected"
                        );
                    } else if self.include_debug_events {
                        debug!(
                            event_type = "database_query_executed",
                            operation = %operation,
                            key_prefix = %key_prefix,
                            duration_ms = duration_ms,
                            correlation_id = ?correlation_id,
                            timestamp = %timestamp,
                            "Database query executed successfully"
                        );
                    }
                } else {
                    error!(
                        event_type = "database_query_failed",
                        operation = %operation,
                        key_prefix = %key_prefix,
                        duration_ms = duration_ms,
                        error_message = ?error_message,
                        correlation_id = ?correlation_id,
                        timestamp = %timestamp,
                        "Database query failed"
                    );
                }
            }
            FmcdEvent::AuthenticationAttempt {
                user_id,
                ip_address,
                endpoint,
                success,
                reason,
                correlation_id,
                timestamp,
            } => {
                if success {
                    info!(
                        event_type = "authentication_success",
                        user_id = ?user_id,
                        ip_address = %ip_address,
                        endpoint = %endpoint,
                        correlation_id = ?correlation_id,
                        timestamp = %timestamp,
                        "Authentication successful"
                    );
                } else {
                    warn!(
                        event_type = "authentication_failed",
                        user_id = ?user_id,
                        ip_address = %ip_address,
                        endpoint = %endpoint,
                        reason = ?reason,
                        correlation_id = ?correlation_id,
                        timestamp = %timestamp,
                        "Authentication failed"
                    );
                }
            }
            // Deposit events
            FmcdEvent::DepositAddressGenerated {
                operation_id,
                federation_id,
                address,
                correlation_id,
                timestamp,
            } => {
                info!(
                    operation_id = %operation_id,
                    federation_id = %federation_id,
                    address = %address,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Deposit address generated"
                );
            }
            FmcdEvent::DepositDetected {
                operation_id,
                federation_id,
                address,
                amount_sat,
                txid,
                correlation_id,
                timestamp,
            } => {
                info!(
                    operation_id = %operation_id,
                    federation_id = %federation_id,
                    address = %address,
                    amount_sat = amount_sat,
                    txid = %txid,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Deposit detected"
                );
            }
            // Withdrawal events
            FmcdEvent::WithdrawalInitiated {
                operation_id,
                federation_id,
                address,
                amount_sat,
                fee_sat,
                correlation_id,
                timestamp,
            } => {
                info!(
                    operation_id = %operation_id,
                    federation_id = %federation_id,
                    address = %address,
                    amount_sat = amount_sat,
                    fee_sat = fee_sat,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Withdrawal initiated"
                );
            }
            FmcdEvent::WithdrawalSucceeded {
                operation_id,
                federation_id,
                amount_sat,
                txid,
                timestamp,
            } => {
                info!(
                    event_type = "withdrawal_succeeded",
                    operation_id = %operation_id,
                    federation_id = %federation_id,
                    amount_sat = amount_sat,
                    txid = %txid,
                    timestamp = %timestamp,
                    "Withdrawal succeeded"
                );
            }
            FmcdEvent::WithdrawalFailed {
                operation_id,
                federation_id,
                reason,
                correlation_id,
                timestamp,
            } => {
                error!(
                    operation_id = %operation_id,
                    federation_id = %federation_id,
                    reason = %reason,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Withdrawal failed"
                );
            }
            FmcdEvent::PaymentRefunded {
                operation_id,
                federation_id,
                reason,
                correlation_id,
                timestamp,
            } => {
                info!(
                    event_type = "payment_refunded",
                    operation_id = %operation_id,
                    federation_id = %federation_id,
                    reason = %reason,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Payment refunded"
                );
            }
            FmcdEvent::DepositClaimed {
                operation_id,
                federation_id,
                amount_sat,
                txid,
                correlation_id,
                timestamp,
            } => {
                info!(
                    event_type = "deposit_claimed",
                    operation_id = %operation_id,
                    federation_id = %federation_id,
                    amount_sat = amount_sat,
                    txid = %txid,
                    correlation_id = ?correlation_id,
                    timestamp = %timestamp,
                    "Deposit claimed"
                );
            }
        }

        Ok(())
    }

    fn name(&self) -> &str {
        "logging"
    }

    /// Logging handler is critical - we want to ensure logs are written
    fn is_critical(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tracing_test::traced_test;

    use super::*;

    #[tokio::test]
    #[traced_test]
    async fn test_logging_handler_payment_events() {
        let handler = LoggingEventHandler::new(false);

        let event = FmcdEvent::PaymentInitiated {
            payment_id: "test_payment_id".to_string(),
            federation_id: "test_federation_id".to_string(),
            amount_msat: 1000,
            invoice: "lnbc1000n1pwjw8xepp5...".to_string(),
            correlation_id: Some("test_correlation".to_string()),
            timestamp: Utc::now(),
        };

        let result = handler.handle(event).await;
        assert!(result.is_ok());

        // Check that the log was written
        assert!(logs_contain("Payment initiated"));
        assert!(logs_contain("test_payment_id"));
        assert!(logs_contain("test_federation_id"));
    }

    #[tokio::test]
    #[traced_test]
    async fn test_logging_handler_sanitizes_sensitive_data() {
        let handler = LoggingEventHandler::new(false);

        let event = FmcdEvent::PaymentSucceeded {
            operation_id: "test_operation_id".to_string(),
            federation_id: "test_federation_id".to_string(),
            amount_msat: 1000,
            fee_msat: Some(10),
            preimage: "a1b2c3d4e5f6789012345678901234567890123456789012345678901234567890"
                .to_string(),
            correlation_id: Some("test-correlation-id".to_string()),
            timestamp: Utc::now(),
        };

        let result = handler.handle(event).await;
        assert!(result.is_ok());

        // Check that the sensitive data is sanitized in logs
        assert!(logs_contain("Payment succeeded"));
        assert!(logs_contain("test_operation_id"));
        // Full preimage should not appear in logs - it should be sanitized
        assert!(!logs_contain(
            "a1b2c3d4e5f6789012345678901234567890123456789012345678901234567890"
        ));
    }

    #[tokio::test]
    #[traced_test]
    async fn test_logging_handler_debug_events() {
        let handler_without_debug = LoggingEventHandler::new(false);

        let event = FmcdEvent::FederationBalanceUpdated {
            federation_id: "test_federation_id".to_string(),
            balance_msat: 5000,
            correlation_id: Some("test_correlation".to_string()),
            timestamp: Utc::now(),
        };

        // Handler without debug should not log this event
        let result = handler_without_debug.handle(event).await;
        assert!(result.is_ok());
        assert!(!logs_contain("Federation balance updated"));
    }

    #[tokio::test]
    async fn test_logging_handler_is_critical() {
        let handler = LoggingEventHandler::new(false);
        assert!(handler.is_critical());
    }
}
