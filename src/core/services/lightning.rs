use anyhow::anyhow;
use fedimint_client::ClientHandleArc;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_core::secp256k1::PublicKey;
use fedimint_core::Amount;
use fedimint_ln_client::{LightningClientModule, OutgoingLightningPayment};
use fedimint_ln_common::lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescription, Description};
use serde_json::Value;
use tracing::error;

use crate::error::AppError;

#[derive(Debug, Default)]
pub struct LightningService;

impl LightningService {
    pub fn new() -> Self {
        Self
    }

    pub async fn create_invoice(
        &self,
        client: &ClientHandleArc,
        federation_id: FederationId,
        gateway_id: PublicKey,
        amount_msat: Amount,
        description: &str,
        expiry_time: Option<u64>,
        metadata: Value,
    ) -> Result<(OperationId, Bolt11Invoice), AppError> {
        let lightning_module = client
            .get_first_module::<LightningClientModule>()
            .map_err(|e| {
                error!(
                    federation_id = %federation_id,
                    error = ?e,
                    "Failed to get Lightning module from fedimint client"
                );
                AppError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    anyhow!("Failed to get Lightning module: {}", e),
                )
            })?;

        let gateway = lightning_module
            .select_gateway(&gateway_id)
            .await
            .ok_or_else(|| {
                error!(
                    gateway_id = %gateway_id,
                    federation_id = %federation_id,
                    "Failed to select gateway - gateway may be offline or not registered"
                );
                AppError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    anyhow!(
                        "Failed to select gateway with ID {}. Gateway may be offline or not registered with this federation.",
                        gateway_id
                    ),
                )
            })?;

        let description = Description::new(description.to_owned()).map_err(|e| {
            error!(
                federation_id = %federation_id,
                description = description,
                error = ?e,
                "Invalid invoice description"
            );
            AppError::new(
                axum::http::StatusCode::BAD_REQUEST,
                anyhow!("Invalid invoice description: {}", e),
            )
        })?;

        let (operation_id, invoice, _) = lightning_module
            .create_bolt11_invoice(
                amount_msat,
                Bolt11InvoiceDescription::Direct(description),
                expiry_time,
                metadata,
                Some(gateway),
            )
            .await
            .map_err(|e| {
                error!(
                    federation_id = %federation_id,
                    amount_msat = %amount_msat.msats,
                    error = ?e,
                    "Failed to create fedimint invoice"
                );
                AppError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    anyhow!("Failed to create invoice: {}", e),
                )
            })?;

        Ok((operation_id, invoice))
    }

    pub async fn pay_invoice(
        &self,
        client: &ClientHandleArc,
        federation_id: FederationId,
        gateway_id: PublicKey,
        bolt11: Bolt11Invoice,
        amount_msat: Option<Amount>,
    ) -> Result<OutgoingLightningPayment, AppError> {
        let lightning_module = client
            .get_first_module::<LightningClientModule>()
            .map_err(|e| {
                let error_msg = "Lightning module not available".to_string();
                error!(
                    error = ?e,
                    federation_id = %federation_id,
                    "Lightning module not available"
                );
                AppError::with_category(crate::error::ErrorCategory::PaymentTimeout, error_msg)
            })?;

        let gateway = lightning_module
            .select_gateway(&gateway_id)
            .await
            .ok_or_else(|| {
                let error_msg = format!("Gateway {} not available", gateway_id);
                error!(
                    gateway_id = %gateway_id,
                    federation_id = %federation_id,
                    "Gateway not available"
                );
                AppError::with_category(crate::error::ErrorCategory::GatewayError, error_msg)
            })?;

        lightning_module
            .pay_bolt11_invoice(Some(gateway), bolt11, amount_msat)
            .await
            .map_err(|e| {
                let error_msg = format!("Payment failed: {}", e);
                error!(
                    error = ?e,
                    federation_id = %federation_id,
                    "Payment failed during execution"
                );
                AppError::with_category(crate::error::ErrorCategory::PaymentTimeout, error_msg)
            })
    }
}
