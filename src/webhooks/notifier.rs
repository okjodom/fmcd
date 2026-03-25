use std::collections::HashSet;
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use hex;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::events::{EventHandler, FmcdEvent};

/// Configuration for webhook retry behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_attempts: u32,
    /// Initial delay between retries in milliseconds
    pub initial_delay_ms: u64,
    /// Maximum delay between retries in milliseconds
    pub max_delay_ms: u64,
    /// Multiplier for exponential backoff
    pub backoff_multiplier: f64,
    /// Timeout for each webhook request in seconds
    pub timeout_secs: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay_ms: 1000, // 1 second
            max_delay_ms: 30000,    // 30 seconds
            backoff_multiplier: 2.0,
            timeout_secs: 30,
        }
    }
}

/// Configuration for a single webhook endpoint
#[derive(Clone, Serialize, Deserialize)]
pub struct WebhookEndpoint {
    /// Unique identifier for the endpoint
    pub id: String,
    /// The webhook URL to send events to
    pub url: String,
    /// Optional secret for HMAC-SHA256 signature generation
    pub secret: Option<String>,
    /// List of event types this endpoint should receive
    pub events: Vec<String>,
    /// Retry configuration for this endpoint
    pub retry_config: RetryConfig,
    /// Whether this endpoint is active
    pub enabled: bool,
    /// Optional description of the endpoint
    pub description: Option<String>,
}

impl fmt::Debug for WebhookEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebhookEndpoint")
            .field("id", &self.id)
            .field("url", &self.url)
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .field("events", &self.events)
            .field("retry_config", &self.retry_config)
            .field("enabled", &self.enabled)
            .field("description", &self.description)
            .finish()
    }
}

impl WebhookEndpoint {
    pub fn new(id: String, url: String) -> anyhow::Result<Self> {
        // Validate URL to prevent SSRF attacks
        Self::validate_webhook_url(&url)?;

        Ok(Self {
            id,
            url,
            secret: None,
            events: Vec::new(),
            retry_config: RetryConfig::default(),
            enabled: true,
            description: None,
        })
    }

    /// Validate webhook URL to prevent SSRF attacks
    fn validate_webhook_url(url: &str) -> anyhow::Result<()> {
        let parsed_url =
            reqwest::Url::parse(url).map_err(|e| anyhow::anyhow!("Invalid URL format: {}", e))?;

        // Only allow HTTP/HTTPS schemes
        match parsed_url.scheme() {
            "http" | "https" => {}
            scheme => return Err(anyhow::anyhow!("Unsupported URL scheme: {}", scheme)),
        }

        // In production, enforce HTTPS
        #[cfg(not(debug_assertions))]
        if parsed_url.scheme() != "https" {
            return Err(anyhow::anyhow!(
                "HTTPS required for webhook URLs in production"
            ));
        }

        // Get the host
        let host = parsed_url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("URL must have a host"))?;

        // Check if host is an IP address and validate it's not private
        if let Ok(ip) = IpAddr::from_str(host) {
            if Self::is_private_ip(ip) {
                return Err(anyhow::anyhow!(
                    "Webhook URLs cannot target private IP addresses"
                ));
            }
        } else {
            // Check for localhost variations
            let host_lower = host.to_lowercase();
            if host_lower == "localhost" || host_lower.ends_with(".localhost") {
                return Err(anyhow::anyhow!("Webhook URLs cannot target localhost"));
            }
        }

        // Validate port is not in dangerous ranges
        if let Some(port) = parsed_url.port() {
            if Self::is_dangerous_port(port) {
                return Err(anyhow::anyhow!(
                    "Webhook URLs cannot target dangerous ports"
                ));
            }
        }

        Ok(())
    }

    /// Check if an IP address is private/internal
    fn is_private_ip(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ipv4) => {
                // Private IPv4 ranges
                ipv4.is_private()
                    || ipv4.is_loopback()
                    || ipv4.is_link_local()
                    || ipv4.is_broadcast()
                    || ipv4.is_documentation()
                    // Additional dangerous ranges
                    || (ipv4.octets()[0] == 169 && ipv4.octets()[1] == 254) // Link-local
                    || (ipv4.octets()[0] == 224) // Multicast
            }
            IpAddr::V6(ipv6) => {
                // Private IPv6 ranges
                ipv6.is_loopback()
                    || ipv6.is_unspecified()
                    || ipv6.is_multicast()
                    // Private/local ranges
                    || (ipv6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 (Unique local)
                    || (ipv6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 (Link-local)
            }
        }
    }

    /// Check if a port is in a dangerous range
    fn is_dangerous_port(port: u16) -> bool {
        // Common dangerous ports that should not be accessible
        match port {
            // Well-known system ports
            22 | 23 | 25 | 53 | 110 | 143 | 993 | 995 => true,
            // Database ports
            1433 | 1521 | 3306 | 5432 | 6379 | 27017 => true,
            // Internal service ports
            8080..=8090 | 9000..=9999 => true,
            // Note: 80 and 443 are allowed for standard HTTP/HTTPS
            _ => false,
        }
    }

    pub fn with_secret(mut self, secret: String) -> anyhow::Result<Self> {
        // Validate secret for minimum length and entropy
        Self::validate_hmac_secret(&secret)?;
        self.secret = Some(secret);
        Ok(self)
    }

    /// Validate HMAC secret for cryptographic security
    fn validate_hmac_secret(secret: &str) -> anyhow::Result<()> {
        // Minimum length for HMAC-SHA256 security (32 bytes = 256 bits)
        const MIN_SECRET_LENGTH: usize = 32;

        if secret.len() < MIN_SECRET_LENGTH {
            return Err(anyhow::anyhow!(
                "HMAC secret must be at least {} characters long for cryptographic security",
                MIN_SECRET_LENGTH
            ));
        }

        // Check for sufficient entropy (variety of character types)
        let has_uppercase = secret.chars().any(|c| c.is_ascii_uppercase());
        let has_lowercase = secret.chars().any(|c| c.is_ascii_lowercase());
        let has_digit = secret.chars().any(|c| c.is_ascii_digit());
        let has_special = secret.chars().any(|c| !c.is_ascii_alphanumeric());

        let entropy_score = [has_uppercase, has_lowercase, has_digit, has_special]
            .iter()
            .filter(|&&x| x)
            .count();

        if entropy_score < 3 {
            return Err(anyhow::anyhow!(
                "HMAC secret must contain at least 3 of: uppercase, lowercase, digits, special characters"
            ));
        }

        // Check for common weak patterns
        if secret.chars().all(|c| c.is_ascii_digit()) {
            return Err(anyhow::anyhow!("HMAC secret cannot contain only digits"));
        }

        if secret.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(anyhow::anyhow!(
                "HMAC secret must contain non-alphabetic characters"
            ));
        }

        // Check for sequential patterns (simple check)
        let secret_bytes = secret.as_bytes();
        let mut sequential_count = 0;
        for i in 1..secret_bytes.len() {
            if secret_bytes[i] == secret_bytes[i - 1] + 1
                || secret_bytes[i] == secret_bytes[i - 1] - 1
                || secret_bytes[i] == secret_bytes[i - 1]
            {
                sequential_count += 1;
                if sequential_count > secret.len() / 3 {
                    return Err(anyhow::anyhow!(
                        "HMAC secret contains too many sequential or repeated characters"
                    ));
                }
            } else {
                sequential_count = 0;
            }
        }

        Ok(())
    }

    pub fn with_events(mut self, events: Vec<String>) -> Self {
        self.events = events;
        self
    }

    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    pub fn with_description(mut self, description: String) -> Self {
        self.description = Some(description);
        self
    }

    /// Check if this endpoint should receive the given event type
    pub fn should_receive_event(&self, event_type: &str) -> bool {
        self.enabled && (self.events.is_empty() || self.events.contains(&event_type.to_string()))
    }
}

/// Configuration for the webhook system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    /// List of webhook endpoints
    pub endpoints: Vec<WebhookEndpoint>,
    /// Whether webhook delivery is enabled globally
    pub enabled: bool,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            endpoints: Vec::new(),
            enabled: true,
        }
    }
}

/// Webhook notifier that implements the EventHandler trait
#[derive(Clone)]
pub struct WebhookNotifier {
    client: Client,
    config: WebhookConfig,
}

impl WebhookNotifier {
    /// Create a new webhook notifier with the given configuration
    pub fn new(config: WebhookConfig) -> anyhow::Result<Self> {
        let client = Client::builder().timeout(Duration::from_secs(30)).build()?;

        Ok(Self { client, config })
    }

    /// Update the webhook configuration
    pub fn update_config(&mut self, config: WebhookConfig) {
        self.config = config;
        info!(
            endpoint_count = self.config.endpoints.len(),
            enabled = self.config.enabled,
            "Webhook configuration updated"
        );
    }

    /// Get the current webhook configuration
    pub fn config(&self) -> &WebhookConfig {
        &self.config
    }

    /// Send webhook notification for an event to all matching endpoints
    pub async fn notify(&self, event: &FmcdEvent) -> anyhow::Result<()> {
        if !self.config.enabled {
            debug!("Webhook notifications disabled, skipping event");
            return Ok(());
        }

        let event_type = event.event_type();
        let event_id = event.event_id();
        let _timestamp = event.timestamp();

        debug!(
            event_id = %event_id,
            event_type = %event_type,
            endpoint_count = self.config.endpoints.len(),
            "Processing webhook notifications for event"
        );

        let payload = self.create_webhook_payload(event)?;
        let payload_str = serde_json::to_string(&payload)?;

        // Send to all matching endpoints concurrently
        let mut tasks = Vec::new();

        for endpoint in &self.config.endpoints {
            if endpoint.should_receive_event(event_type) {
                let client = self.client.clone();
                let endpoint = endpoint.clone();
                let payload_str = payload_str.clone();
                let event_id = event_id.clone();

                let task = tokio::spawn(async move {
                    Self::deliver_webhook(client, endpoint, payload_str, &event_id).await
                });

                tasks.push(task);
            }
        }

        // Wait for all webhook deliveries to complete
        let mut success_count = 0;
        let mut error_count = 0;

        for task in tasks {
            match task.await {
                Ok(Ok(())) => success_count += 1,
                Ok(Err(e)) => {
                    error_count += 1;
                    error!("Webhook delivery failed: {}", e);
                }
                Err(e) => {
                    error_count += 1;
                    error!("Webhook task panicked: {}", e);
                }
            }
        }

        info!(
            event_id = %event_id,
            event_type = %event_type,
            success_count = success_count,
            error_count = error_count,
            "Webhook notification processing completed"
        );

        Ok(())
    }

    /// Create the webhook payload for an event with sensitive data sanitized
    pub(crate) fn create_webhook_payload(
        &self,
        event: &FmcdEvent,
    ) -> anyhow::Result<serde_json::Value> {
        // First serialize the full event to JSON
        let event_json = serde_json::to_value(event)?;

        // Sanitize the event data to remove sensitive information
        let sanitized_data = self.sanitize_event_data(event_json)?;

        let payload = serde_json::json!({
            "id": event.event_id(),
            "type": event.event_type(),
            "timestamp": event.timestamp(),
            "correlation_id": event.correlation_id(),
            "data": sanitized_data
        });

        Ok(payload)
    }

    /// Sanitize event data to remove sensitive information
    fn sanitize_event_data(&self, mut value: Value) -> anyhow::Result<Value> {
        // Define sensitive fields that should be redacted
        let sensitive_fields: HashSet<&str> = [
            "preimage",
            "invoice", // Raw invoice strings may contain sensitive info
            "secret",
            "password",
            "token",
            "key",
            "private_key",
            "seed",
            "mnemonic",
            "invite_code", // Federation invite codes
            "ip_address",
            "client_ip",
            "user_agent",
            "x_forwarded_for",
            "authorization",
        ]
        .iter()
        .cloned()
        .collect();

        self.sanitize_value(&mut value, &sensitive_fields)?;
        Ok(value)
    }

    /// Recursively sanitize a JSON value by redacting sensitive fields
    fn sanitize_value(
        &self,
        value: &mut Value,
        sensitive_fields: &HashSet<&str>,
    ) -> anyhow::Result<()> {
        match value {
            Value::Object(map) => {
                for (key, val) in map.iter_mut() {
                    let key_lower = key.to_lowercase();

                    // Check if this field should be redacted
                    let should_redact = sensitive_fields
                        .iter()
                        .any(|&sensitive| key_lower.contains(sensitive) || key.contains(sensitive));

                    if should_redact {
                        *val = Value::String("[REDACTED]".to_string());
                    } else {
                        // Recursively sanitize nested objects/arrays
                        self.sanitize_value(val, sensitive_fields)?;
                    }
                }
            }
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    self.sanitize_value(item, sensitive_fields)?;
                }
            }
            _ => {
                // Primitive values don't need sanitization
            }
        }

        Ok(())
    }

    /// Deliver a webhook to a single endpoint with retry logic
    async fn deliver_webhook(
        client: Client,
        endpoint: WebhookEndpoint,
        payload: String,
        event_id: &str,
    ) -> anyhow::Result<()> {
        let mut attempt = 0;
        let mut delay_ms = endpoint.retry_config.initial_delay_ms;

        while attempt < endpoint.retry_config.max_attempts {
            attempt += 1;

            debug!(
                endpoint_id = %endpoint.id,
                event_id = %event_id,
                attempt = attempt,
                max_attempts = endpoint.retry_config.max_attempts,
                "Attempting webhook delivery"
            );

            match Self::send_webhook_request(&client, &endpoint, &payload, event_id).await {
                Ok(()) => {
                    info!(
                        endpoint_id = %endpoint.id,
                        event_id = %event_id,
                        attempt = attempt,
                        "Webhook delivered successfully"
                    );
                    return Ok(());
                }
                Err(e) => {
                    warn!(
                        endpoint_id = %endpoint.id,
                        event_id = %event_id,
                        attempt = attempt,
                        max_attempts = endpoint.retry_config.max_attempts,
                        error = %e,
                        "Webhook delivery attempt failed"
                    );

                    // If this isn't the last attempt, wait before retrying
                    if attempt < endpoint.retry_config.max_attempts {
                        sleep(Duration::from_millis(delay_ms)).await;

                        // Exponential backoff with cap
                        delay_ms = ((delay_ms as f64 * endpoint.retry_config.backoff_multiplier)
                            as u64)
                            .min(endpoint.retry_config.max_delay_ms);
                    }
                }
            }
        }

        error!(
            endpoint_id = %endpoint.id,
            event_id = %event_id,
            attempts = attempt,
            "Webhook delivery failed after all retry attempts"
        );

        Err(anyhow::anyhow!(
            "Webhook delivery failed after {} attempts",
            endpoint.retry_config.max_attempts
        ))
    }

    /// Send a single webhook HTTP request
    async fn send_webhook_request(
        client: &Client,
        endpoint: &WebhookEndpoint,
        payload: &str,
        event_id: &str,
    ) -> anyhow::Result<()> {
        let mut request_builder = client
            .post(&endpoint.url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "fmcd-webhook/1.0")
            .header("X-Event-Id", event_id)
            .timeout(Duration::from_secs(endpoint.retry_config.timeout_secs))
            .body(payload.to_string());

        // Add HMAC signature if secret is configured
        if let Some(secret) = &endpoint.secret {
            let signature = Self::calculate_hmac_signature(payload, secret)?;
            request_builder = request_builder.header("X-Signature-SHA256", signature);
        }

        let response = request_builder.send().await?;

        // Check if response indicates success
        if response.status().is_success() {
            debug!(
                endpoint_id = %endpoint.id,
                event_id = %event_id,
                status_code = %response.status(),
                "Webhook request completed successfully"
            );
            Ok(())
        } else {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            Err(anyhow::anyhow!(
                "Webhook request failed with status {}: {}",
                status,
                body
            ))
        }
    }

    /// Calculate HMAC-SHA256 signature for webhook payload
    pub(crate) fn calculate_hmac_signature(payload: &str, secret: &str) -> anyhow::Result<String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .map_err(|e| anyhow::anyhow!("Invalid HMAC key: {}", e))?;

        mac.update(payload.as_bytes());
        let result = mac.finalize();
        let signature = hex::encode(result.into_bytes());

        Ok(format!("sha256={}", signature))
    }

    /// Verify HMAC-SHA256 signature for incoming webhook validation
    pub fn verify_hmac_signature(payload: &str, signature: &str, secret: &str) -> bool {
        match Self::calculate_hmac_signature(payload, secret) {
            Ok(expected_signature) => {
                // Constant time comparison to prevent timing attacks
                signature == expected_signature
            }
            Err(_) => false,
        }
    }
}

#[async_trait]
impl EventHandler for WebhookNotifier {
    async fn handle(&self, event: FmcdEvent) -> anyhow::Result<()> {
        // Webhook notifications are non-critical - we don't want to block event
        // processing if webhook delivery fails, so we spawn this in the
        // background
        let notifier_clone = self.clone();

        tokio::spawn(async move {
            if let Err(e) = notifier_clone.notify(&event).await {
                error!(
                    event_id = %event.event_id(),
                    event_type = %event.event_type(),
                    error = %e,
                    "Failed to send webhook notifications"
                );
            }
        });

        Ok(())
    }

    fn name(&self) -> &str {
        "webhook_notifier"
    }

    fn is_critical(&self) -> bool {
        // Webhook delivery is not critical - it should not block event processing
        false
    }
}
