use anyhow::anyhow;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use fedimint_client::ClientHandleArc;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_ln_client::{LightningClientModule, LnReceiveState};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{error, info, instrument, warn};

use crate::core::{InvoiceStatus, SettlementInfo};
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusQuery {
    pub federation_id: FederationId,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub invoice_id: Option<String>,
    pub operation_id: OperationId,
    pub status: InvoiceStatus,
    pub settlement: Option<SettlementInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracked_status: Option<String>,
    pub last_updated: chrono::DateTime<Utc>,
}

/// Unified status endpoint that supports both invoice_id and operation_id
/// lookup
#[instrument(
    skip(state, client),
    fields(
        operation_id = ?operation_id,
        federation_id = %query.federation_id,
        status = tracing::field::Empty,
    )
)]
async fn _get_status(
    state: &AppState,
    client: ClientHandleArc,
    operation_id: OperationId,
    query: StatusQuery,
) -> Result<StatusResponse, AppError> {
    let span = tracing::Span::current();
    let lightning_module = client.get_first_module::<LightningClientModule>()?;

    // Try to get the invoice amount from operation metadata
    let invoice_amount_msat = client
        .operation_log()
        .get_operation(operation_id)
        .await
        .and_then(|op| {
            // Extract amount from operation metadata if available
            op.meta::<serde_json::Value>()
                .get("amount")
                .and_then(|v| v.as_u64())
        })
        .unwrap_or(0); // Default to 0 if not found

    // Use fedimint's native subscribe_ln_receive to get current state
    let current_state = match lightning_module.subscribe_ln_receive(operation_id).await {
        Ok(stream) => {
            // Get the current state from the stream - this is fedimint's native approach
            let mut stream = stream.into_stream();
            match stream.next().await {
                Some(state) => state,
                None => {
                    // If no state is available, try to determine if operation exists
                    return Err(AppError::new(
                        StatusCode::NOT_FOUND,
                        anyhow!("Invoice operation not found or monitoring stream unavailable"),
                    ));
                }
            }
        }
        Err(e) => {
            error!(
                operation_id = ?operation_id,
                federation_id = %query.federation_id,
                error = ?e,
                "Failed to get invoice status from fedimint native client"
            );
            return Err(AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                anyhow!("Failed to get invoice status: {}", e),
            ));
        }
    };

    let tracked_operation = state.core.get_tracked_operation(&operation_id).await;
    let last_updated = tracked_operation
        .as_ref()
        .map(|operation| operation.updated_at)
        .unwrap_or_else(Utc::now);
    let (status, settlement) = match current_state {
        LnReceiveState::Created => (InvoiceStatus::Created, None),
        LnReceiveState::WaitingForPayment { .. } => (InvoiceStatus::Pending, None),
        LnReceiveState::Claimed => {
            // NOTE: Fedimint's LnReceiveState::Claimed doesn't include settlement details
            // The actual amount received might differ from invoice amount due to fees.
            // Using the invoice amount from operation metadata as a reasonable
            // approximation. This ensures API consumers receive meaningful data
            // rather than 0.
            let settlement_info = SettlementInfo {
                amount_received_msat: if invoice_amount_msat > 0 {
                    invoice_amount_msat
                } else {
                    // Log warning if amount is not available
                    warn!(
                        operation_id = ?operation_id,
                        "Invoice amount not found in operation metadata, using 0"
                    );
                    0
                },
                settled_at: last_updated, // Using current time as approximation
                preimage: None,           // Not exposed in current API
                gateway_fee_msat: tracked_operation.as_ref().and_then(|op| op.fee_msat),
            };
            (
                InvoiceStatus::Claimed {
                    amount_received_msat: settlement_info.amount_received_msat,
                    settled_at: settlement_info.settled_at,
                },
                Some(settlement_info),
            )
        }
        LnReceiveState::Canceled { reason } => (
            InvoiceStatus::Canceled {
                reason: reason.to_string(),
                canceled_at: last_updated,
            },
            None,
        ),
        LnReceiveState::Funded => (InvoiceStatus::Pending, None),
        LnReceiveState::AwaitingFunds => (InvoiceStatus::Pending, None),
        // Note: All LnReceiveState variants are now explicitly handled
        // If new variants are added to fedimint, compilation will fail here
    };

    span.record("status", format!("{:?}", status));

    info!(
        operation_id = ?operation_id,
        federation_id = %query.federation_id,
        status = ?status,
        "Retrieved invoice status using fedimint native behavior"
    );

    Ok(StatusResponse {
        invoice_id: None, // Invoice ID is provided separately when looked up by invoice_id
        operation_id,
        status,
        settlement,
        tracked_status: tracked_operation.map(|operation| format!("{:?}", operation.status)),
        last_updated,
    })
}

pub async fn handle_ws(state: AppState, v: Value) -> Result<Value, AppError> {
    #[derive(Deserialize)]
    struct WSRequest {
        operation_id: OperationId,
        federation_id: FederationId,
    }

    let req = serde_json::from_value::<WSRequest>(v)
        .map_err(|e| AppError::new(StatusCode::BAD_REQUEST, anyhow!("Invalid request: {}", e)))?;

    let client = state.get_client(req.federation_id).await?;
    let query = StatusQuery {
        federation_id: req.federation_id,
    };
    let status = _get_status(&state, client, req.operation_id, query).await?;
    Ok(json!(status))
}

/// REST endpoint for status query by operation ID
#[axum_macros::debug_handler]
pub async fn handle_rest_by_operation_id(
    State(state): State<AppState>,
    Path(operation_id_str): Path<String>,
    Query(query): Query<StatusQuery>,
) -> Result<Json<StatusResponse>, AppError> {
    let operation_id = operation_id_str.parse::<OperationId>().map_err(|e| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            anyhow!("Invalid operation ID: {}", e),
        )
    })?;

    let client = state.get_client(query.federation_id).await?;
    let status = _get_status(&state, client, operation_id, query).await?;
    Ok(Json(status))
}

/// Bulk status query for multiple invoices
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkStatusRequest {
    pub federation_id: FederationId,
    pub operation_ids: Vec<OperationId>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkStatusResponse {
    pub statuses: Vec<BulkStatusItem>,
    pub errors: Vec<BulkStatusError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkStatusItem {
    pub operation_id: OperationId,
    pub status: InvoiceStatus,
    pub settlement: Option<SettlementInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracked_status: Option<String>,
    pub last_updated: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkStatusError {
    pub operation_id: OperationId,
    pub error: String,
}

/// Bulk status endpoint for querying multiple invoices efficiently
#[axum_macros::debug_handler]
pub async fn handle_bulk_status(
    State(state): State<AppState>,
    Json(req): Json<BulkStatusRequest>,
) -> Result<Json<BulkStatusResponse>, AppError> {
    let client = state.get_client(req.federation_id).await?;
    let mut statuses = Vec::new();
    let mut errors = Vec::new();

    info!(
        federation_id = %req.federation_id,
        operation_count = req.operation_ids.len(),
        "Processing bulk status request using fedimint native behavior"
    );

    // Process each operation ID
    for operation_id in req.operation_ids {
        let query = StatusQuery {
            federation_id: req.federation_id,
        };

        match _get_status(&state, client.clone(), operation_id, query).await {
            Ok(status_response) => {
                statuses.push(BulkStatusItem {
                    operation_id,
                    status: status_response.status,
                    settlement: status_response.settlement,
                    tracked_status: status_response.tracked_status,
                    last_updated: status_response.last_updated,
                });
            }
            Err(e) => {
                error!(
                    operation_id = ?operation_id,
                    federation_id = %req.federation_id,
                    error = ?e,
                    "Failed to get status for operation in bulk request"
                );
                errors.push(BulkStatusError {
                    operation_id,
                    error: e.to_string(),
                });
            }
        }
    }

    info!(
        federation_id = %req.federation_id,
        successful_count = statuses.len(),
        error_count = errors.len(),
        "Completed bulk status request"
    );

    Ok(Json(BulkStatusResponse { statuses, errors }))
}
