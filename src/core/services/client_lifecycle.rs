use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use fedimint_client::backup::Metadata;
use fedimint_client::ClientHandleArc;
use fedimint_core::config::{FederationId, FederationIdPrefix};
use fedimint_core::core::ModuleInstanceId;
use fedimint_core::invite_code::InviteCode;
use serde::Serialize;
use serde_json::Value;
use tracing::{info, warn};

use crate::core::multimint::MultiMint;
use crate::core::JoinFederationResponse;
use crate::error::{AppError, ErrorCategory};
use crate::events::{EventBus, FmcdEvent};
use crate::observability::correlation::RequestContext;

#[derive(Debug, Clone)]
pub struct ClientLifecycleService {
    multimint: Arc<MultiMint>,
    event_bus: Arc<EventBus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleCapability {
    pub instance_id: ModuleInstanceId,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LightningCapabilities {
    pub lnv1: bool,
    pub lnv2: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationCapabilities {
    pub federation_id: FederationId,
    pub modules: Vec<ModuleCapability>,
    pub lightning: LightningCapabilities,
    pub mint: bool,
    pub wallet: bool,
}

impl ClientLifecycleService {
    pub fn new(multimint: Arc<MultiMint>, event_bus: Arc<EventBus>) -> Self {
        Self {
            multimint,
            event_bus,
        }
    }

    pub async fn get_client(
        &self,
        federation_id: FederationId,
    ) -> Result<ClientHandleArc, AppError> {
        info!(
            federation_id = %federation_id,
            "Retrieving client for federation"
        );

        match self.multimint.get(&federation_id).await {
            Some(client) => {
                info!(
                    federation_id = %federation_id,
                    "Client retrieved successfully"
                );
                Ok(client)
            }
            None => {
                warn!(
                    federation_id = %federation_id,
                    "No client found for federation"
                );
                Err(AppError::with_category(
                    ErrorCategory::FederationNotFound,
                    format!("No client found for federation id: {}", federation_id),
                ))
            }
        }
    }

    pub async fn get_client_by_prefix(
        &self,
        federation_id_prefix: &FederationIdPrefix,
    ) -> Result<ClientHandleArc, AppError> {
        info!(
            federation_id_prefix = %federation_id_prefix,
            "Retrieving client for federation prefix"
        );

        match self.multimint.get_by_prefix(federation_id_prefix).await {
            Some(client) => {
                info!(
                    federation_id_prefix = %federation_id_prefix,
                    "Client retrieved successfully by prefix"
                );
                Ok(client)
            }
            None => {
                warn!(
                    federation_id_prefix = %federation_id_prefix,
                    "No client found for federation prefix"
                );
                Err(AppError::with_category(
                    ErrorCategory::FederationNotFound,
                    format!(
                        "No client found for federation id prefix: {}",
                        federation_id_prefix
                    ),
                ))
            }
        }
    }

    pub async fn join_federation(
        &self,
        invite_code: InviteCode,
        context: Option<RequestContext>,
    ) -> Result<JoinFederationResponse> {
        let federation_id = invite_code.federation_id();

        info!(
            federation_id = %federation_id,
            "Attempting to join federation"
        );

        let mut multimint = (*self.multimint).clone();

        let this_federation_id = multimint
            .register_new(invite_code.clone())
            .await
            .inspect_err(|e| {
                let event_bus = self.event_bus.clone();
                let federation_id_str = federation_id.to_string();
                let correlation_id = context.as_ref().map(|c| c.correlation_id.clone());
                let error_msg = e.to_string();

                tokio::spawn(async move {
                    let event = FmcdEvent::FederationDisconnected {
                        federation_id: federation_id_str,
                        reason: format!("Failed to join: {}", error_msg),
                        correlation_id,
                        timestamp: Utc::now(),
                    };
                    let _ = event_bus.publish(event).await;
                });
            })?;

        let event_bus = self.event_bus.clone();
        let federation_id_str = this_federation_id.to_string();
        let correlation_id = context.as_ref().map(|c| c.correlation_id.clone());

        tokio::spawn(async move {
            let event = FmcdEvent::FederationConnected {
                federation_id: federation_id_str,
                correlation_id,
                timestamp: Utc::now(),
            };
            let _ = event_bus.publish(event).await;
        });

        let federation_ids = self.multimint.ids().await.into_iter().collect::<Vec<_>>();

        info!(
            federation_id = %this_federation_id,
            total_federations = federation_ids.len(),
            "Successfully joined federation"
        );

        Ok(JoinFederationResponse {
            this_federation_id,
            federation_ids,
        })
    }

    pub async fn backup_to_federation(
        &self,
        federation_id: FederationId,
        metadata: std::collections::BTreeMap<String, String>,
    ) -> Result<(), AppError> {
        let client = self.get_client(federation_id).await?;
        client
            .backup_to_federation(Metadata::from_json_serialized(metadata))
            .await
            .map_err(|e| AppError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e))
    }

    pub async fn get_configs(&self) -> Result<Value, AppError> {
        let mut config = HashMap::new();
        for (id, client) in self.multimint.clients.lock().await.iter() {
            config.insert(*id, client.config().await.to_json());
        }
        Ok(serde_json::to_value(config)
            .map_err(|e| anyhow::anyhow!("Client config is serializable: {e}"))?)
    }

    pub async fn get_capabilities(
        &self,
    ) -> Result<HashMap<FederationId, FederationCapabilities>, AppError> {
        let mut capabilities = HashMap::new();

        for (federation_id, client) in self.multimint.clients.lock().await.iter() {
            let config = client.config().await;
            let mut modules = config
                .modules
                .iter()
                .map(|(instance_id, module_config)| ModuleCapability {
                    instance_id: *instance_id,
                    kind: module_config.kind().as_str().to_owned(),
                })
                .collect::<Vec<_>>();
            modules.sort_by_key(|module| module.instance_id);

            let has_module = |kind: &str| modules.iter().any(|module| module.kind == kind);
            let has_lnv1 = has_module("ln");
            let has_lnv2 = has_module("lnv2");
            let has_mint = has_module("mint");
            let has_wallet = has_module("wallet");

            capabilities.insert(
                *federation_id,
                FederationCapabilities {
                    federation_id: *federation_id,
                    modules,
                    lightning: LightningCapabilities {
                        lnv1: has_lnv1,
                        lnv2: has_lnv2,
                    },
                    mint: has_mint,
                    wallet: has_wallet,
                },
            );
        }

        Ok(capabilities)
    }
}
