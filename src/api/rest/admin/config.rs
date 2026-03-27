use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::error::AppError;
use crate::state::AppState;

pub async fn handle_ws(state: AppState) -> Result<Value, AppError> {
    let config = state.core.get_federation_configs().await?;
    let config_json = json!(config);
    Ok(config_json)
}

#[axum_macros::debug_handler]
pub async fn handle_rest(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let config = state.core.get_federation_configs().await?;
    Ok(Json(config))
}
