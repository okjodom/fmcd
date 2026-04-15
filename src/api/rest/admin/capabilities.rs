use std::collections::HashMap;

use axum::extract::State;
use axum::Json;
use fedimint_core::config::FederationId;
use serde_json::{json, Value};

use crate::core::services::client_lifecycle::FederationCapabilities;
use crate::error::AppError;
use crate::state::AppState;

pub async fn handle_ws(state: AppState) -> Result<Value, AppError> {
    let capabilities = state.core.get_federation_capabilities().await?;
    Ok(json!(capabilities))
}

#[axum_macros::debug_handler]
pub async fn handle_rest(
    State(state): State<AppState>,
) -> Result<Json<HashMap<FederationId, FederationCapabilities>>, AppError> {
    let capabilities = state.core.get_federation_capabilities().await?;
    Ok(Json(capabilities))
}
