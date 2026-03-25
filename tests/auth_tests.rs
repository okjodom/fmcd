use fmcd::auth::hmac::{AuthenticatedMessage, WebSocketAuth};
use serde_json::json;

#[test]
fn test_websocket_auth_disabled() {
    let auth = WebSocketAuth::new(None);
    assert!(!auth.is_enabled());

    // Should always return true when disabled
    assert!(auth.verify_signature("test", 1234567890, "invalid"));
}

#[test]
fn test_websocket_auth_enabled() {
    let auth = WebSocketAuth::new(Some("testpassword".to_string()));
    assert!(auth.is_enabled());

    let timestamp = 1234567890;
    let message = "test message";

    let signature = auth.create_signature(message, timestamp).unwrap();
    assert!(auth.verify_signature(message, timestamp, &signature));

    // Wrong signature should fail
    assert!(!auth.verify_signature(message, timestamp, "wrongsig"));

    // Wrong timestamp should fail
    assert!(!auth.verify_signature(message, timestamp + 1, &signature));
}

#[test]
fn test_authenticated_message() {
    let auth = WebSocketAuth::new(Some("testpassword".to_string()));
    let payload = json!({"type": "test", "data": "hello"});

    let msg = AuthenticatedMessage::new(payload.clone(), &auth).unwrap();
    assert!(msg.verify(&auth));

    // Tampered payload should fail verification
    let mut tampered_msg = AuthenticatedMessage::new(payload, &auth).unwrap();
    tampered_msg.timestamp = msg.timestamp;
    tampered_msg.signature = msg.signature.clone();
    tampered_msg.payload = json!({"type": "tampered"});
    assert!(!tampered_msg.verify(&auth));
}
