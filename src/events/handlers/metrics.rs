use async_trait::async_trait;
use metrics::{counter, histogram};
use tracing::debug;

use crate::events::{EventHandler, FmcdEvent};
use crate::metrics::{
    AUTH_ATTEMPTS_TOTAL, DATABASE_QUERIES_TOTAL, DATABASE_QUERY_DURATION_SECONDS,
    EVENT_BUS_EVENTS_TOTAL, FEDERATION_BALANCE_MSAT, FEDERATION_CONNECTIONS_TOTAL,
    GATEWAY_FAILURES_TOTAL, GATEWAY_SELECTIONS_TOTAL, INVOICES_TOTAL, INVOICE_AMOUNT_MSAT,
    PAYMENTS_TOTAL, PAYMENT_AMOUNT_MSAT, PAYMENT_FEES_MSAT,
};

/// Event handler that collects metrics from events for Prometheus export
pub struct MetricsEventHandler {}

impl MetricsEventHandler {
    pub fn new(_service_name: impl Into<String>) -> Self {
        Self {}
    }

    /// Record payment metrics using standardized metric names
    fn record_payment_metrics(
        &self,
        federation_id: &str,
        status: &str,
        amount_msat: Option<u64>,
        fee_msat: Option<u64>,
    ) {
        // Use standardized metric names
        counter!(PAYMENTS_TOTAL, "federation_id" => federation_id.to_string(), "status" => status.to_string()).increment(1);

        // Payment amounts (if available)
        if let Some(amount) = amount_msat {
            histogram!(PAYMENT_AMOUNT_MSAT, "federation_id" => federation_id.to_string())
                .record(amount as f64);
        }

        // Payment fees (if available)
        if let Some(fee) = fee_msat {
            histogram!(PAYMENT_FEES_MSAT, "federation_id" => federation_id.to_string())
                .record(fee as f64);
        }
    }

    /// Record invoice metrics using standardized metric names
    fn record_invoice_metrics(&self, federation_id: &str, status: &str, amount_msat: Option<u64>) {
        counter!(INVOICES_TOTAL, "federation_id" => federation_id.to_string(), "status" => status.to_string()).increment(1);

        // Invoice amounts (if available)
        if let Some(amount) = amount_msat {
            histogram!(INVOICE_AMOUNT_MSAT, "federation_id" => federation_id.to_string())
                .record(amount as f64);
        }
    }

    /// Record federation metrics using standardized metric names
    fn record_federation_metrics(
        &self,
        federation_id: &str,
        status: &str,
        balance_msat: Option<u64>,
    ) {
        counter!(FEDERATION_CONNECTIONS_TOTAL, "federation_id" => federation_id.to_string(), "status" => status.to_string()).increment(1);

        // Federation balance histogram (if available)
        if let Some(balance) = balance_msat {
            histogram!(FEDERATION_BALANCE_MSAT, "federation_id" => federation_id.to_string())
                .record(balance as f64);
        }
    }

    /// Record gateway metrics using standardized metric names
    fn record_gateway_metrics(&self, gateway_id: &str, federation_id: &str, status: &str) {
        let metric_name = if status == "selected" {
            GATEWAY_SELECTIONS_TOTAL
        } else {
            GATEWAY_FAILURES_TOTAL
        };

        counter!(metric_name,
            "gateway_id" => gateway_id.to_string(),
            "federation_id" => federation_id.to_string(),
            "result" => status.to_string()
        )
        .increment(1);
    }

    /// Record database metrics using standardized metric names
    fn record_database_metrics(&self, operation: &str, duration_ms: u128, success: bool) {
        let status = if success { "success" } else { "error" };

        counter!(DATABASE_QUERIES_TOTAL, "operation" => operation.to_string(), "status" => status.to_string()).increment(1);

        // Database operation duration in seconds (converting from milliseconds)
        histogram!(DATABASE_QUERY_DURATION_SECONDS, "operation" => operation.to_string())
            .record(duration_ms as f64 / 1000.0);
    }

    /// Record authentication metrics using standardized metric names
    fn record_auth_metrics(&self, endpoint: &str, success: bool, _ip_address: &str) {
        let status = if success { "success" } else { "failure" };

        counter!(AUTH_ATTEMPTS_TOTAL, "endpoint" => endpoint.to_string(), "status" => status.to_string()).increment(1);
    }
}

#[async_trait]
impl EventHandler for MetricsEventHandler {
    async fn handle(&self, event: FmcdEvent) -> anyhow::Result<()> {
        // Capture event type before matching (to avoid move issues)
        let event_type = event.event_type().to_string();

        match event {
            FmcdEvent::PaymentInitiated {
                federation_id,
                amount_msat,
                ..
            } => {
                self.record_payment_metrics(&federation_id, "initiated", Some(amount_msat), None);
            }
            FmcdEvent::PaymentSucceeded {
                federation_id,
                amount_msat,
                fee_msat,
                ..
            } => {
                self.record_payment_metrics(
                    &federation_id,
                    "succeeded",
                    Some(amount_msat),
                    fee_msat,
                );
            }
            FmcdEvent::PaymentFailed { federation_id, .. } => {
                self.record_payment_metrics(&federation_id, "failed", None, None);
            }
            FmcdEvent::InvoiceCreated {
                federation_id,
                amount_msat,
                ..
            } => {
                self.record_invoice_metrics(&federation_id, "created", Some(amount_msat));
            }
            FmcdEvent::InvoicePaid {
                federation_id,
                amount_msat,
                ..
            } => {
                self.record_invoice_metrics(&federation_id, "paid", Some(amount_msat));
            }
            FmcdEvent::InvoiceExpired { federation_id, .. } => {
                self.record_invoice_metrics(&federation_id, "expired", None);
            }
            FmcdEvent::FederationConnected { federation_id, .. } => {
                self.record_federation_metrics(&federation_id, "connected", None);
            }
            FmcdEvent::FederationDisconnected { federation_id, .. } => {
                self.record_federation_metrics(&federation_id, "disconnected", None);
            }
            FmcdEvent::FederationBalanceUpdated {
                federation_id,
                balance_msat,
                ..
            } => {
                self.record_federation_metrics(
                    &federation_id,
                    "balance_updated",
                    Some(balance_msat),
                );
            }
            FmcdEvent::GatewaySelected {
                gateway_id,
                federation_id,
                ..
            } => {
                self.record_gateway_metrics(&gateway_id, &federation_id, "selected");
            }
            FmcdEvent::GatewayUnavailable {
                gateway_id,
                federation_id,
                ..
            } => {
                self.record_gateway_metrics(&gateway_id, &federation_id, "unavailable");
            }
            FmcdEvent::DatabaseQueryExecuted {
                operation,
                duration_ms,
                success,
                ..
            } => {
                self.record_database_metrics(&operation, duration_ms, success);
            }
            FmcdEvent::AuthenticationAttempt {
                endpoint,
                success,
                ip_address,
                ..
            } => {
                self.record_auth_metrics(&endpoint, success, &ip_address);
            }
            // Withdrawal events
            FmcdEvent::WithdrawalInitiated {
                federation_id,
                amount_sat,
                ..
            } => {
                counter!(PAYMENTS_TOTAL, "federation_id" => federation_id.clone(), "type" => "withdrawal", "status" => "initiated").increment(1);
                histogram!(PAYMENT_AMOUNT_MSAT, "federation_id" => federation_id)
                    .record((amount_sat * 1000) as f64);
            }
            FmcdEvent::WithdrawalSucceeded { federation_id, .. } => {
                counter!(PAYMENTS_TOTAL, "federation_id" => federation_id, "type" => "withdrawal", "status" => "completed").increment(1);
            }
            FmcdEvent::WithdrawalFailed { federation_id, .. } => {
                counter!(PAYMENTS_TOTAL, "federation_id" => federation_id, "type" => "withdrawal", "status" => "failed").increment(1);
            }
            // Deposit events
            FmcdEvent::DepositAddressGenerated { federation_id, .. } => {
                counter!(PAYMENTS_TOTAL, "federation_id" => federation_id, "type" => "deposit", "status" => "address_generated").increment(1);
            }
            FmcdEvent::DepositDetected {
                federation_id,
                amount_sat,
                ..
            } => {
                counter!(PAYMENTS_TOTAL, "federation_id" => federation_id.clone(), "type" => "deposit", "status" => "detected").increment(1);
                histogram!(PAYMENT_AMOUNT_MSAT, "federation_id" => federation_id)
                    .record((amount_sat * 1000) as f64);
            }
            FmcdEvent::PaymentRefunded { federation_id, .. } => {
                counter!(PAYMENTS_TOTAL, "federation_id" => federation_id, "status" => "refunded")
                    .increment(1);
            }
            FmcdEvent::DepositClaimed {
                federation_id,
                amount_sat,
                ..
            } => {
                counter!(PAYMENTS_TOTAL, "federation_id" => federation_id.clone(), "type" => "deposit", "status" => "claimed").increment(1);
                histogram!(PAYMENT_AMOUNT_MSAT, "federation_id" => federation_id)
                    .record((amount_sat * 1000) as f64);
            }
        }

        // Record general event bus metrics
        counter!(EVENT_BUS_EVENTS_TOTAL, "event_type" => event_type).increment(1);

        debug!(handler = self.name(), "Event metrics recorded");

        Ok(())
    }

    fn name(&self) -> &str {
        "metrics"
    }

    /// Metrics handler is not critical - failures shouldn't block event
    /// processing
    fn is_critical(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono::Utc;

    use super::*;

    // Mock metrics implementation for testing
    static PAYMENT_COUNTER: AtomicU64 = AtomicU64::new(0);
    static INVOICE_COUNTER: AtomicU64 = AtomicU64::new(0);
    static FEDERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

    // Helper to reset counters for each test
    fn reset_test_counters() {
        PAYMENT_COUNTER.store(0, Ordering::SeqCst);
        INVOICE_COUNTER.store(0, Ordering::SeqCst);
        FEDERATION_COUNTER.store(0, Ordering::SeqCst);
    }

    #[tokio::test]
    async fn test_metrics_handler_payment_events() {
        reset_test_counters();
        let handler = MetricsEventHandler::new("test");

        let event = FmcdEvent::PaymentSucceeded {
            operation_id: "test_operation_id".to_string(),
            federation_id: "test_federation_id".to_string(),
            amount_msat: 1000,
            fee_msat: Some(10),
            preimage: "test_preimage".to_string(),
            correlation_id: Some("test-correlation-id".to_string()),
            timestamp: Utc::now(),
        };

        let result = handler.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_metrics_handler_invoice_events() {
        reset_test_counters();
        let handler = MetricsEventHandler::new("test");

        let event = FmcdEvent::InvoiceCreated {
            invoice_id: "test_invoice_id".to_string(),
            federation_id: "test_federation_id".to_string(),
            amount_msat: 2000,
            invoice: "test_invoice".to_string(),
            correlation_id: Some("test_correlation".to_string()),
            timestamp: Utc::now(),
        };

        let result = handler.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_metrics_handler_database_events() {
        reset_test_counters();
        let handler = MetricsEventHandler::new("test");

        let event = FmcdEvent::DatabaseQueryExecuted {
            operation: "get".to_string(),
            key_prefix: "12345678".to_string(),
            duration_ms: 150, // Slow query
            success: true,
            error_message: None,
            correlation_id: Some("test_correlation".to_string()),
            timestamp: Utc::now(),
        };

        let result = handler.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_metrics_handler_auth_events() {
        reset_test_counters();
        let handler = MetricsEventHandler::new("test");

        let event = FmcdEvent::AuthenticationAttempt {
            user_id: Some("test_user".to_string()),
            ip_address: "192.168.1.100".to_string(),
            endpoint: "/ln/pay".to_string(),
            success: false,
            reason: Some("Invalid token".to_string()),
            correlation_id: Some("test_correlation".to_string()),
            timestamp: Utc::now(),
        };

        let result = handler.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_metrics_handler_is_not_critical() {
        let handler = MetricsEventHandler::new("test");
        assert!(!handler.is_critical());
    }

    #[tokio::test]
    async fn test_metrics_handler_name() {
        let handler = MetricsEventHandler::new("test");
        assert_eq!(handler.name(), "metrics");
    }
}
