#![allow(clippy::unwrap_used)]
use chrono::Utc;
use serde_json::json;

use crate::events::FmcdEvent;
use crate::webhooks::notifier::{RetryConfig, WebhookConfig, WebhookEndpoint, WebhookNotifier};

#[test]
fn test_webhook_endpoint_creation() {
    let endpoint = WebhookEndpoint::new(
        "test-endpoint".to_string(),
        "https://example.com/webhook".to_string(),
    )
    .expect("Failed to create webhook endpoint")
    .with_secret("MySecureSecret123!@#WithEnoughEntropyForHMAC".to_string())
    .expect("Failed to set secret")
    .with_events(vec![
        "payment_succeeded".to_string(),
        "invoice_created".to_string(),
    ])
    .with_description("Test webhook endpoint".to_string());

    assert_eq!(endpoint.id, "test-endpoint");
    assert_eq!(endpoint.url, "https://example.com/webhook");
    assert_eq!(
        endpoint.secret,
        Some("MySecureSecret123!@#WithEnoughEntropyForHMAC".to_string())
    );
    assert_eq!(endpoint.events.len(), 2);
    assert!(endpoint.should_receive_event("payment_succeeded"));
    assert!(endpoint.should_receive_event("invoice_created"));
    assert!(!endpoint.should_receive_event("payment_failed"));
}

#[test]
fn test_webhook_endpoint_event_filtering() {
    // Endpoint with specific events
    let endpoint_with_events = WebhookEndpoint::new(
        "filtered".to_string(),
        "https://example.com/webhook".to_string(),
    )
    .expect("Failed to create webhook endpoint")
    .with_events(vec!["payment_succeeded".to_string()]);

    assert!(endpoint_with_events.should_receive_event("payment_succeeded"));
    assert!(!endpoint_with_events.should_receive_event("payment_failed"));

    // Endpoint with no specific events (receives all)
    let endpoint_all_events =
        WebhookEndpoint::new("all".to_string(), "https://example.com/webhook".to_string())
            .expect("Failed to create webhook endpoint");

    assert!(endpoint_all_events.should_receive_event("payment_succeeded"));
    assert!(endpoint_all_events.should_receive_event("payment_failed"));
    assert!(endpoint_all_events.should_receive_event("any_event"));

    // Disabled endpoint
    let mut disabled_endpoint = WebhookEndpoint::new(
        "disabled".to_string(),
        "https://example.com/webhook".to_string(),
    )
    .expect("Failed to create webhook endpoint");
    disabled_endpoint.enabled = false;

    assert!(!disabled_endpoint.should_receive_event("payment_succeeded"));
}

#[test]
fn test_hmac_signature_calculation() {
    let payload = r#"{"test": "data"}"#;
    let secret = "my-secret-key";

    let signature = WebhookNotifier::calculate_hmac_signature(payload, secret)
        .expect("Failed to calculate signature");

    assert!(signature.starts_with("sha256="));
    assert_eq!(signature.len(), 71); // "sha256=" + 64 hex characters

    // Verify the signature
    assert!(WebhookNotifier::verify_hmac_signature(
        payload, &signature, secret
    ));

    // Verify incorrect signature fails
    assert!(!WebhookNotifier::verify_hmac_signature(
        payload,
        "sha256=invalid",
        secret
    ));

    // Verify different secret fails
    assert!(!WebhookNotifier::verify_hmac_signature(
        payload,
        &signature,
        "wrong-secret"
    ));
}

#[test]
fn test_retry_config_defaults() {
    let config = RetryConfig::default();
    assert_eq!(config.max_attempts, 3);
    assert_eq!(config.initial_delay_ms, 1000);
    assert_eq!(config.max_delay_ms, 30000);
    assert_eq!(config.backoff_multiplier, 2.0);
    assert_eq!(config.timeout_secs, 30);
}

#[tokio::test]
async fn test_webhook_notifier_creation() {
    let config = WebhookConfig::default();
    let notifier = WebhookNotifier::new(config).expect("Failed to create notifier");

    assert!(notifier.config().enabled);
    assert_eq!(notifier.config().endpoints.len(), 0);
}

#[test]
fn test_create_webhook_payload() {
    let config = WebhookConfig::default();
    let notifier = WebhookNotifier::new(config).expect("Failed to create notifier");

    let event = FmcdEvent::PaymentSucceeded {
        operation_id: "test-operation".to_string(),
        federation_id: "test-fed".to_string(),
        amount_msat: 1000,
        preimage: "test-preimage".to_string(),
        timestamp: Utc::now(),
    };

    let payload = notifier
        .create_webhook_payload(&event)
        .expect("Failed to create payload");

    assert_eq!(payload["type"], "payment_succeeded");
    assert!(payload["correlation_id"].is_null());
    assert!(payload["id"].is_string());
    assert!(payload["timestamp"].is_string());
    assert!(payload["data"].is_object());

    // Verify sensitive data is redacted
    if let Some(data) = payload["data"].as_object() {
        // The preimage should be redacted
        assert_eq!(data.get("preimage").unwrap(), "[REDACTED]");
        assert_eq!(data.get("operation_id").unwrap(), "test-operation");
    }
}

#[test]
fn test_url_validation_prevents_ssrf() {
    // Valid URLs should work
    assert!(WebhookEndpoint::new(
        "valid".to_string(),
        "https://example.com/webhook".to_string()
    )
    .is_ok());

    // Private IPs should be rejected
    assert!(WebhookEndpoint::new(
        "private".to_string(),
        "http://192.168.1.1/webhook".to_string()
    )
    .is_err());

    assert!(WebhookEndpoint::new(
        "private2".to_string(),
        "http://10.0.0.1/webhook".to_string()
    )
    .is_err());

    assert!(WebhookEndpoint::new(
        "localhost".to_string(),
        "http://localhost:8080/webhook".to_string()
    )
    .is_err());

    assert!(WebhookEndpoint::new(
        "loopback".to_string(),
        "http://127.0.0.1/webhook".to_string()
    )
    .is_err());

    // Invalid schemes should be rejected
    assert!(
        WebhookEndpoint::new("ftp".to_string(), "ftp://example.com/webhook".to_string()).is_err()
    );

    assert!(WebhookEndpoint::new("file".to_string(), "file:///etc/passwd".to_string()).is_err());
}

#[test]
fn test_sensitive_data_sanitization() {
    let config = WebhookConfig::default();
    let notifier = WebhookNotifier::new(config).expect("Failed to create notifier");

    // Test with an event containing sensitive data
    let event = FmcdEvent::PaymentSucceeded {
        operation_id: "test-operation".to_string(),
        federation_id: "test-fed".to_string(),
        amount_msat: 1000,
        preimage: "sensitive-preimage-data".to_string(),
        timestamp: Utc::now(),
    };

    let payload = notifier
        .create_webhook_payload(&event)
        .expect("Failed to create payload");

    // Verify that sensitive fields are redacted
    let data = payload["data"]
        .as_object()
        .expect("Data should be an object");
    assert_eq!(data.get("preimage").unwrap(), "[REDACTED]");

    // Non-sensitive fields should remain
    assert_eq!(data.get("operation_id").unwrap(), "test-operation");
    assert_eq!(data.get("federation_id").unwrap(), "test-fed");
    assert_eq!(data.get("amount_msat").unwrap(), 1000);
}

#[test]
fn test_debug_does_not_leak_secrets() {
    let endpoint = WebhookEndpoint::new(
        "test".to_string(),
        "https://example.com/webhook".to_string(),
    )
    .expect("Failed to create webhook endpoint")
    .with_secret("SuperSecretKey123!@#WithEnoughEntropyForHMAC".to_string())
    .expect("Failed to set secret");

    let debug_str = format!("{:?}", endpoint);
    assert!(!debug_str.contains("SuperSecretKey123!@#WithEnoughEntropyForHMAC"));
    assert!(debug_str.contains("[REDACTED]"));
}

#[test]
fn test_hmac_secret_validation() {
    let base_endpoint = WebhookEndpoint::new(
        "test".to_string(),
        "https://example.com/webhook".to_string(),
    )
    .expect("Failed to create webhook endpoint");

    // Test valid secret
    assert!(base_endpoint
        .clone()
        .with_secret("ValidSecret123!@#WithEnoughEntropyAndLength".to_string())
        .is_ok());

    // Test secret too short
    assert!(base_endpoint
        .clone()
        .with_secret("Short123!".to_string())
        .is_err());

    // Test secret with only digits
    assert!(base_endpoint
        .clone()
        .with_secret("12345678901234567890123456789012345678".to_string())
        .is_err());

    // Test secret with only letters
    assert!(base_endpoint
        .clone()
        .with_secret("abcdefghijklmnopqrstuvwxyzabcdefghijklmnop".to_string())
        .is_err());

    // Test secret with insufficient entropy (only 2 character types)
    assert!(base_endpoint
        .clone()
        .with_secret("abcdefghijklmnopqrstuvwxyz1234567890".to_string())
        .is_err());

    // Test secret with too many sequential characters
    assert!(base_endpoint
        .clone()
        .with_secret("abcdefghijklmnopqrstuvwxyzABCDEFGHI123!".to_string())
        .is_err());

    // Test secret with good entropy
    assert!(base_endpoint
        .clone()
        .with_secret("MySecure123!Random@WordsWithGoodEntropy#".to_string())
        .is_ok());
}
