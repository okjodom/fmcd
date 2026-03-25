use std::str::FromStr;

use anyhow::Context;
use async_trait::async_trait;
use fedimint_core::Amount;
use fedimint_ln_common::lightning_invoice::Bolt11Invoice;
use tracing::debug;

use crate::core::PaymentInfoResolver;
use crate::error::AppError;
use crate::observability::sanitize_invoice;

/// LNURL resolver implementation for the API layer
/// Handles LNURL and Lightning Address resolution to Bolt11 invoices
pub struct LnurlResolver {
    http_client: reqwest::Client,
}

impl LnurlResolver {
    pub fn new() -> Self {
        Self {
            http_client: reqwest::Client::new(),
        }
    }
}

impl Default for LnurlResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PaymentInfoResolver for LnurlResolver {
    async fn resolve_payment_info(
        &self,
        payment_info: &str,
        amount_msat: Option<Amount>,
        lnurl_comment: Option<&str>,
    ) -> Result<Option<String>, AppError> {
        let info = payment_info.trim();

        // First check if it's already a Bolt11 invoice
        if let Ok(invoice) = Bolt11Invoice::from_str(info) {
            debug!(
                "Payment info is already a bolt11 invoice: {}",
                sanitize_invoice(&invoice)
            );

            // Validate amount constraints
            match (invoice.amount_milli_satoshis(), amount_msat) {
                (Some(_), Some(_)) => {
                    return Err(AppError::validation_error(
                        "Amount specified in both invoice and request",
                    ));
                }
                (None, _) => {
                    return Err(AppError::validation_error(
                        "Invoices without amounts are not supported",
                    ));
                }
                _ => {}
            }

            // Return None to indicate no resolution needed - use original payment_info
            return Ok(None);
        }

        // Try to parse as LNURL or Lightning Address
        let lnurl = if info.to_lowercase().starts_with("lnurl") {
            lnurl::lnurl::LnUrl::from_str(info)
                .map_err(|e| AppError::validation_error(format!("Invalid LNURL: {}", e)))?
        } else if info.contains('@') {
            lnurl::lightning_address::LightningAddress::from_str(info)
                .map_err(|e| {
                    AppError::validation_error(format!("Invalid Lightning Address: {}", e))
                })?
                .lnurl()
        } else {
            // Not LNURL or Lightning Address, return None to try as Bolt11
            return Ok(None);
        };

        debug!("Parsed payment info as LNURL: {:?}", lnurl);

        let amount = amount_msat
            .context("Amount must be specified when using LNURL or Lightning Address")
            .map_err(|e| AppError::validation_error(e.to_string()))?;

        // Create LNURL client
        let async_client = lnurl::AsyncClient::from_client(self.http_client.clone());

        // Make LNURL request
        let response = async_client
            .make_request(&lnurl.url)
            .await
            .map_err(|e| AppError::gateway_error(format!("LNURL request failed: {}", e)))?;

        match response {
            lnurl::LnUrlResponse::LnUrlPayResponse(pay_response) => {
                // Get the invoice from the LNURL service
                let invoice_response = async_client
                    .get_invoice(&pay_response, amount.msats, None, lnurl_comment)
                    .await
                    .map_err(|e| {
                        AppError::gateway_error(format!("Failed to get invoice from LNURL: {}", e))
                    })?;

                // Validate the returned invoice
                let invoice = Bolt11Invoice::from_str(invoice_response.invoice()).map_err(|e| {
                    AppError::validation_error(format!("Invalid invoice from LNURL: {}", e))
                })?;

                // Verify amount matches
                if invoice.amount_milli_satoshis() != Some(amount.msats) {
                    return Err(AppError::validation_error(format!(
                        "LNURL returned invoice with wrong amount. Expected {} msat, got {:?}",
                        amount.msats,
                        invoice.amount_milli_satoshis()
                    )));
                }

                Ok(Some(invoice.to_string()))
            }
            other => Err(AppError::validation_error(format!(
                "Unexpected LNURL response type: {:?}",
                other
            ))),
        }
    }
}
