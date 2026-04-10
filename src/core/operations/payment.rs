use std::sync::Arc;

use chrono::{DateTime, Utc};
use fedimint_core::config::FederationId;
use sha2::{Digest, Sha256};
use tracing::{info_span, instrument, Span};

use crate::events::{EventBus, FmcdEvent};
use crate::observability::correlation::RequestContext;

#[derive(Debug, Clone, PartialEq)]
pub enum PaymentState {
    Initiated,
    GatewaySelected,
    Executing,
    Succeeded,
    Failed,
}

impl PaymentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PaymentState::Initiated => "initiated",
            PaymentState::GatewaySelected => "gateway_selected",
            PaymentState::Executing => "executing",
            PaymentState::Succeeded => "succeeded",
            PaymentState::Failed => "failed",
        }
    }
}

/// Tracks payment operations through their entire lifecycle
/// Derives payment IDs from invoice payment hashes
pub struct PaymentTracker {
    payment_id: String,
    federation_id: String,
    state: PaymentState,
    event_bus: Arc<EventBus>,
    span: Span,
    correlation_id: Option<String>,
    initiated_at: DateTime<Utc>,
}

impl PaymentTracker {
    /// Create a new payment tracker
    pub fn new(
        federation_id: FederationId,
        invoice: &str,
        amount_msat: u64,
        event_bus: Arc<EventBus>,
        context: Option<RequestContext>,
    ) -> Self {
        let payment_id = Self::derive_payment_id(invoice);
        let federation_id_str = federation_id.to_string();
        let correlation_id = context.as_ref().map(|c| c.correlation_id.clone());
        let initiated_at = Utc::now();

        let span = info_span!(
            "payment_operation",
            payment_id = %payment_id,
            federation_id = %federation_id_str,
            amount_msat = amount_msat,
            state = %PaymentState::Initiated.as_str(),
            correlation_id = ?correlation_id,
        );

        Self {
            payment_id,
            federation_id: federation_id_str,
            state: PaymentState::Initiated,
            event_bus,
            span,
            correlation_id,
            initiated_at,
        }
    }

    /// Derive payment ID from invoice payment hash
    /// Derives a payment ID from the invoice string by hashing it with SHA-256
    /// and taking the first 16 bytes of the hash, encoded as a hexadecimal
    /// string.
    ///
    /// # Format
    /// The resulting payment ID is a 32-character lowercase hexadecimal string,
    /// representing the first 16 bytes (128 bits) of the SHA-256 hash of the
    /// invoice.
    ///
    /// # Security Implications
    /// - Using only the first 16 bytes of the SHA-256 hash reduces the
    ///   collision resistance compared to the full hash, but 128 bits is
    ///   generally sufficient for uniqueness in practice for non-adversarial
    ///   settings.
    /// - This function is intended to generate unique, non-secret identifiers
    ///   for payment tracking, not for cryptographic authentication or as a
    ///   secret.
    /// - Do not use this function for purposes requiring strong collision
    ///   resistance or cryptographic security.
    pub fn derive_payment_id(invoice: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(invoice.as_bytes());
        let result = hasher.finalize();
        hex::encode(&result[..16]) // Use first 16 bytes for shorter ID
    }

    /// Get the payment ID
    pub fn payment_id(&self) -> &str {
        &self.payment_id
    }

    /// Get the federation ID
    pub fn federation_id(&self) -> &str {
        &self.federation_id
    }

    /// Get the current state
    pub fn state(&self) -> &PaymentState {
        &self.state
    }

    /// Get the tracing span for this payment
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Get the correlation ID
    pub fn correlation_id(&self) -> Option<&String> {
        self.correlation_id.as_ref()
    }

    /// Mark payment as initiated and publish event
    #[instrument(skip(self), fields(payment_id = %self.payment_id))]
    pub async fn initiate(&mut self, invoice: String, amount_msat: u64) {
        self.state = PaymentState::Initiated;
        self.span.record("state", self.state.as_str());

        let event = FmcdEvent::PaymentInitiated {
            payment_id: self.payment_id.clone(),
            federation_id: self.federation_id.clone(),
            amount_msat,
            invoice,
            correlation_id: self.correlation_id.clone(),
            timestamp: self.initiated_at,
        };

        if let Err(e) = self.event_bus.publish(event).await {
            tracing::error!(
                payment_id = %self.payment_id,
                error = ?e,
                "Failed to publish payment initiated event"
            );
        }
    }

    /// Record gateway selection
    #[instrument(skip(self), fields(payment_id = %self.payment_id))]
    pub async fn gateway_selected(&mut self, gateway_id: String) {
        self.state = PaymentState::GatewaySelected;
        self.span.record("state", self.state.as_str());
        self.span.record("gateway_id", &gateway_id);

        let event = FmcdEvent::GatewaySelected {
            gateway_id,
            federation_id: self.federation_id.clone(),
            payment_id: Some(self.payment_id.clone()),
            correlation_id: self.correlation_id.clone(),
            timestamp: Utc::now(),
        };

        if let Err(e) = self.event_bus.publish(event).await {
            tracing::error!(
                payment_id = %self.payment_id,
                error = ?e,
                "Failed to publish gateway selected event"
            );
        }
    }

    /// Mark payment as executing
    #[instrument(skip(self), fields(payment_id = %self.payment_id))]
    pub async fn start_execution(&mut self) {
        self.state = PaymentState::Executing;
        self.span.record("state", self.state.as_str());
    }

    /// Mark payment as succeeded and publish event
    #[instrument(skip(self), fields(payment_id = %self.payment_id))]
    pub async fn succeed(&mut self, preimage: String, amount_msat: u64, fee_msat: u64) {
        self.state = PaymentState::Succeeded;
        self.span.record("state", self.state.as_str());
        self.span.record("fee_msat", fee_msat);

        // Record the total duration
        let duration = Utc::now().signed_duration_since(self.initiated_at);
        self.span.record("duration_ms", duration.num_milliseconds());

        let event = FmcdEvent::PaymentSucceeded {
            operation_id: self.payment_id.clone(),
            federation_id: self.federation_id.clone(),
            amount_msat,
            fee_msat: Some(fee_msat),
            preimage,
            correlation_id: self.correlation_id.clone(),
            timestamp: Utc::now(),
        };

        if let Err(e) = self.event_bus.publish(event).await {
            tracing::error!(
                payment_id = %self.payment_id,
                error = ?e,
                "Failed to publish payment succeeded event"
            );
        }
    }

    /// Mark payment as failed and publish event
    #[instrument(skip(self), fields(payment_id = %self.payment_id))]
    pub async fn fail(&mut self, reason: String) {
        self.state = PaymentState::Failed;
        self.span.record("state", self.state.as_str());
        self.span.record("failure_reason", &reason);

        // Record the total duration
        let duration = Utc::now().signed_duration_since(self.initiated_at);
        self.span.record("duration_ms", duration.num_milliseconds());

        let event = FmcdEvent::PaymentFailed {
            payment_id: self.payment_id.clone(),
            federation_id: self.federation_id.clone(),
            reason,
            correlation_id: self.correlation_id.clone(),
            timestamp: Utc::now(),
        };

        if let Err(e) = self.event_bus.publish(event).await {
            tracing::error!(
                payment_id = %self.payment_id,
                error = ?e,
                "Failed to publish payment failed event"
            );
        }
    }

    /// Check if the payment is in a terminal state
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, PaymentState::Succeeded | PaymentState::Failed)
    }

    /// Get payment duration since initiation
    pub fn duration(&self) -> chrono::Duration {
        Utc::now().signed_duration_since(self.initiated_at)
    }
}

impl Drop for PaymentTracker {
    fn drop(&mut self) {
        // Ensure we don't leak unfinished payments
        if !self.is_terminal() {
            tracing::warn!(
                payment_id = %self.payment_id,
                state = %self.state.as_str(),
                "PaymentTracker dropped without reaching terminal state"
            );
        }
    }
}

/// Helper for tracking invoice operations
pub struct InvoiceTracker {
    invoice_id: String,
    federation_id: String,
    event_bus: Arc<EventBus>,
    correlation_id: Option<String>,
    created_at: DateTime<Utc>,
}

impl InvoiceTracker {
    /// Create a new invoice tracker
    pub fn new(
        invoice_id: String,
        federation_id: FederationId,
        event_bus: Arc<EventBus>,
        context: Option<RequestContext>,
    ) -> Self {
        let correlation_id = context.as_ref().map(|c| c.correlation_id.clone());

        Self {
            invoice_id,
            federation_id: federation_id.to_string(),
            event_bus,
            correlation_id,
            created_at: Utc::now(),
        }
    }

    /// Get the invoice ID
    pub fn invoice_id(&self) -> &str {
        &self.invoice_id
    }

    /// Get the federation ID
    pub fn federation_id(&self) -> &str {
        &self.federation_id
    }

    /// Get the correlation ID
    pub fn correlation_id(&self) -> Option<&String> {
        self.correlation_id.as_ref()
    }

    /// Record invoice creation
    #[instrument(skip(self), fields(invoice_id = %self.invoice_id))]
    pub async fn created(&self, amount_msat: u64, invoice: String) {
        let event = FmcdEvent::InvoiceCreated {
            invoice_id: self.invoice_id.clone(),
            federation_id: self.federation_id.clone(),
            amount_msat,
            invoice,
            correlation_id: self.correlation_id.clone(),
            timestamp: self.created_at,
        };

        if let Err(e) = self.event_bus.publish(event).await {
            tracing::error!(
                invoice_id = %self.invoice_id,
                error = ?e,
                "Failed to publish invoice created event"
            );
        }
    }

    /// Record invoice payment
    #[instrument(skip(self), fields(invoice_id = %self.invoice_id))]
    pub async fn paid(&self, amount_received_msat: u64) {
        let event = FmcdEvent::InvoicePaid {
            operation_id: self.invoice_id.clone(), // Using invoice_id as operation_id
            federation_id: self.federation_id.clone(),
            amount_msat: amount_received_msat,
            correlation_id: self.correlation_id.clone(),
            timestamp: Utc::now(),
        };

        if let Err(e) = self.event_bus.publish(event).await {
            tracing::error!(
                invoice_id = %self.invoice_id,
                error = ?e,
                "Failed to publish invoice paid event"
            );
        }
    }

    /// Record invoice expiration
    #[instrument(skip(self), fields(invoice_id = %self.invoice_id))]
    pub async fn expired(&self) {
        let event = FmcdEvent::InvoiceExpired {
            invoice_id: self.invoice_id.clone(),
            federation_id: self.federation_id.clone(),
            correlation_id: self.correlation_id.clone(),
            timestamp: Utc::now(),
        };

        if let Err(e) = self.event_bus.publish(event).await {
            tracing::error!(
                invoice_id = %self.invoice_id,
                error = ?e,
                "Failed to publish invoice expired event"
            );
        }
    }
}
