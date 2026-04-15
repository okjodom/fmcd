use anyhow::anyhow;
use fedimint_client::ClientHandleArc;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_core::secp256k1::PublicKey;
use fedimint_core::util::SafeUrl;
use fedimint_core::Amount;
use fedimint_ln_client::{InternalPayState, LightningClientModule, LnPayState, PayType};
use fedimint_ln_common::lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescription, Description};
use fedimint_lnv2_client::{
    LightningClientModule as LightningClientModuleV2, LightningOperationMeta,
    ReceiveOperationState, SendOperationState,
};
use fedimint_lnv2_common::Bolt11InvoiceDescription as Bolt11InvoiceDescriptionV2;
use futures_util::StreamExt;
use serde_json::Value;
use tracing::{error, warn};

use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightningProtocol {
    Lnv1,
    Lnv2,
}

impl LightningProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lnv1 => "lnv1",
            Self::Lnv2 => "lnv2",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LightningInvoiceResult {
    pub protocol: LightningProtocol,
    pub operation_id: OperationId,
    pub invoice: Bolt11Invoice,
}

#[derive(Debug, Clone)]
pub struct LightningPayResult {
    pub protocol: LightningProtocol,
    pub operation_id: OperationId,
    pub contract_id: String,
    pub fee: Amount,
    pub preimage: String,
    pub payment_type: PayType,
}

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
        gateway_id: Option<PublicKey>,
        amount_msat: Amount,
        description: &str,
        expiry_time: Option<u64>,
        metadata: Value,
    ) -> Result<LightningInvoiceResult, AppError> {
        if let Ok(lightning_module_v2) = client.get_first_module::<LightningClientModuleV2>() {
            return self
                .create_invoice_lnv2(
                    &lightning_module_v2,
                    federation_id,
                    gateway_id,
                    amount_msat,
                    description,
                    expiry_time,
                    metadata,
                )
                .await;
        }

        self.create_invoice_lnv1(
            client,
            federation_id,
            gateway_id,
            amount_msat,
            description,
            expiry_time,
            metadata,
        )
        .await
    }

    async fn create_invoice_lnv1(
        &self,
        client: &ClientHandleArc,
        federation_id: FederationId,
        gateway_id: Option<PublicKey>,
        amount_msat: Amount,
        description: &str,
        expiry_time: Option<u64>,
        metadata: Value,
    ) -> Result<LightningInvoiceResult, AppError> {
        let gateway_id = gateway_id.ok_or_else(|| {
            AppError::validation_error(
                "gateway_id is required for legacy lightning federations without lnv2 support",
            )
        })?;

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

        Ok(LightningInvoiceResult {
            protocol: LightningProtocol::Lnv1,
            operation_id,
            invoice,
        })
    }

    async fn create_invoice_lnv2(
        &self,
        lightning_module: &LightningClientModuleV2,
        federation_id: FederationId,
        gateway_id: Option<PublicKey>,
        amount_msat: Amount,
        description: &str,
        expiry_time: Option<u64>,
        metadata: Value,
    ) -> Result<LightningInvoiceResult, AppError> {
        if gateway_id.is_some() {
            warn!(
                federation_id = %federation_id,
                "Ignoring legacy gateway_id selection for lnv2 invoice creation"
            );
        }

        let expiry_secs = expiry_time.unwrap_or(3600).min(u64::from(u32::MAX)) as u32;
        let (invoice, operation_id) = lightning_module
            .receive(
                amount_msat,
                expiry_secs,
                Bolt11InvoiceDescriptionV2::Direct(description.to_owned()),
                Option::<SafeUrl>::None,
                metadata,
            )
            .await
            .map_err(|e| {
                error!(
                    federation_id = %federation_id,
                    amount_msat = %amount_msat.msats,
                    error = ?e,
                    "Failed to create fedimint lnv2 invoice"
                );
                AppError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    anyhow!("Failed to create lnv2 invoice: {}", e),
                )
            })?;

        Ok(LightningInvoiceResult {
            protocol: LightningProtocol::Lnv2,
            operation_id,
            invoice,
        })
    }

    pub async fn pay_invoice(
        &self,
        client: &ClientHandleArc,
        federation_id: FederationId,
        gateway_id: Option<PublicKey>,
        bolt11: Bolt11Invoice,
        amount_msat: Option<Amount>,
    ) -> Result<LightningPayResult, AppError> {
        if let Ok(lightning_module_v2) = client.get_first_module::<LightningClientModuleV2>() {
            return self
                .pay_invoice_lnv2(
                    client,
                    &lightning_module_v2,
                    federation_id,
                    gateway_id,
                    bolt11,
                    amount_msat,
                )
                .await;
        }

        self.pay_invoice_lnv1(client, federation_id, gateway_id, bolt11, amount_msat)
            .await
    }

    async fn pay_invoice_lnv1(
        &self,
        client: &ClientHandleArc,
        federation_id: FederationId,
        gateway_id: Option<PublicKey>,
        bolt11: Bolt11Invoice,
        amount_msat: Option<Amount>,
    ) -> Result<LightningPayResult, AppError> {
        let gateway_id = gateway_id.ok_or_else(|| {
            AppError::validation_error(
                "gateway_id is required for legacy lightning federations without lnv2 support",
            )
        })?;

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

        let payment = lightning_module
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
            })?;

        let operation_id = match payment.payment_type {
            PayType::Internal(operation_id) | PayType::Lightning(operation_id) => operation_id,
        };
        let preimage = self
            .await_lnv1_payment_preimage(&lightning_module, payment.payment_type)
            .await?;

        Ok(LightningPayResult {
            protocol: LightningProtocol::Lnv1,
            operation_id,
            contract_id: payment.contract_id.to_string(),
            fee: payment.fee,
            preimage,
            payment_type: payment.payment_type,
        })
    }

    async fn pay_invoice_lnv2(
        &self,
        client: &ClientHandleArc,
        lightning_module: &LightningClientModuleV2,
        federation_id: FederationId,
        gateway_id: Option<PublicKey>,
        bolt11: Bolt11Invoice,
        _amount_msat: Option<Amount>,
    ) -> Result<LightningPayResult, AppError> {
        if gateway_id.is_some() {
            warn!(
                federation_id = %federation_id,
                "Ignoring legacy gateway_id selection for lnv2 payments"
            );
        }

        let operation_id = lightning_module
            .send(bolt11, Option::<SafeUrl>::None, Value::Null)
            .await
            .map_err(|e| {
                error!(
                    federation_id = %federation_id,
                    error = ?e,
                    "Failed to start lnv2 payment"
                );
                AppError::with_category(
                    crate::error::ErrorCategory::PaymentTimeout,
                    format!("Payment failed: {}", e),
                )
            })?;

        let operation = client
            .operation_log()
            .get_operation(operation_id)
            .await
            .ok_or_else(|| {
                AppError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    anyhow!("Failed to load lnv2 operation metadata"),
                )
            })?;

        let meta = operation.meta::<LightningOperationMeta>();
        let (contract_id, fee) = match meta {
            LightningOperationMeta::Send(meta) => (
                meta.contract.contract_id().0.to_string(),
                meta.gateway_fee(),
            ),
            _ => {
                return Err(AppError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    anyhow!("Unexpected lnv2 payment operation metadata"),
                ));
            }
        };

        let mut updates = lightning_module
            .subscribe_send_operation_state_updates(operation_id)
            .await
            .map_err(|e| {
                AppError::validation_error(format!("Failed to subscribe to lnv2 payment: {}", e))
            })?
            .into_stream();

        let preimage = loop {
            match updates.next().await {
                Some(SendOperationState::Success(preimage)) => break hex::encode(preimage),
                Some(SendOperationState::Refunded) => {
                    return Err(AppError::validation_error("Payment was refunded"));
                }
                Some(SendOperationState::Failure) => {
                    return Err(AppError::validation_error("Payment failed"));
                }
                Some(SendOperationState::Funding)
                | Some(SendOperationState::Funded)
                | Some(SendOperationState::Refunding) => continue,
                None => {
                    return Err(AppError::validation_error(
                        "Payment completed but no preimage returned",
                    ));
                }
            }
        };

        Ok(LightningPayResult {
            protocol: LightningProtocol::Lnv2,
            operation_id,
            contract_id,
            fee,
            preimage,
            payment_type: PayType::Lightning(operation_id),
        })
    }

    async fn await_lnv1_payment_preimage(
        &self,
        lightning_module: &LightningClientModule,
        payment_type: PayType,
    ) -> Result<String, AppError> {
        match payment_type {
            PayType::Internal(operation_id) => {
                let mut updates = lightning_module
                    .subscribe_internal_pay(operation_id)
                    .await
                    .map_err(|e| {
                        AppError::validation_error(format!(
                            "Failed to subscribe to internal payment: {}",
                            e
                        ))
                    })?
                    .into_stream();

                while let Some(update) = updates.next().await {
                    match update {
                        InternalPayState::Preimage(preimage) => {
                            return Ok(hex::encode(preimage.0));
                        }
                        InternalPayState::RefundSuccess { .. } => {
                            return Err(AppError::validation_error(
                                "Internal payment failed and was refunded",
                            ));
                        }
                        InternalPayState::UnexpectedError(error) => {
                            return Err(AppError::validation_error(format!(
                                "Internal payment failed: {}",
                                error
                            )));
                        }
                        InternalPayState::RefundError {
                            error_message,
                            error,
                        } => {
                            return Err(AppError::validation_error(format!(
                                "Internal payment refund failed: {} {}",
                                error_message, error
                            )));
                        }
                        InternalPayState::FundingFailed { error } => {
                            return Err(AppError::validation_error(format!(
                                "Internal payment funding failed: {}",
                                error
                            )));
                        }
                        InternalPayState::Funding => {}
                    }
                }
            }
            PayType::Lightning(operation_id) => {
                let mut updates = lightning_module
                    .subscribe_ln_pay(operation_id)
                    .await
                    .map_err(|e| {
                        AppError::validation_error(format!(
                            "Failed to subscribe to lightning payment: {}",
                            e
                        ))
                    })?
                    .into_stream();

                while let Some(update) = updates.next().await {
                    match update {
                        LnPayState::Success { preimage } => return Ok(preimage),
                        LnPayState::Refunded { .. } => {
                            return Err(AppError::validation_error("Payment was refunded"));
                        }
                        LnPayState::Canceled => {
                            return Err(AppError::validation_error("Payment was canceled"));
                        }
                        LnPayState::UnexpectedError { error_message } => {
                            return Err(AppError::validation_error(format!(
                                "Payment failed: {}",
                                error_message
                            )));
                        }
                        LnPayState::Created
                        | LnPayState::AwaitingChange
                        | LnPayState::WaitingForRefund { .. }
                        | LnPayState::Funded { .. } => {}
                    }
                }
            }
        }

        Err(AppError::validation_error(
            "Payment completed but no preimage was returned",
        ))
    }

    pub async fn await_invoice_claimed_lnv2(
        &self,
        client: &ClientHandleArc,
        operation_id: OperationId,
    ) -> Result<ReceiveOperationState, AppError> {
        let lightning_module = client
            .get_first_module::<LightningClientModuleV2>()
            .map_err(|e| {
                AppError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    anyhow!("Failed to get lnv2 module: {}", e),
                )
            })?;

        let state = lightning_module
            .await_final_receive_operation_state(operation_id)
            .await
            .map_err(|e| {
                AppError::validation_error(format!("Failed to await lnv2 invoice: {}", e))
            })?;

        Ok(match state {
            fedimint_lnv2_client::FinalReceiveOperationState::Expired => {
                ReceiveOperationState::Expired
            }
            fedimint_lnv2_client::FinalReceiveOperationState::Claimed => {
                ReceiveOperationState::Claimed
            }
            fedimint_lnv2_client::FinalReceiveOperationState::Failure => {
                ReceiveOperationState::Failure
            }
        })
    }
}
