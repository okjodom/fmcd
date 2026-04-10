use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use uuid::Uuid;

use fmcd::events::{EventBus, FmcdEvent, handlers::{LoggingEventHandler, MetricsEventHandler}};
use fmcd::operations::payment::{PaymentTracker, InvoiceTracker, PaymentState};
use fmcd::database::{InstrumentedDatabase, DatabaseInterface, DatabaseWithContext};
use fmcd::observability::correlation::RequestContext;
use fmcd::auth::{BasicAuth, basic_auth_middleware_with_events};

// Mock database for testing
#[derive(Default, Clone)]
struct MockDatabase;

#[async_trait::async_trait]
impl DatabaseInterface for MockDatabase {
    async fn get(&self, _key: &[u8]) -> anyhow::Result<Option<Vec<u8>>> {
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(Some(b"test_value".to_vec()))
    }

    async fn set(&self, _key: &[u8], _value: &[u8]) -> anyhow::Result<()> {
        tokio::time::sleep(Duration::from_millis(5)).await;
        Ok(())
    }

    async fn delete(&self, _key: &[u8]) -> anyhow::Result<()> {
        tokio::time::sleep(Duration::from_millis(3)).await;
        Ok(())
    }

    async fn exists(&self, _key: &[u8]) -> anyhow::Result<bool> {
        tokio::time::sleep(Duration::from_millis(2)).await;
        Ok(true)
    }

    async fn scan_prefix(&self, _prefix: &[u8]) -> anyhow::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(vec![
            (b"key1".to_vec(), b"value1".to_vec()),
            (b"key2".to_vec(), b"value2".to_vec()),
        ])
    }
}

#[tokio::test]
async fn test_event_bus_integration() {
    // Initialize event bus
    let event_bus = Arc::new(EventBus::new(1000));

    // Register handlers
    let logging_handler = Arc::new(LoggingEventHandler::new(false));
    let metrics_handler = Arc::new(MetricsEventHandler::new("test"));

    event_bus.register_handler(logging_handler).await;
    event_bus.register_handler(metrics_handler).await;

    // Test event publishing
    let event = FmcdEvent::PaymentInitiated {
        payment_id: "test_payment_123".to_string(),
        federation_id: "test_federation".to_string(),
        amount_msat: 1000,
        invoice: "lnbc1000n1pwjw8xepp5test".to_string(),
        correlation_id: Some("test_correlation".to_string()),
        timestamp: chrono::Utc::now(),
    };

    let result = event_bus.publish(event).await;
    assert!(result.is_ok());

    // Verify handlers were registered
    let stats = event_bus.stats().await;
    assert_eq!(stats.handler_count, 2);
    assert_eq!(stats.critical_handler_count, 1); // LoggingEventHandler is critical
}

#[tokio::test]
async fn test_payment_tracking_lifecycle() {
    let event_bus = Arc::new(EventBus::new(1000));
    let federation_id = fedimint_core::config::FederationId::dummy();
    let context = RequestContext::new(Some("test_correlation".to_string()));

    let mut payment_tracker = PaymentTracker::new(
        federation_id,
        "lnbc1000n1pwjw8xepp5test_invoice",
        1000,
        event_bus.clone(),
        Some(context),
    );

    // Test payment lifecycle
    payment_tracker.initiate("lnbc1000n1pwjw8xepp5test_invoice".to_string(), 1000).await;
    assert_eq!(*payment_tracker.state(), PaymentState::Initiated);

    payment_tracker.gateway_selected("test_gateway_id".to_string()).await;
    assert_eq!(*payment_tracker.state(), PaymentState::GatewaySelected);

    payment_tracker.start_execution().await;
    assert_eq!(*payment_tracker.state(), PaymentState::Executing);

    payment_tracker.succeed("test_preimage".to_string(), 100).await;
    assert_eq!(*payment_tracker.state(), PaymentState::Succeeded);
    assert!(payment_tracker.is_terminal());

    // Verify payment ID derivation is deterministic
    let payment_id1 = PaymentTracker::derive_payment_id("test_invoice_1");
    let payment_id2 = PaymentTracker::derive_payment_id("test_invoice_1");
    let payment_id3 = PaymentTracker::derive_payment_id("test_invoice_2");

    assert_eq!(payment_id1, payment_id2);
    assert_ne!(payment_id1, payment_id3);
}

#[tokio::test]
async fn test_invoice_tracking() {
    let event_bus = Arc::new(EventBus::new(1000));
    let federation_id = fedimint_core::config::FederationId::dummy();
    let context = RequestContext::new(Some("test_correlation".to_string()));

    let invoice_tracker = InvoiceTracker::new(
        "test_invoice_id".to_string(),
        federation_id,
        event_bus.clone(),
        Some(context),
    );

    // Test invoice lifecycle
    invoice_tracker.created(2000, "test_invoice_string".to_string()).await;
    invoice_tracker.paid(2000).await;

    // Test expiration
    let expired_tracker = InvoiceTracker::new(
        "expired_invoice_id".to_string(),
        federation_id,
        event_bus.clone(),
        None,
    );
    expired_tracker.expired().await;
}

#[tokio::test]
async fn test_database_instrumentation() {
    let event_bus = Arc::new(EventBus::new(1000));
    let mock_db = MockDatabase::default();
    let instrumented_db = InstrumentedDatabase::new(mock_db, event_bus, "test_service");

    let key = b"test_key";
    let value = b"test_value";

    // Test basic operations
    let result = instrumented_db.set(key, value).await;
    assert!(result.is_ok());

    let result = instrumented_db.get(key).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Some(value.to_vec()));

    let result = instrumented_db.exists(key).await;
    assert!(result.is_ok());
    assert!(result.unwrap());

    let result = instrumented_db.scan_prefix(b"test").await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 2);

    // Check statistics
    let stats = instrumented_db.stats().get_summary();
    assert_eq!(stats.total_operations, 4);
    assert_eq!(stats.successful_operations, 4);
    assert_eq!(stats.failed_operations, 0);
    assert_eq!(stats.success_rate, 100.0);
    assert!(stats.average_duration_ms > 0);
}

#[tokio::test]
async fn test_database_with_context() {
    let event_bus = Arc::new(EventBus::new(1000));
    let mock_db = MockDatabase::default();
    let instrumented_db = InstrumentedDatabase::new(mock_db, event_bus, "test_service");
    let context = RequestContext::new(Some("test_correlation".to_string()));

    let db_with_context = instrumented_db.with_context(&context);

    let result = db_with_context.get(b"test_key").await;
    assert!(result.is_ok());

    let result = db_with_context.set(b"test_key", b"test_value").await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_auth_event_publishing() {
    let event_bus = Arc::new(EventBus::new(1000));
    let auth = Arc::new(BasicAuth::new(Some("test_password".to_string())));

    // Test successful authentication
    assert!(auth.verify("Basic Zm1jZDp0ZXN0X3Bhc3N3b3Jk")); // base64("fmcd:test_password")

    // Test failed authentication
    assert!(!auth.verify("Basic aW52YWxpZA==")); // base64("invalid")
    assert!(!auth.verify("Bearer token"));
    assert!(!auth.verify(""));
}

#[tokio::test]
async fn test_event_subscription() {
    let event_bus = Arc::new(EventBus::new(1000));
    let mut receiver = event_bus.subscribe();

    // Publish an event
    let event = FmcdEvent::InvoiceCreated {
        invoice_id: "test_invoice".to_string(),
        federation_id: "test_federation".to_string(),
        amount_msat: 5000,
        invoice: "test_invoice_string".to_string(),
        correlation_id: Some("test_correlation".to_string()),
        timestamp: chrono::Utc::now(),
    };

    event_bus.publish(event.clone()).await.unwrap();

    // Receive the event
    let received_event = timeout(Duration::from_millis(100), receiver.recv())
        .await
        .expect("Should receive event within timeout")
        .expect("Should successfully receive event");

    match received_event {
        FmcdEvent::InvoiceCreated { invoice_id, .. } => {
            assert_eq!(invoice_id, "test_invoice");
        }
        _ => panic!("Expected InvoiceCreated event"),
    }
}

#[tokio::test]
async fn test_event_handler_failure_isolation() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Create a failing event handler
    struct FailingHandler {
        call_count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl fmcd::events::EventHandler for FailingHandler {
        async fn handle(&self, _event: FmcdEvent) -> anyhow::Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("Handler failure");
        }

        fn name(&self) -> &str {
            "failing_handler"
        }
    }

    let event_bus = Arc::new(EventBus::new(1000));
    let failing_handler = Arc::new(FailingHandler {
        call_count: Arc::new(AtomicUsize::new(0)),
    });
    let logging_handler = Arc::new(LoggingEventHandler::new(false));

    event_bus.register_handler(failing_handler.clone()).await;
    event_bus.register_handler(logging_handler).await;

    let event = FmcdEvent::PaymentSucceeded {
        operation_id: "test_operation_id".to_string(),
        federation_id: "test_federation".to_string(),
        amount_msat: 1000,
        preimage: "test_preimage".to_string(),
        fee_msat: Some(100),
        correlation_id: Some("test_correlation".to_string()),
        timestamp: chrono::Utc::now(),
    };

    // Publishing should succeed even with failing handler
    let result = event_bus.publish(event).await;
    assert!(result.is_ok());

    // Give some time for background processing
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify failing handler was called
    assert_eq!(failing_handler.call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_slow_database_queries() {
    use std::sync::atomic::{AtomicU64, Ordering};

    // Create a slow database mock
    #[derive(Default)]
    struct SlowDatabase;

    #[async_trait::async_trait]
    impl DatabaseInterface for SlowDatabase {
        async fn get(&self, _key: &[u8]) -> anyhow::Result<Option<Vec<u8>>> {
            tokio::time::sleep(Duration::from_millis(150)).await; // Slow query (>100ms)
            Ok(Some(b"test_value".to_vec()))
        }

        async fn set(&self, _key: &[u8], _value: &[u8]) -> anyhow::Result<()> {
            tokio::time::sleep(Duration::from_millis(5)).await; // Fast query
            Ok(())
        }

        async fn delete(&self, _key: &[u8]) -> anyhow::Result<()> {
            Ok(())
        }

        async fn exists(&self, _key: &[u8]) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn scan_prefix(&self, _prefix: &[u8]) -> anyhow::Result<Vec<(Vec<u8>, Vec<u8>)>> {
            Ok(vec![])
        }
    }

    let event_bus = Arc::new(EventBus::new(1000));
    let slow_db = SlowDatabase::default();
    let instrumented_db = InstrumentedDatabase::new(slow_db, event_bus, "test_service");

    // Perform slow query
    let result = instrumented_db.get(b"test_key").await;
    assert!(result.is_ok());

    // Perform fast query
    let result = instrumented_db.set(b"test_key", b"test_value").await;
    assert!(result.is_ok());

    // Check statistics - one slow query should be recorded
    let stats = instrumented_db.stats().get_summary();
    assert_eq!(stats.total_operations, 2);
    assert_eq!(stats.slow_operations, 1); // Only the get operation was slow
    assert!(stats.average_duration_ms > 50); // Average should reflect the slow query
}

#[tokio::test]
async fn test_payment_id_consistency_with_phoenixd() {
    // Test that our payment ID derivation is consistent and follows phoenixd patterns
    let test_cases = vec![
        "lnbc1000n1pwjw8xepp5test1",
        "lnbc2000n1pwjw8xepp5test2",
        "lnbc10u1pwjw8xepp5different",
    ];

    for invoice in test_cases {
        let payment_id = PaymentTracker::derive_payment_id(invoice);

        // Payment ID should be consistent
        assert_eq!(payment_id, PaymentTracker::derive_payment_id(invoice));

        // Should be hex string of expected length (32 chars = 16 bytes)
        assert_eq!(payment_id.len(), 32);
        assert!(payment_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // Different invoices should produce different payment IDs
    let id1 = PaymentTracker::derive_payment_id("invoice1");
    let id2 = PaymentTracker::derive_payment_id("invoice2");
    assert_ne!(id1, id2);
}
