use anyhow::bail;
use fedimint_client::ClientHandleArc;
use fedimint_core::Amount;
use fedimint_ln_client::{InternalPayState, LightningClientModule, LnPayState, PayType};
use futures_util::StreamExt;
use tracing::{debug, info};

use crate::core::LnPayResponse;

pub mod gateways;
pub mod invoice;
pub mod pay;
pub mod status;
pub mod stream;

pub async fn wait_for_ln_payment(
    client: &ClientHandleArc,
    payment_type: PayType,
    contract_id: String,
    return_on_funding: bool,
) -> anyhow::Result<Option<LnPayResponse>> {
    let lightning_module = client.get_first_module::<LightningClientModule>()?;
    match payment_type {
        PayType::Internal(operation_id) => {
            let mut updates = lightning_module
                .subscribe_internal_pay(operation_id)
                .await?
                .into_stream();

            while let Some(update) = updates.next().await {
                match update {
                    InternalPayState::Preimage(preimage) => {
                        return Ok(Some(LnPayResponse {
                            operation_id,
                            payment_type,
                            contract_id,
                            fee: Amount::ZERO,
                            preimage: hex::encode(preimage.0),
                            protocol: Some("lnv1".to_string()),
                        }));
                    }
                    InternalPayState::RefundSuccess { out_points, error } => {
                        let e = format!(
                            "Internal payment failed. A refund was issued to {:?} Error: {error}",
                            out_points
                        );
                        bail!("{e}");
                    }
                    InternalPayState::UnexpectedError(e) => {
                        bail!("{e}");
                    }
                    InternalPayState::Funding if return_on_funding => return Ok(None),
                    InternalPayState::Funding => {}
                    InternalPayState::RefundError {
                        error_message,
                        error,
                    } => bail!("RefundError: {error_message} {error}"),
                    InternalPayState::FundingFailed { error } => {
                        bail!("FundingFailed: {error}")
                    }
                }
                debug!("Payment state update received");
            }
        }
        PayType::Lightning(operation_id) => {
            let mut updates = lightning_module
                .subscribe_ln_pay(operation_id)
                .await?
                .into_stream();

            while let Some(update) = updates.next().await {
                let update_clone = update.clone();
                match update_clone {
                    LnPayState::Success { preimage } => {
                        return Ok(Some(LnPayResponse {
                            operation_id,
                            payment_type,
                            contract_id,
                            fee: Amount::ZERO,
                            preimage,
                            protocol: Some("lnv1".to_string()),
                        }));
                    }
                    LnPayState::Refunded { gateway_error } => {
                        info!("{gateway_error}");
                        Err(anyhow::anyhow!("Payment was refunded"))?;
                    }
                    LnPayState::Canceled => {
                        Err(anyhow::anyhow!("Payment was canceled"))?;
                    }
                    LnPayState::Funded { block_height: _ } if return_on_funding => return Ok(None),
                    LnPayState::Created
                    | LnPayState::AwaitingChange
                    | LnPayState::WaitingForRefund { .. }
                    | LnPayState::Funded { block_height: _ } => {}
                    LnPayState::UnexpectedError { error_message } => {
                        bail!("UnexpectedError: {error_message}")
                    }
                }
                debug!("Payment state update received");
            }
        }
    };
    bail!("Lightning Payment failed")
}
