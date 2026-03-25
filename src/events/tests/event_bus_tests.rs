#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use chrono::Utc;
    use tokio::time::{timeout, Duration};

    use crate::events::*;

    struct TestEventHandler {
        name: String,
        call_count: Arc<AtomicUsize>,
        should_fail: bool,
    }

    #[async_trait]
    impl EventHandler for TestEventHandler {
        async fn handle(&self, _event: FmcdEvent) -> anyhow::Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            if self.should_fail {
                anyhow::bail!("Test handler failure");
            }
            Ok(())
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    #[tokio::test]
    async fn test_event_bus_creation() {
        let event_bus = EventBus::new(100);
        let stats = event_bus.stats().await;
        assert_eq!(stats.capacity, 100);
        assert_eq!(stats.handler_count, 0);
        assert_eq!(stats.critical_handler_count, 0);
    }

    #[tokio::test]
    async fn test_handler_registration() {
        let event_bus = EventBus::new(100);
        let call_count = Arc::new(AtomicUsize::new(0));

        let handler = Arc::new(TestEventHandler {
            name: "test_handler".to_string(),
            call_count: call_count.clone(),
            should_fail: false,
        });

        event_bus.register_handler(handler).await;

        let stats = event_bus.stats().await;
        assert_eq!(stats.handler_count, 1);
    }

    #[tokio::test]
    async fn test_event_publishing() {
        let event_bus = EventBus::new(100);
        let call_count = Arc::new(AtomicUsize::new(0));

        let handler = Arc::new(TestEventHandler {
            name: "test_handler".to_string(),
            call_count: call_count.clone(),
            should_fail: false,
        });

        event_bus.register_handler(handler).await;

        let event = FmcdEvent::PaymentInitiated {
            payment_id: "test_payment_id".to_string(),
            federation_id: "test_federation_id".to_string(),
            amount_msat: 1000,
            invoice: "test_invoice".to_string(),
            correlation_id: Some("test_correlation_id".to_string()),
            timestamp: Utc::now(),
        };

        let _ = event_bus.publish(event).await.map_err(|e| {
            eprintln!("Failed to publish event: {}", e);
            e
        });

        // Give some time for the background task to complete
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_multiple_handlers() {
        let event_bus = EventBus::new(100);
        let call_count1 = Arc::new(AtomicUsize::new(0));
        let call_count2 = Arc::new(AtomicUsize::new(0));

        let handler1 = Arc::new(TestEventHandler {
            name: "test_handler_1".to_string(),
            call_count: call_count1.clone(),
            should_fail: false,
        });

        let handler2 = Arc::new(TestEventHandler {
            name: "test_handler_2".to_string(),
            call_count: call_count2.clone(),
            should_fail: false,
        });

        event_bus.register_handler(handler1).await;
        event_bus.register_handler(handler2).await;

        let event = FmcdEvent::PaymentSucceeded {
            operation_id: "test_operation_id".to_string(),
            federation_id: "test_federation_id".to_string(),
            amount_msat: 1000,
            preimage: "test_preimage".to_string(),
            timestamp: Utc::now(),
        };

        let _ = event_bus.publish(event).await.map_err(|e| {
            eprintln!("Failed to publish event: {}", e);
            e
        });

        // Give some time for the background tasks to complete
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_handler_failure_doesnt_affect_others() {
        let event_bus = EventBus::new(100);
        let call_count1 = Arc::new(AtomicUsize::new(0));
        let call_count2 = Arc::new(AtomicUsize::new(0));

        let failing_handler = Arc::new(TestEventHandler {
            name: "failing_handler".to_string(),
            call_count: call_count1.clone(),
            should_fail: true,
        });

        let working_handler = Arc::new(TestEventHandler {
            name: "working_handler".to_string(),
            call_count: call_count2.clone(),
            should_fail: false,
        });

        event_bus.register_handler(failing_handler).await;
        event_bus.register_handler(working_handler).await;

        let event = FmcdEvent::PaymentFailed {
            payment_id: "test_payment_id".to_string(),
            federation_id: "test_federation_id".to_string(),
            reason: "test_failure".to_string(),
            correlation_id: Some("test_correlation_id".to_string()),
            timestamp: Utc::now(),
        };

        // Publishing should succeed even if one handler fails
        let _ = event_bus.publish(event).await.map_err(|e| {
            eprintln!("Failed to publish event: {}", e);
            e
        });

        // Give some time for the background tasks to complete
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(call_count1.load(Ordering::SeqCst), 1); // Failed handler was called
        assert_eq!(call_count2.load(Ordering::SeqCst), 1); // Working handler
                                                           // was called
    }

    #[tokio::test]
    async fn test_broadcast_subscription() {
        let event_bus = EventBus::new(100);
        let mut receiver = event_bus.subscribe();

        let event = FmcdEvent::InvoiceCreated {
            invoice_id: "test_invoice_id".to_string(),
            federation_id: "test_federation_id".to_string(),
            amount_msat: 2000,
            invoice: "test_invoice".to_string(),
            correlation_id: Some("test_correlation_id".to_string()),
            timestamp: Utc::now(),
        };

        // Publish event
        let _ = event_bus.publish(event.clone()).await.map_err(|e| {
            eprintln!("Failed to publish event: {}", e);
            e
        });

        // Receive event via subscription
        let received_event = match timeout(Duration::from_millis(100), receiver.recv()).await {
            Ok(Ok(event)) => event,
            Ok(Err(e)) => panic!("Failed to receive event: {}", e),
            Err(_) => panic!("Timeout waiting for event"),
        };

        match received_event {
            FmcdEvent::InvoiceCreated { invoice_id, .. } => {
                assert_eq!(invoice_id, "test_invoice_id");
            }
            _ => panic!("Expected InvoiceCreated event"),
        }
    }

    #[tokio::test]
    async fn test_event_metadata() {
        let event = FmcdEvent::PaymentInitiated {
            payment_id: "test_payment_id".to_string(),
            federation_id: "test_federation_id".to_string(),
            amount_msat: 1000,
            invoice: "test_invoice".to_string(),
            correlation_id: Some("test_correlation_id".to_string()),
            timestamp: Utc::now(),
        };

        assert_eq!(event.event_type(), "payment_initiated");
        assert_eq!(
            event.correlation_id(),
            Some(&"test_correlation_id".to_string())
        );
        assert!(!event.event_id().is_empty());
    }
}
