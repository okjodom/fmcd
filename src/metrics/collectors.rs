use std::sync::Arc;

use async_trait::async_trait;
use lazy_static::lazy_static;
use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing::{debug, info};

use crate::events::{EventHandler, FmcdEvent};

// Define metric names as constants to avoid typos
pub const PAYMENTS_TOTAL: &str = "fmcd_payments_total";
pub const PAYMENT_AMOUNT_MSAT: &str = "fmcd_payment_amount_msat";
pub const PAYMENT_DURATION_SECONDS: &str = "fmcd_payment_duration_seconds";
pub const PAYMENT_FEES_MSAT: &str = "fmcd_payment_fees_msat";

pub const INVOICES_TOTAL: &str = "fmcd_invoices_total";
pub const INVOICE_AMOUNT_MSAT: &str = "fmcd_invoice_amount_msat";

pub const GATEWAY_SELECTIONS_TOTAL: &str = "fmcd_gateway_selections_total";
pub const GATEWAY_FAILURES_TOTAL: &str = "fmcd_gateway_failures_total";

pub const FEDERATION_BALANCE_MSAT: &str = "fmcd_federation_balance_msat";
pub const FEDERATION_CONNECTIONS_TOTAL: &str = "fmcd_federation_connections_total";

pub const API_REQUESTS_TOTAL: &str = "fmcd_api_requests_total";
pub const API_REQUEST_DURATION_SECONDS: &str = "fmcd_api_request_duration_seconds";

pub const DATABASE_QUERIES_TOTAL: &str = "fmcd_database_queries_total";
pub const DATABASE_QUERY_DURATION_SECONDS: &str = "fmcd_database_query_duration_seconds";

pub const AUTH_ATTEMPTS_TOTAL: &str = "fmcd_auth_attempts_total";

pub const WEBHOOK_DELIVERIES_TOTAL: &str = "fmcd_webhook_deliveries_total";
pub const WEBHOOK_DELIVERY_DURATION_SECONDS: &str = "fmcd_webhook_delivery_duration_seconds";

pub const EVENT_BUS_EVENTS_TOTAL: &str = "fmcd_event_bus_events_total";
pub const EVENT_BUS_HANDLERS_ACTIVE: &str = "fmcd_event_bus_handlers_active";

lazy_static! {
    static ref PROMETHEUS_HANDLE: Arc<tokio::sync::RwLock<Option<PrometheusHandle>>> =
        Arc::new(tokio::sync::RwLock::new(None));
}

/// Initialize Prometheus metrics collection
pub async fn init_prometheus_metrics() -> anyhow::Result<PrometheusHandle> {
    let builder = PrometheusBuilder::new();

    let handle = builder
        .install_recorder()
        .map_err(|e| anyhow::anyhow!("Failed to install Prometheus recorder: {}", e))?;

    // Metrics are now automatically registered in metrics 0.23 when first used
    // No need for explicit registration
    info!("Prometheus metrics collection initialized");

    // Store handle for later retrieval
    let mut handle_lock = PROMETHEUS_HANDLE.write().await;
    *handle_lock = Some(handle.clone());

    info!("Prometheus metrics collection initialized successfully");
    Ok(handle)
}

/// Get the Prometheus metrics handle
pub async fn get_prometheus_handle() -> Option<PrometheusHandle> {
    PROMETHEUS_HANDLE.read().await.clone()
}

/// Metrics collector that implements the EventHandler trait
pub struct MetricsCollector {}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {}
    }

    /// Record payment metrics from payment events
    fn record_payment_metrics(&self, event: &FmcdEvent) {
        match event {
            FmcdEvent::PaymentInitiated {
                federation_id,
                amount_msat,
                ..
            } => {
                counter!(PAYMENTS_TOTAL, "federation_id" => federation_id.clone(), "status" => "initiated").increment(1);
                histogram!(PAYMENT_AMOUNT_MSAT, "federation_id" => federation_id.clone())
                    .record(*amount_msat as f64);

                debug!(
                    federation_id = %federation_id,
                    amount_msat = amount_msat,
                    "Recorded payment initiation metrics"
                );
            }
            FmcdEvent::PaymentSucceeded { federation_id, .. } => {
                counter!(PAYMENTS_TOTAL, "federation_id" => federation_id.clone(), "status" => "succeeded").increment(1);
                // Note: fee_msat is not available in PaymentSucceeded event

                debug!(
                    federation_id = %federation_id,
                    "Recorded payment success metrics"
                );
            }
            FmcdEvent::PaymentFailed {
                federation_id,
                reason,
                ..
            } => {
                counter!(PAYMENTS_TOTAL, "federation_id" => federation_id.clone(), "status" => "failed").increment(1);

                debug!(
                    federation_id = %federation_id,
                    reason = %reason,
                    "Recorded payment failure metrics"
                );
            }
            _ => {}
        }
    }

    /// Record invoice metrics from invoice events
    fn record_invoice_metrics(&self, event: &FmcdEvent) {
        match event {
            FmcdEvent::InvoiceCreated {
                federation_id,
                amount_msat,
                ..
            } => {
                counter!(INVOICES_TOTAL, "federation_id" => federation_id.clone(), "status" => "created").increment(1);
                histogram!(INVOICE_AMOUNT_MSAT, "federation_id" => federation_id.clone())
                    .record(*amount_msat as f64);

                debug!(
                    federation_id = %federation_id,
                    amount_msat = amount_msat,
                    "Recorded invoice creation metrics"
                );
            }
            FmcdEvent::InvoicePaid {
                federation_id,
                amount_msat,
                ..
            } => {
                counter!(INVOICES_TOTAL, "federation_id" => federation_id.clone(), "status" => "paid").increment(1);
                histogram!(INVOICE_AMOUNT_MSAT, "federation_id" => federation_id.clone())
                    .record(*amount_msat as f64);

                debug!(
                    federation_id = %federation_id,
                    amount_msat = amount_msat,
                    "Recorded invoice payment metrics"
                );
            }
            FmcdEvent::InvoiceExpired { federation_id, .. } => {
                counter!(INVOICES_TOTAL, "federation_id" => federation_id.clone(), "status" => "expired").increment(1);

                debug!(
                    federation_id = %federation_id,
                    "Recorded invoice expiration metrics"
                );
            }
            _ => {}
        }
    }

    /// Record gateway metrics from gateway events
    fn record_gateway_metrics(&self, event: &FmcdEvent) {
        match event {
            FmcdEvent::GatewaySelected {
                gateway_id,
                federation_id,
                ..
            } => {
                counter!(
                    GATEWAY_SELECTIONS_TOTAL,
                    "federation_id" => federation_id.clone(),
                    "gateway_id" => gateway_id.clone(),
                    "result" => "selected"
                )
                .increment(1);

                debug!(
                    federation_id = %federation_id,
                    gateway_id = %gateway_id,
                    "Recorded gateway selection metrics"
                );
            }
            FmcdEvent::GatewayUnavailable {
                gateway_id,
                federation_id,
                reason,
                ..
            } => {
                counter!(
                    GATEWAY_FAILURES_TOTAL,
                    "federation_id" => federation_id.clone(),
                    "gateway_id" => gateway_id.clone(),
                    "reason" => reason.clone()
                )
                .increment(1);

                debug!(
                    federation_id = %federation_id,
                    gateway_id = %gateway_id,
                    reason = %reason,
                    "Recorded gateway failure metrics"
                );
            }
            _ => {}
        }
    }

    /// Record federation metrics from federation events
    fn record_federation_metrics(&self, event: &FmcdEvent) {
        match event {
            FmcdEvent::FederationConnected { federation_id, .. } => {
                counter!(
                    FEDERATION_CONNECTIONS_TOTAL,
                    "federation_id" => federation_id.clone(),
                    "status" => "connected"
                )
                .increment(1);

                debug!(
                    federation_id = %federation_id,
                    "Recorded federation connection metrics"
                );
            }
            FmcdEvent::FederationDisconnected {
                federation_id,
                reason,
                ..
            } => {
                counter!(
                    FEDERATION_CONNECTIONS_TOTAL,
                    "federation_id" => federation_id.clone(),
                    "status" => "disconnected"
                )
                .increment(1);

                debug!(
                    federation_id = %federation_id,
                    reason = %reason,
                    "Recorded federation disconnection metrics"
                );
            }
            FmcdEvent::FederationBalanceUpdated {
                federation_id,
                balance_msat,
                ..
            } => {
                gauge!(
                    FEDERATION_BALANCE_MSAT,
                    "federation_id" => federation_id.clone()
                )
                .set(*balance_msat as f64);

                debug!(
                    federation_id = %federation_id,
                    balance_msat = balance_msat,
                    "Recorded federation balance metrics"
                );
            }
            _ => {}
        }
    }

    /// Record database metrics from database events
    fn record_database_metrics(&self, event: &FmcdEvent) {
        if let FmcdEvent::DatabaseQueryExecuted {
            operation,
            duration_ms,
            success,
            ..
        } = event
        {
            let status = if *success { "success" } else { "error" };

            counter!(
                DATABASE_QUERIES_TOTAL,
                "operation" => operation.clone(),
                "status" => status
            )
            .increment(1);

            histogram!(
                DATABASE_QUERY_DURATION_SECONDS,
                "operation" => operation.clone()
            )
            .record(*duration_ms as f64 / 1000.0);

            debug!(
                operation = %operation,
                duration_ms = duration_ms,
                success = success,
                "Recorded database query metrics"
            );
        }
    }

    /// Record authentication metrics from auth events
    fn record_auth_metrics(&self, event: &FmcdEvent) {
        if let FmcdEvent::AuthenticationAttempt {
            success, endpoint, ..
        } = event
        {
            let status = if *success { "success" } else { "failure" };

            counter!(
                AUTH_ATTEMPTS_TOTAL,
                "endpoint" => endpoint.clone(),
                "status" => status
            )
            .increment(1);

            debug!(
                endpoint = %endpoint,
                success = success,
                "Recorded authentication attempt metrics"
            );
        }
    }

    /// Record general event bus metrics
    fn record_event_metrics(&self, event: &FmcdEvent) {
        counter!(
            EVENT_BUS_EVENTS_TOTAL,
            "event_type" => event.event_type()
        )
        .increment(1);

        debug!(
            event_type = %event.event_type(),
            "Recorded event bus metrics"
        );
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventHandler for MetricsCollector {
    async fn handle(&self, event: FmcdEvent) -> anyhow::Result<()> {
        // Record metrics for different event categories
        self.record_payment_metrics(&event);
        self.record_invoice_metrics(&event);
        self.record_gateway_metrics(&event);
        self.record_federation_metrics(&event);
        self.record_database_metrics(&event);
        self.record_auth_metrics(&event);
        self.record_event_metrics(&event);

        Ok(())
    }

    fn name(&self) -> &str {
        "metrics_collector"
    }

    fn is_critical(&self) -> bool {
        // Metrics collection is not critical - it should not block event processing
        false
    }
}

/// Utility functions for recording API metrics from middleware
pub mod api_metrics {
    use std::time::Duration;

    use super::*;

    /// Record API request metrics
    pub fn record_api_request(method: &str, path: &str, status_code: u16, duration: Duration) {
        let status_class = match status_code {
            200..=299 => "2xx",
            300..=399 => "3xx",
            400..=499 => "4xx",
            500..=599 => "5xx",
            _ => "unknown",
        };

        counter!(
            API_REQUESTS_TOTAL,
            "method" => method.to_string(),
            "endpoint" => path.to_string(),
            "status" => status_class.to_string()
        )
        .increment(1);

        histogram!(
            API_REQUEST_DURATION_SECONDS,
            "method" => method.to_string(),
            "endpoint" => path.to_string()
        )
        .record(duration.as_secs_f64());

        debug!(
            method = %method,
            path = %path,
            status_code = status_code,
            duration_ms = duration.as_millis(),
            "Recorded API request metrics"
        );
    }
}

/// Utility functions for recording webhook metrics
pub mod webhook_metrics {
    use std::time::Duration;

    use super::*;

    /// Record webhook delivery metrics
    pub fn record_webhook_delivery(
        endpoint_id: &str,
        event_type: &str,
        success: bool,
        duration: Duration,
    ) {
        let status = if success { "success" } else { "failure" };

        counter!(
            WEBHOOK_DELIVERIES_TOTAL,
            "endpoint_id" => endpoint_id.to_string(),
            "event_type" => event_type.to_string(),
            "status" => status.to_string()
        )
        .increment(1);

        histogram!(
            WEBHOOK_DELIVERY_DURATION_SECONDS,
            "endpoint_id" => endpoint_id.to_string(),
            "event_type" => event_type.to_string()
        )
        .record(duration.as_secs_f64());

        debug!(
            endpoint_id = %endpoint_id,
            event_type = %event_type,
            success = success,
            duration_ms = duration.as_millis(),
            "Recorded webhook delivery metrics"
        );
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::events::FmcdEvent;

    #[tokio::test]
    async fn test_metrics_collector_creation() {
        let collector = MetricsCollector::new();
        assert_eq!(collector.name(), "metrics_collector");
        assert!(!collector.is_critical());
    }

    #[tokio::test]
    async fn test_payment_metrics_recording() {
        let collector = MetricsCollector::new();

        let event = FmcdEvent::PaymentSucceeded {
            operation_id: "test-payment".to_string(),
            federation_id: "test-fed".to_string(),
            amount_msat: 50000,
            fee_msat: Some(50),
            preimage: "test-preimage".to_string(),
            correlation_id: Some("test-correlation-id".to_string()),
            timestamp: Utc::now(),
        };

        // This should not fail
        let result = collector.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_invoice_metrics_recording() {
        let collector = MetricsCollector::new();

        let event = FmcdEvent::InvoiceCreated {
            invoice_id: "test-invoice".to_string(),
            federation_id: "test-fed".to_string(),
            amount_msat: 50000,
            invoice: "test-invoice-string".to_string(),
            correlation_id: Some("test-correlation".to_string()),
            timestamp: Utc::now(),
        };

        let result = collector.handle(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_gateway_metrics_recording() {
        let collector = MetricsCollector::new();

        let event = FmcdEvent::GatewaySelected {
            gateway_id: "test-gateway".to_string(),
            federation_id: "test-fed".to_string(),
            payment_id: Some("test-payment".to_string()),
            correlation_id: Some("test-correlation".to_string()),
            timestamp: Utc::now(),
        };

        let result = collector.handle(event).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_api_metrics_utility() {
        use std::time::Duration;

        // This should not panic
        api_metrics::record_api_request("GET", "/health", 200, Duration::from_millis(50));
        api_metrics::record_api_request("POST", "/ln/pay", 400, Duration::from_millis(100));
        api_metrics::record_api_request("GET", "/admin/info", 500, Duration::from_secs(1));
    }

    #[test]
    fn test_webhook_metrics_utility() {
        use std::time::Duration;

        // This should not panic
        webhook_metrics::record_webhook_delivery(
            "endpoint-1",
            "payment_succeeded",
            true,
            Duration::from_millis(200),
        );
        webhook_metrics::record_webhook_delivery(
            "endpoint-2",
            "invoice_created",
            false,
            Duration::from_secs(2),
        );
    }
}
