use std::time::UNIX_EPOCH;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use fedimint_client::ClientHandleArc;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::format_description::well_known::iso8601;
use time::OffsetDateTime;

use crate::core::operations::PaymentOperation;
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListOperationsRequest {
    pub limit: usize,
    pub federation_id: FederationId,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationOutput {
    pub id: OperationId,
    pub creation_time: String,
    pub operation_kind: String,
    pub operation_meta: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracked_operation: Option<TrackedOperationOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedOperationOutput {
    pub id: OperationId,
    pub federation_id: FederationId,
    pub payment_type: String,
    pub status: String,
    pub amount_msat: Option<u64>,
    pub fee_msat: Option<u64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    pub last_error: Option<String>,
    pub claim_attempted: bool,
    pub ecash_claimed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedOperationsResponse {
    pub operations: Vec<TrackedOperationOutput>,
}

fn tracked_operation_output(operation: PaymentOperation) -> TrackedOperationOutput {
    TrackedOperationOutput {
        id: operation.operation_id,
        federation_id: operation.federation_id,
        payment_type: format!("{:?}", operation.payment_type),
        status: format!("{:?}", operation.status),
        amount_msat: operation.amount_msat.map(|amount| amount.msats),
        fee_msat: operation.fee_msat,
        created_at: operation.created_at,
        updated_at: operation.updated_at,
        correlation_id: operation.correlation_id,
        metadata: operation.metadata,
        last_error: operation.last_error,
        claim_attempted: operation.claim_attempted,
        ecash_claimed: operation.ecash_claimed,
    }
}

async fn _operations(
    state: &AppState,
    client: ClientHandleArc,
    req: ListOperationsRequest,
) -> Result<Value, AppError> {
    const ISO8601_CONFIG: iso8601::EncodedConfig = iso8601::Config::DEFAULT
        .set_formatted_components(iso8601::FormattedComponents::DateTime)
        .encode();
    let mut operations = Vec::new();

    for (k, v) in client
        .operation_log()
        .paginate_operations_rev(req.limit, None)
        .await
    {
        let creation_time = OffsetDateTime::from_unix_timestamp(
            k.creation_time
                .duration_since(UNIX_EPOCH)
                .map_err(|e| {
                    anyhow::anyhow!("Couldn't convert time from SystemTime to timestamp: {}", e)
                })?
                .as_secs() as i64,
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "Couldn't convert time from SystemTime to OffsetDateTime: {}",
                e
            )
        })?
        .format(&iso8601::Iso8601::<ISO8601_CONFIG>)
        .map_err(|e| anyhow::anyhow!("Couldn't format OffsetDateTime as ISO8601: {}", e))?;

        operations.push(OperationOutput {
            id: k.operation_id,
            creation_time,
            operation_kind: v.operation_module_kind().to_owned(),
            operation_meta: v.meta(),
            tracked_operation: state
                .core
                .get_tracked_operation(&k.operation_id)
                .await
                .map(tracked_operation_output),
            outcome: v.outcome(),
        });
    }

    Ok(json!({
        "operations": operations,
    }))
}

async fn _tracked_operations(
    state: &AppState,
    req: ListOperationsRequest,
) -> Result<TrackedOperationsResponse, AppError> {
    let operations = state
        .core
        .list_tracked_operations(req.federation_id, req.limit)
        .await
        .into_iter()
        .map(tracked_operation_output)
        .collect();

    Ok(TrackedOperationsResponse { operations })
}

async fn _tracked_operation_by_id(
    state: &AppState,
    operation_id: OperationId,
) -> Result<TrackedOperationOutput, AppError> {
    state
        .core
        .get_tracked_operation(&operation_id)
        .await
        .map(tracked_operation_output)
        .ok_or_else(|| {
            AppError::new(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("Tracked operation not found: {:?}", operation_id),
            )
        })
}

pub async fn handle_ws(state: AppState, v: Value) -> Result<Value, AppError> {
    let v = serde_json::from_value::<ListOperationsRequest>(v).map_err(|e| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("Invalid request: {}", e),
        )
    })?;
    let client = state.get_client(v.federation_id).await?;
    let operations = _operations(&state, client, v).await?;
    let operations_json = json!(operations);
    Ok(operations_json)
}

#[axum_macros::debug_handler]
pub async fn handle_rest(
    State(state): State<AppState>,
    Json(req): Json<ListOperationsRequest>,
) -> Result<Json<Value>, AppError> {
    let client = state.get_client(req.federation_id).await?;
    let operations = _operations(&state, client, req).await?;
    Ok(Json(operations))
}

#[axum_macros::debug_handler]
pub async fn handle_tracked_rest(
    State(state): State<AppState>,
    Json(req): Json<ListOperationsRequest>,
) -> Result<Json<TrackedOperationsResponse>, AppError> {
    let operations = _tracked_operations(&state, req).await?;
    Ok(Json(operations))
}

#[axum_macros::debug_handler]
pub async fn handle_tracked_rest_by_operation_id(
    State(state): State<AppState>,
    Path(operation_id_str): Path<String>,
) -> Result<Json<TrackedOperationOutput>, AppError> {
    let operation_id = operation_id_str.parse::<OperationId>().map_err(|e| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("Invalid operation ID: {}", e),
        )
    })?;

    let operation = _tracked_operation_by_id(&state, operation_id).await?;
    Ok(Json(operation))
}
