#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;

    use fedimint_core::config::FederationId;
    use tokio::time::Duration;

    use crate::core::operations::{InvoiceTracker, PaymentState, PaymentTracker};
    use crate::events::EventBus;
    use crate::observability::correlation::RequestContext;

    fn create_test_federation_id() -> FederationId {
        FederationId::from_str("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .expect("Valid federation ID")
    }

    fn create_test_context() -> RequestContext {
        RequestContext::new(Some("test_correlation".to_string()))
    }

    #[tokio::test]
    async fn test_payment_tracker_creation() {
        let event_bus = Arc::new(EventBus::new(100));
        let federation_id = create_test_federation_id();
        let context = create_test_context();
        let invoice = "lnbc1000n1pwjw8xepp5...".to_string();

        let tracker = PaymentTracker::new(federation_id, &invoice, 1000, event_bus, Some(context));

        assert_eq!(*tracker.state(), PaymentState::Initiated);
        assert!(!tracker.payment_id().is_empty());
        assert_eq!(tracker.federation_id(), &federation_id.to_string());
        assert!(tracker.correlation_id().is_some());
    }

    #[tokio::test]
    async fn test_payment_id_derivation() {
        let invoice1 = "lnbc1000n1pwjw8xepp5test1";
        let invoice2 = "lnbc2000n1pwjw8xepp5test2";

        let id1 = PaymentTracker::derive_payment_id(invoice1);
        let id2 = PaymentTracker::derive_payment_id(invoice2);

        // IDs should be deterministic
        assert_eq!(id1, PaymentTracker::derive_payment_id(invoice1));

        // Different invoices should produce different IDs
        assert_ne!(id1, id2);

        // IDs should be hex strings of expected length (32 chars = 16 bytes)
        assert_eq!(id1.len(), 32);
        assert_eq!(id2.len(), 32);
        assert!(id1.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(id2.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn test_payment_lifecycle() {
        let event_bus = Arc::new(EventBus::new(100));
        let federation_id = create_test_federation_id();
        let context = create_test_context();
        let invoice = "lnbc1000n1pwjw8xepp5test".to_string();

        let mut tracker =
            PaymentTracker::new(federation_id, &invoice, 1000, event_bus, Some(context));

        // Test initiation
        tracker.initiate(invoice.clone(), 1000).await;
        assert_eq!(*tracker.state(), PaymentState::Initiated);
        assert!(!tracker.is_terminal());

        // Test gateway selection
        tracker.gateway_selected("test_gateway".to_string()).await;
        assert_eq!(*tracker.state(), PaymentState::GatewaySelected);
        assert!(!tracker.is_terminal());

        // Test execution start
        tracker.start_execution().await;
        assert_eq!(*tracker.state(), PaymentState::Executing);
        assert!(!tracker.is_terminal());

        // Test success
        tracker
            .succeed("test_preimage".to_string(), 1000, 100)
            .await;
        assert_eq!(*tracker.state(), PaymentState::Succeeded);
        assert!(tracker.is_terminal());
    }

    #[tokio::test]
    async fn test_payment_failure() {
        let event_bus = Arc::new(EventBus::new(100));
        let federation_id = create_test_federation_id();
        let context = create_test_context();
        let invoice = "lnbc1000n1pwjw8xepp5test".to_string();

        let mut tracker =
            PaymentTracker::new(federation_id, &invoice, 1000, event_bus, Some(context));

        tracker.initiate(invoice.clone(), 1000).await;
        tracker.fail("Test failure reason".to_string()).await;

        assert_eq!(*tracker.state(), PaymentState::Failed);
        assert!(tracker.is_terminal());
    }

    #[tokio::test]
    async fn test_payment_duration() {
        let event_bus = Arc::new(EventBus::new(100));
        let federation_id = create_test_federation_id();
        let context = create_test_context();
        let invoice = "lnbc1000n1pwjw8xepp5test".to_string();

        let tracker = PaymentTracker::new(federation_id, &invoice, 1000, event_bus, Some(context));

        // Small delay to ensure duration > 0
        tokio::time::sleep(Duration::from_millis(10)).await;

        let duration = tracker.duration();
        assert!(duration.num_milliseconds() > 0);
    }

    #[tokio::test]
    async fn test_invoice_tracker() {
        let event_bus = Arc::new(EventBus::new(100));
        let federation_id = create_test_federation_id();
        let context = create_test_context();

        let tracker = InvoiceTracker::new(
            "test_invoice_id".to_string(),
            federation_id,
            event_bus,
            Some(context),
        );

        assert_eq!(tracker.invoice_id(), "test_invoice_id");

        // Test invoice lifecycle
        tracker
            .created(2000, "test_invoice_string".to_string())
            .await;
        tracker.paid(2000).await;
        tracker.expired().await;
    }

    #[tokio::test]
    async fn test_payment_state_string_conversion() {
        assert_eq!(PaymentState::Initiated.as_str(), "initiated");
        assert_eq!(PaymentState::GatewaySelected.as_str(), "gateway_selected");
        assert_eq!(PaymentState::Executing.as_str(), "executing");
        assert_eq!(PaymentState::Succeeded.as_str(), "succeeded");
        assert_eq!(PaymentState::Failed.as_str(), "failed");
    }

    #[tokio::test]
    async fn test_tracker_with_no_context() {
        let event_bus = Arc::new(EventBus::new(100));
        let federation_id = create_test_federation_id();
        let invoice = "lnbc1000n1pwjw8xepp5test".to_string();

        let tracker = PaymentTracker::new(federation_id, &invoice, 1000, event_bus, None);

        assert!(tracker.correlation_id().is_none());
    }
}
