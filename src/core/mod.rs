pub mod multimint;
pub mod operations;
pub mod services;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use bitcoin::{Address, Txid};
use fedimint_client::ClientHandleArc;
use fedimint_core::config::{FederationId, FederationIdPrefix};
use fedimint_core::core::OperationId;
use fedimint_core::invite_code::InviteCode;
use fedimint_core::secp256k1::PublicKey;
use fedimint_core::{Amount, BitcoinAmountOrAll, TieredCounts};
use fedimint_ln_client::{LightningClientModule, PayType};
use fedimint_mint_client::MintClientModule;
use fedimint_wallet_client::client_db::TweakIdx;
use fedimint_wallet_client::{WalletClientModule, WithdrawState};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

// Use local module imports
use self::multimint::MultiMint;
use self::operations::payment::InvoiceTracker;
use self::operations::PaymentTracker;
use self::services::{
    BalanceMonitor, BalanceMonitorConfig, ClientLifecycleService, DepositMonitor,
    DepositMonitorConfig, LightningService, PaymentLifecycleConfig, PaymentLifecycleManager,
};
use crate::error::AppError;
use crate::events::handlers::{LoggingEventHandler, MetricsEventHandler};
use crate::events::EventBus;
use crate::observability::correlation::RequestContext;
use crate::webhooks::{WebhookConfig, WebhookNotifier};

fn tracked_lightning_metadata(
    protocol: &str,
    operation_type: &str,
    metadata: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let mut object = match metadata {
        Some(serde_json::Value::Object(map)) => map,
        Some(other) => {
            let mut map = serde_json::Map::new();
            map.insert("clientMetadata".to_string(), other);
            map
        }
        None => serde_json::Map::new(),
    };

    object.insert(
        "protocol".to_string(),
        serde_json::Value::String(protocol.to_string()),
    );
    object.insert(
        "type".to_string(),
        serde_json::Value::String(operation_type.to_string()),
    );

    Some(serde_json::Value::Object(object))
}

/// Trait for resolving payment information into Bolt11 invoices
/// This allows the core to remain agnostic about web protocols like LNURL
/// while allowing the API layer to provide resolution capabilities
#[async_trait::async_trait]
pub trait PaymentInfoResolver: Send + Sync {
    /// Resolve payment info (LNURL, Lightning Address, etc.) into a Bolt11
    /// invoice Returns the invoice string if resolution was successful, or
    /// None if the payment_info should be treated as a raw Bolt11 invoice
    async fn resolve_payment_info(
        &self,
        payment_info: &str,
        amount_msat: Option<Amount>,
        lnurl_comment: Option<&str>,
    ) -> Result<Option<String>, AppError>;
}

/// Invoice creation request with essential fields
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LnInvoiceRequest {
    pub amount_msat: Amount,
    pub description: String,
    pub expiry_time: Option<u64>,
    #[serde(default)]
    pub gateway_id: Option<PublicKey>,
    pub federation_id: FederationId,
    pub metadata: Option<serde_json::Value>,
}

/// Lightning payment request
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LnPayRequest {
    pub payment_info: String,
    pub amount_msat: Option<Amount>,
    pub lnurl_comment: Option<String>,
    #[serde(default)]
    pub gateway_id: Option<PublicKey>,
    pub federation_id: FederationId,
}

/// Lightning payment response
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LnPayResponse {
    pub operation_id: OperationId,
    pub payment_type: PayType,
    pub contract_id: String,
    pub fee: Amount,
    pub preimage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

/// Invoice response with essential information
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LnInvoiceResponse {
    pub invoice_id: String,
    pub operation_id: OperationId,
    pub invoice: String,
    pub status: InvoiceStatus,
    pub settlement: Option<SettlementInfo>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

/// Unified invoice status enum
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum InvoiceStatus {
    Created,
    Pending,
    Claimed {
        amount_received_msat: u64,
        settled_at: chrono::DateTime<chrono::Utc>,
    },
    Expired {
        expired_at: chrono::DateTime<chrono::Utc>,
    },
    Canceled {
        reason: String,
        canceled_at: chrono::DateTime<chrono::Utc>,
    },
}

/// Settlement information structure
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SettlementInfo {
    pub amount_received_msat: u64,
    pub settled_at: chrono::DateTime<chrono::Utc>,
    pub preimage: Option<String>,
    pub gateway_fee_msat: Option<u64>,
}

/// Info response structure
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InfoResponse {
    pub network: String,
    pub meta: BTreeMap<String, String>,
    pub total_amount_msat: Amount,
    /// Number of ecash notes in the mint module (does not include on-chain or
    /// lightning balances)
    pub total_num_notes: usize,
    /// Breakdown of ecash notes by denomination in the mint module
    pub denominations_msat: TieredCounts,
}

/// Join federation response
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinFederationResponse {
    pub this_federation_id: FederationId,
    pub federation_ids: Vec<FederationId>,
}

/// Onchain withdraw request
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WithdrawRequest {
    pub address: String,
    pub amount_sat: BitcoinAmountOrAll,
    pub federation_id: FederationId,
}

/// Onchain withdraw response
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WithdrawResponse {
    pub txid: Txid,
    pub fees_sat: u64,
}

/// Deposit address request
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepositAddressRequest {
    pub federation_id: FederationId,
}

/// Deposit address response
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DepositAddressResponse {
    pub address: String,
    pub operation_id: OperationId,
    pub tweak_idx: TweakIdx,
}

/// Main entry point for library consumers
pub struct FmcdCore {
    pub multimint: Arc<MultiMint>,
    pub start_time: Instant,
    pub event_bus: Arc<EventBus>,
    pub client_lifecycle_service: Arc<ClientLifecycleService>,
    pub lightning_service: Arc<LightningService>,
    pub deposit_monitor: Option<Arc<DepositMonitor>>,
    pub balance_monitor: Option<Arc<BalanceMonitor>>,
    pub payment_lifecycle_manager: Option<Arc<PaymentLifecycleManager>>,
}

impl FmcdCore {
    pub async fn new(data_dir: PathBuf) -> Result<Self> {
        Self::new_with_config(data_dir, WebhookConfig::default()).await
    }

    pub async fn new_with_config(data_dir: PathBuf, webhook_config: WebhookConfig) -> Result<Self> {
        let multimint = MultiMint::new(data_dir.clone()).await?;
        multimint.update_gateway_caches().await?;
        let multimint = Arc::new(multimint);

        // Initialize event bus with reasonable capacity
        let event_bus = Arc::new(EventBus::new(1000));
        let client_lifecycle_service = Arc::new(ClientLifecycleService::new(
            multimint.clone(),
            event_bus.clone(),
        ));
        let lightning_service = Arc::new(LightningService::new());

        // Register default event handlers
        let logging_handler = Arc::new(LoggingEventHandler::new(false));
        let metrics_handler = Arc::new(MetricsEventHandler::new("fmcd"));

        event_bus.register_handler(logging_handler).await;
        event_bus.register_handler(metrics_handler).await;

        // Register webhook notifier if webhooks are configured
        if webhook_config.enabled && !webhook_config.endpoints.is_empty() {
            match WebhookNotifier::new(webhook_config) {
                Ok(webhook_notifier) => {
                    let webhook_handler = Arc::new(webhook_notifier);
                    event_bus.register_handler(webhook_handler).await;
                    info!("Webhook notifier registered successfully");
                }
                Err(e) => {
                    warn!("Failed to initialize webhook notifier: {}", e);
                }
            }
        }

        info!("Event bus initialized with all handlers");

        // Initialize monitoring services
        let deposit_monitor = Arc::new(DepositMonitor::new(
            event_bus.clone(),
            multimint.clone(),
            DepositMonitorConfig::default(),
        ));

        let balance_monitor = Arc::new(BalanceMonitor::new(
            event_bus.clone(),
            multimint.clone(),
            BalanceMonitorConfig::default(),
        ));

        let payment_lifecycle_manager = Arc::new(PaymentLifecycleManager::new(
            event_bus.clone(),
            multimint.clone(),
            data_dir.clone(),
            PaymentLifecycleConfig::default(),
        ));

        Ok(Self {
            multimint,
            start_time: Instant::now(),
            event_bus,
            client_lifecycle_service,
            lightning_service,
            deposit_monitor: Some(deposit_monitor),
            balance_monitor: Some(balance_monitor),
            payment_lifecycle_manager: Some(payment_lifecycle_manager),
        })
    }

    /// Get uptime since core was initialized
    pub fn uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Start the monitoring services (deposit, balance, and payment lifecycle
    /// monitors)
    pub async fn start_monitoring_services(&self) -> Result<()> {
        if let Some(ref deposit_monitor) = self.deposit_monitor {
            deposit_monitor.start().await?;
            info!("Deposit monitor started successfully");
        }

        if let Some(ref balance_monitor) = self.balance_monitor {
            balance_monitor.start().await?;
            info!("Balance monitor started successfully");
        }

        if let Some(ref payment_lifecycle_manager) = self.payment_lifecycle_manager {
            payment_lifecycle_manager.start().await?;
            info!("Payment lifecycle manager started successfully");
        }

        Ok(())
    }

    /// Stop the monitoring services (deposit and balance monitors)
    pub async fn stop_monitoring_services(&self) -> Result<()> {
        if let Some(ref deposit_monitor) = self.deposit_monitor {
            deposit_monitor.stop().await?;
            info!("Deposit monitor stopped successfully");
        }

        if let Some(ref balance_monitor) = self.balance_monitor {
            balance_monitor.stop().await?;
            info!("Balance monitor stopped successfully");
        }

        Ok(())
    }

    /// Get a client by federation ID
    pub async fn get_client(
        &self,
        federation_id: FederationId,
    ) -> Result<ClientHandleArc, AppError> {
        self.client_lifecycle_service
            .get_client(federation_id)
            .await
    }

    /// Get a client by federation ID prefix
    pub async fn get_client_by_prefix(
        &self,
        federation_id_prefix: &FederationIdPrefix,
    ) -> Result<ClientHandleArc, AppError> {
        self.client_lifecycle_service
            .get_client_by_prefix(federation_id_prefix)
            .await
    }

    /// Join a federation with an invite code
    pub async fn join_federation(
        &self,
        invite_code: InviteCode,
        context: Option<RequestContext>,
    ) -> Result<JoinFederationResponse> {
        self.client_lifecycle_service
            .join_federation(invite_code, context)
            .await
    }

    pub async fn get_federation_configs(&self) -> Result<serde_json::Value, AppError> {
        self.client_lifecycle_service.get_configs().await
    }

    pub async fn get_federation_capabilities(
        &self,
    ) -> Result<
        std::collections::HashMap<
            FederationId,
            crate::core::services::client_lifecycle::FederationCapabilities,
        >,
        AppError,
    > {
        self.client_lifecycle_service.get_capabilities().await
    }

    pub async fn backup_federation(
        &self,
        federation_id: FederationId,
        metadata: BTreeMap<String, String>,
    ) -> Result<(), AppError> {
        self.client_lifecycle_service
            .backup_to_federation(federation_id, metadata)
            .await
    }

    pub async fn get_tracked_operation(
        &self,
        operation_id: &OperationId,
    ) -> Option<crate::core::operations::PaymentOperation> {
        match &self.payment_lifecycle_manager {
            Some(manager) => manager.get_operation(operation_id).await,
            None => None,
        }
    }

    pub async fn list_tracked_operations(
        &self,
        federation_id: FederationId,
        limit: usize,
    ) -> Vec<crate::core::operations::PaymentOperation> {
        match &self.payment_lifecycle_manager {
            Some(manager) => manager.list_operations(federation_id, limit).await,
            None => Vec::new(),
        }
    }

    /// Get wallet info for all federations
    pub async fn get_info(&self) -> Result<HashMap<FederationId, InfoResponse>> {
        let mut info = HashMap::new();

        for (id, client) in self.multimint.clients.lock().await.iter() {
            let mint_client = client.get_first_module::<MintClientModule>()?;
            let wallet_client = client.get_first_module::<WalletClientModule>()?;
            let mut dbtx = client.db().begin_transaction_nc().await;
            let summary = mint_client.get_note_counts_by_denomination(&mut dbtx).await;

            // Get the actual total balance from the client (includes all modules)
            let total_balance = client.get_balance_err().await?;

            info.insert(
                *id,
                InfoResponse {
                    network: wallet_client.get_network().to_string(),
                    meta: client.config().await.global.meta.clone(),
                    total_amount_msat: total_balance,
                    total_num_notes: summary.count_items(),
                    denominations_msat: summary,
                },
            );
        }

        Ok(info)
    }

    /// Create a lightning invoice
    pub async fn create_invoice(
        &self,
        req: LnInvoiceRequest,
        context: RequestContext,
    ) -> Result<LnInvoiceResponse, AppError> {
        use chrono::Utc;
        use uuid::Uuid;

        let client = self.get_client(req.federation_id).await?;

        info!(
            gateway_id = req.gateway_id.map(|gateway| gateway.to_string()).unwrap_or_else(|| "auto".to_string()),
            federation_id = %req.federation_id,
            amount_msat = %req.amount_msat.msats,
            "Creating invoice with automatic monitoring"
        );

        let created_at = Utc::now();
        let expires_at = req
            .expiry_time
            .map(|expiry| created_at + chrono::Duration::seconds(expiry as i64));

        // Use provided metadata or default to null
        let metadata = req.metadata.clone().unwrap_or(serde_json::Value::Null);

        let invoice_result = self
            .lightning_service
            .create_invoice(
                &client,
                req.federation_id,
                req.gateway_id,
                req.amount_msat,
                &req.description,
                req.expiry_time,
                metadata,
            )
            .await?;
        let operation_id = invoice_result.operation_id;
        let invoice = invoice_result.invoice;
        let protocol = invoice_result.protocol;

        // Generate unique invoice ID for tracking
        let invoice_id = format!("inv_{}", Uuid::new_v4().simple());

        // Create invoice tracker for observability
        let invoice_tracker = InvoiceTracker::new(
            invoice_id.clone(),
            req.federation_id,
            self.event_bus.clone(),
            Some(context.clone()),
        );

        // Track invoice creation
        invoice_tracker
            .created(req.amount_msat.msats, invoice.to_string())
            .await;

        let response = LnInvoiceResponse {
            invoice_id: invoice_id.clone(),
            operation_id,
            invoice: invoice.to_string(),
            status: InvoiceStatus::Created,
            settlement: None,
            created_at,
            expires_at,
            metadata: req.metadata.clone(),
            protocol: Some(protocol.as_str().to_string()),
        };

        // Register with payment lifecycle manager for comprehensive tracking
        if let Some(ref payment_lifecycle_manager) = self.payment_lifecycle_manager {
            if let Err(e) = payment_lifecycle_manager
                .track_lightning_receive(
                    operation_id,
                    req.federation_id,
                    req.amount_msat,
                    tracked_lightning_metadata(
                        protocol.as_str(),
                        "ln_receive",
                        req.metadata.clone(),
                    ),
                    Some(context.correlation_id.clone()),
                )
                .await
            {
                error!(
                    operation_id = ?operation_id,
                    invoice_id = %invoice_id,
                    error = ?e,
                    "Failed to register invoice with payment lifecycle manager"
                );
            } else {
                info!(
                    operation_id = ?operation_id,
                    invoice_id = %invoice_id,
                    "Invoice registered with payment lifecycle manager for automatic ecash claiming"
                );
            }
        }

        if protocol == crate::core::services::lightning::LightningProtocol::Lnv1 {
            self.start_invoice_monitoring(
                client,
                operation_id,
                invoice_id.clone(),
                req.amount_msat.msats,
                invoice_tracker,
            )
            .await;
        }

        info!(
            operation_id = ?operation_id,
            invoice_id = %invoice_id,
            federation_id = %req.federation_id,
            amount_msat = %req.amount_msat.msats,
            "Invoice created successfully with automatic monitoring"
        );

        Ok(response)
    }

    /// Generate a deposit address for receiving on-chain payments
    pub async fn create_deposit_address(
        &self,
        req: DepositAddressRequest,
        context: RequestContext,
    ) -> Result<DepositAddressResponse, AppError> {
        use chrono::Utc;

        use crate::core::services::deposit_monitor::DepositInfo;
        use crate::events::FmcdEvent;

        let client = self.get_client(req.federation_id).await?;
        let wallet_module = client
            .get_first_module::<WalletClientModule>()
            .map_err(|e| {
                error!(
                    federation_id = %req.federation_id,
                    error = ?e,
                    "Failed to get wallet module from fedimint client"
                );
                AppError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    anyhow!("Failed to get wallet module: {}", e),
                )
            })?;

        let (operation_id, address, tweak_idx) = wallet_module
            .allocate_deposit_address_expert_only(())
            .await
            .map_err(|e| {
                error!(
                    federation_id = %req.federation_id,
                    error = ?e,
                    "Failed to generate deposit address"
                );
                AppError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    anyhow!("Failed to generate deposit address: {}", e),
                )
            })?;

        // Emit deposit address generated event
        let event_bus = self.event_bus.clone();
        let federation_id_str = req.federation_id.to_string();
        let address_str = address.to_string();
        let operation_id_str = format!("{:?}", operation_id);
        let correlation_id = context.correlation_id.clone();

        tokio::spawn(async move {
            let event = FmcdEvent::DepositAddressGenerated {
                operation_id: operation_id_str,
                federation_id: federation_id_str,
                address: address_str,
                correlation_id: Some(correlation_id),
                timestamp: Utc::now(),
            };
            let _ = event_bus.publish(event).await;
        });

        // Register deposit with monitor for detection
        if let Some(ref deposit_monitor) = self.deposit_monitor {
            let deposit_info = DepositInfo {
                operation_id,
                federation_id: req.federation_id,
                address: address.to_string(),
                correlation_id: Some(context.correlation_id.clone()),
                created_at: Utc::now(),
            };

            if let Err(e) = deposit_monitor.add_deposit(deposit_info).await {
                // Log error but don't fail the request - monitoring is best effort
                warn!(
                    operation_id = ?operation_id,
                    federation_id = %req.federation_id,
                    error = ?e,
                    "Failed to register deposit with monitor"
                );
            } else {
                info!(
                    operation_id = ?operation_id,
                    federation_id = %req.federation_id,
                    "Deposit registered with monitor"
                );
            }
        }

        // Register with payment lifecycle manager for automatic ecash claiming
        if let Some(ref payment_lifecycle_manager) = self.payment_lifecycle_manager {
            if let Err(e) = payment_lifecycle_manager
                .track_onchain_deposit(
                    operation_id,
                    req.federation_id,
                    Some(context.correlation_id.clone()),
                )
                .await
            {
                warn!(
                    operation_id = ?operation_id,
                    federation_id = %req.federation_id,
                    error = ?e,
                    "Failed to register deposit with payment lifecycle manager"
                );
            } else {
                info!(
                    operation_id = ?operation_id,
                    federation_id = %req.federation_id,
                    "Deposit registered with payment lifecycle manager for automatic ecash claiming"
                );
            }
        }

        info!(
            federation_id = %req.federation_id,
            operation_id = ?operation_id,
            address = %address,
            "Deposit address generated successfully"
        );

        Ok(DepositAddressResponse {
            address: address.to_string(),
            operation_id,
            tweak_idx,
        })
    }

    /// Withdraw funds to an on-chain Bitcoin address
    pub async fn withdraw_onchain(
        &self,
        req: WithdrawRequest,
        context: RequestContext,
    ) -> Result<WithdrawResponse, AppError> {
        use std::str::FromStr;

        use chrono::Utc;
        use futures_util::StreamExt;

        use crate::events::FmcdEvent;

        let client = self.get_client(req.federation_id).await?;
        let wallet_module = client
            .get_first_module::<WalletClientModule>()
            .map_err(|e| {
                error!(
                    federation_id = %req.federation_id,
                    error = ?e,
                    "Failed to get wallet module from fedimint client"
                );
                AppError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    anyhow!("Failed to get wallet module: {}", e),
                )
            })?;

        // Parse the address - from_str gives us Address<NetworkUnchecked>
        let address_unchecked = Address::from_str(&req.address)
            .map_err(|e| AppError::validation_error(format!("Invalid Bitcoin address: {}", e)))?;

        // TODO: Properly validate network - for now assuming valid
        let address = address_unchecked.assume_checked();
        let (amount, fees) = match req.amount_sat {
            // If the amount is "all", then we need to subtract the fees from
            // the amount we are withdrawing
            BitcoinAmountOrAll::All => {
                let balance =
                    bitcoin::Amount::from_sat(client.get_balance_err().await?.msats / 1000);
                let fees = wallet_module.get_withdraw_fees(&address, balance).await?;
                let amount = balance.checked_sub(fees.amount());
                let amount = match amount {
                    Some(amount) => amount,
                    None => {
                        return Err(AppError::new(
                            axum::http::StatusCode::BAD_REQUEST,
                            anyhow!("Insufficient balance to pay fees"),
                        ))
                    }
                };

                (amount, fees)
            }
            BitcoinAmountOrAll::Amount(amount) => (
                amount,
                wallet_module.get_withdraw_fees(&address, amount).await?,
            ),
        };
        let absolute_fees = fees.amount();

        info!("Attempting withdraw with fees: {fees:?}");

        let operation_id = wallet_module.withdraw(&address, amount, fees, ()).await?;

        // Emit withdrawal initiated event
        let withdrawal_initiated_event = FmcdEvent::WithdrawalInitiated {
            operation_id: format!("{:?}", operation_id),
            federation_id: req.federation_id.to_string(),
            address: address.to_string(),
            amount_sat: amount.to_sat(),
            fee_sat: absolute_fees.to_sat(),
            correlation_id: Some(context.correlation_id.clone()),
            timestamp: Utc::now(),
        };
        if let Err(e) = self.event_bus.publish(withdrawal_initiated_event).await {
            error!(
                operation_id = ?operation_id,
                correlation_id = %context.correlation_id,
                error = ?e,
                "Failed to publish withdrawal initiated event"
            );
        }

        info!(
            operation_id = ?operation_id,
            address = %address,
            amount_sat = amount.to_sat(),
            fee_sat = absolute_fees.to_sat(),
            "Withdrawal initiated"
        );

        // Register with payment lifecycle manager for comprehensive monitoring
        if let Some(ref payment_lifecycle_manager) = self.payment_lifecycle_manager {
            if let Err(e) = payment_lifecycle_manager
                .track_onchain_withdraw(
                    operation_id,
                    req.federation_id,
                    amount.to_sat(),
                    absolute_fees.to_sat(),
                    Some(context.correlation_id.clone()),
                )
                .await
            {
                error!(
                    operation_id = ?operation_id,
                    error = ?e,
                    "Failed to register withdrawal with payment lifecycle manager"
                );
            } else {
                info!(
                    operation_id = ?operation_id,
                    "Withdrawal registered with payment lifecycle manager for monitoring"
                );
            }
        }

        let mut updates = wallet_module
            .subscribe_withdraw_updates(operation_id)
            .await?
            .into_stream();

        while let Some(update) = updates.next().await {
            info!("Update: {update:?}");

            match update {
                WithdrawState::Succeeded(txid) => {
                    // Emit withdrawal succeeded event
                    let withdrawal_succeeded_event = FmcdEvent::WithdrawalSucceeded {
                        operation_id: format!("{:?}", operation_id),
                        federation_id: req.federation_id.to_string(),
                        amount_sat: amount.to_sat(),
                        txid: txid.to_string(),
                        timestamp: Utc::now(),
                    };
                    if let Err(e) = self.event_bus.publish(withdrawal_succeeded_event).await {
                        error!(
                            operation_id = ?operation_id,
                            correlation_id = %context.correlation_id,
                            txid = %txid,
                            error = ?e,
                            "Failed to publish withdrawal completed event"
                        );
                    }

                    info!(
                        operation_id = ?operation_id,
                        txid = %txid,
                        "Withdrawal completed successfully"
                    );

                    return Ok(WithdrawResponse {
                        txid,
                        fees_sat: absolute_fees.to_sat(),
                    });
                }
                WithdrawState::Failed(e) => {
                    let error_reason = format!("Withdraw failed: {:?}", e);

                    // Emit withdrawal failed event
                    let withdrawal_failed_event = FmcdEvent::WithdrawalFailed {
                        operation_id: format!("{:?}", operation_id),
                        federation_id: req.federation_id.to_string(),
                        reason: error_reason.clone(),
                        correlation_id: Some(context.correlation_id.clone()),
                        timestamp: Utc::now(),
                    };
                    if let Err(event_err) = self.event_bus.publish(withdrawal_failed_event).await {
                        error!(
                            operation_id = ?operation_id,
                            correlation_id = %context.correlation_id,
                            error = ?event_err,
                            "Failed to publish withdrawal failed event"
                        );
                    }

                    error!(
                        operation_id = ?operation_id,
                        error = ?e,
                        "Withdrawal failed"
                    );

                    return Err(AppError::new(
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        anyhow!("{}", error_reason),
                    ));
                }
                _ => continue,
            };
        }

        // Emit withdrawal failed event for stream ending without outcome
        let error_reason = "Update stream ended without outcome".to_string();
        let withdrawal_failed_event = FmcdEvent::WithdrawalFailed {
            operation_id: format!("{:?}", operation_id),
            federation_id: req.federation_id.to_string(),
            reason: error_reason.clone(),
            correlation_id: Some(context.correlation_id.clone()),
            timestamp: Utc::now(),
        };
        if let Err(e) = self.event_bus.publish(withdrawal_failed_event).await {
            error!(
                operation_id = ?operation_id,
                correlation_id = %context.correlation_id,
                error = ?e,
                "Failed to publish withdrawal failed event for stream timeout"
            );
        }

        error!(
            operation_id = ?operation_id,
            "Update stream ended without outcome"
        );

        Err(AppError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            anyhow!("{}", error_reason),
        ))
    }

    /// Pay a lightning invoice
    pub async fn pay_invoice(
        &self,
        req: LnPayRequest,
        context: RequestContext,
    ) -> Result<LnPayResponse, AppError> {
        self.pay_invoice_with_resolver(req, context, None).await
    }

    /// Pay a lightning invoice with optional payment info resolver
    pub async fn pay_invoice_with_resolver(
        &self,
        mut req: LnPayRequest,
        context: RequestContext,
        resolver: Option<&dyn PaymentInfoResolver>,
    ) -> Result<LnPayResponse, AppError> {
        use crate::observability::{sanitize_invoice, sanitize_preimage};

        let client = self.get_client(req.federation_id).await?;

        // Use resolver if provided to handle non-Bolt11 payment info
        if let Some(resolver) = resolver {
            if let Some(resolved_invoice) = resolver
                .resolve_payment_info(
                    &req.payment_info,
                    req.amount_msat,
                    req.lnurl_comment.as_deref(),
                )
                .await?
            {
                req.payment_info = resolved_invoice;
            }
        }

        // Parse invoice - after resolution, this should be a bolt11 invoice
        use std::str::FromStr;

        use fedimint_ln_common::lightning_invoice::Bolt11Invoice;

        let bolt11 = Bolt11Invoice::from_str(req.payment_info.trim()).map_err(|e| {
            error!(error = ?e, "Failed to parse invoice after resolution");
            AppError::validation_error(format!("Invalid bolt11 invoice: {}", e))
                .with_context(context.clone())
        })?;

        // Validate invoice amount
        if bolt11.amount_milli_satoshis().is_none() {
            return Err(AppError::validation_error("Invoice must have an amount")
                .with_context(context.clone()));
        }
        if req.amount_msat.is_some() && bolt11.amount_milli_satoshis().is_some() {
            return Err(
                AppError::validation_error("Amount specified in both invoice and request")
                    .with_context(context.clone()),
            );
        }

        // Initialize payment tracker
        let mut payment_tracker = PaymentTracker::new(
            req.federation_id,
            &bolt11.to_string(),
            req.amount_msat.map(|a| a.msats).unwrap_or(0),
            self.event_bus.clone(),
            Some(context.clone()),
        );

        info!(
            invoice = %sanitize_invoice(&bolt11),
            payment_id = %payment_tracker.payment_id(),
            "Processing lightning payment"
        );

        // Track payment initiation
        payment_tracker
            .initiate(
                bolt11.to_string(),
                req.amount_msat.map(|a| a.msats).unwrap_or(0),
            )
            .await;

        let tracked_amount = req
            .amount_msat
            .unwrap_or_else(|| Amount::from_msats(bolt11.amount_milli_satoshis().unwrap_or(0)));

        let payment_result = self
            .lightning_service
            .pay_invoice(
                &client,
                req.federation_id,
                req.gateway_id,
                bolt11,
                req.amount_msat,
            )
            .await
            .map_err(|e| {
                error!(error = ?e, payment_id = %payment_tracker.payment_id(), "Payment failed during execution");
                AppError::new(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    anyhow!(e.to_string()),
                )
                    .with_context(context.clone())
            })?;
        let operation_id = payment_result.operation_id;
        let protocol = payment_result.protocol;

        if let Some(ref payment_lifecycle_manager) = self.payment_lifecycle_manager {
            if let Err(e) = payment_lifecycle_manager
                .track_lightning_pay(
                    operation_id,
                    req.federation_id,
                    tracked_amount,
                    Some(payment_result.fee.msats),
                    tracked_lightning_metadata(protocol.as_str(), "ln_pay", None),
                    Some(context.correlation_id.clone()),
                )
                .await
            {
                warn!(
                    operation_id = ?operation_id,
                    error = ?e,
                    "Failed to register outgoing payment with payment lifecycle manager"
                );
            }
        }

        let preimage = payment_result.preimage.clone();

        // Track successful payment
        payment_tracker
            .succeed(
                preimage.clone(),
                req.amount_msat.map(|a| a.msats).unwrap_or(0),
                payment_result.fee.msats,
            )
            .await;

        info!(
            payment_id = %payment_tracker.payment_id(),
            preimage = %sanitize_preimage(&preimage),
            "Payment completed successfully"
        );

        Ok(LnPayResponse {
            operation_id,
            payment_type: payment_result.payment_type,
            contract_id: payment_result.contract_id,
            fee: payment_result.fee,
            preimage,
            protocol: Some(protocol.as_str().to_string()),
        })
    }

    /// Start automatic monitoring for an invoice
    async fn start_invoice_monitoring(
        &self,
        client: ClientHandleArc,
        operation_id: OperationId,
        invoice_id: String,
        amount_msat: u64,
        invoice_tracker: InvoiceTracker,
    ) {
        let timeout = Duration::from_secs(24 * 60 * 60); // 24 hours max timeout

        tokio::spawn(async move {
            if let Err(e) = Self::monitor_invoice_settlement(
                client,
                operation_id,
                invoice_id.clone(),
                amount_msat,
                timeout,
                invoice_tracker,
            )
            .await
            {
                error!(
                    operation_id = ?operation_id,
                    invoice_id = %invoice_id,
                    error = ?e,
                    "Failed to automatically monitor invoice settlement"
                );
            }
        });
    }

    /// Monitor invoice settlement using fedimint's subscribe_ln_receive
    async fn monitor_invoice_settlement(
        client: ClientHandleArc,
        operation_id: OperationId,
        invoice_id: String,
        amount_msat: u64,
        timeout: Duration,
        invoice_tracker: InvoiceTracker,
    ) -> anyhow::Result<()> {
        use fedimint_ln_client::LnReceiveState;
        use futures_util::StreamExt;

        let lightning_module = client.get_first_module::<LightningClientModule>()?;

        let mut updates = lightning_module
            .subscribe_ln_receive(operation_id)
            .await?
            .into_stream();

        info!(
            operation_id = ?operation_id,
            invoice_id = %invoice_id,
            timeout_secs = timeout.as_secs(),
            "Started automatic invoice settlement monitoring"
        );

        let timeout_future = tokio::time::sleep(timeout);
        tokio::pin!(timeout_future);

        loop {
            tokio::select! {
                update = updates.next() => {
                    match update {
                        Some(LnReceiveState::Claimed) => {
                            info!(
                                operation_id = ?operation_id,
                                invoice_id = %invoice_id,
                                "Invoice settled - publishing event to event bus"
                            );

                            // Publish invoice paid event to event bus
                            invoice_tracker.paid(amount_msat).await;
                            break;
                        }
                        Some(LnReceiveState::Canceled { reason }) => {
                            warn!(
                                operation_id = ?operation_id,
                                invoice_id = %invoice_id,
                                reason = %reason,
                                "Invoice canceled - publishing event to event bus"
                            );

                            // Publish invoice expiration/cancellation event to event bus
                            invoice_tracker.expired().await;
                            break;
                        }
                        Some(state) => {
                            info!(
                                operation_id = ?operation_id,
                                invoice_id = %invoice_id,
                                state = ?state,
                                "Invoice status update - continuing automatic monitoring"
                            );
                            continue;
                        }
                        None => {
                            warn!(
                                operation_id = ?operation_id,
                                invoice_id = %invoice_id,
                                "Automatic monitoring stream ended unexpectedly"
                            );
                            break;
                        }
                    }
                }
                _ = &mut timeout_future => {
                    warn!(
                        operation_id = ?operation_id,
                        invoice_id = %invoice_id,
                        timeout_secs = timeout.as_secs(),
                        "Invoice settlement monitoring timed out"
                    );
                    break;
                }
            }
        }

        info!(
            operation_id = ?operation_id,
            invoice_id = %invoice_id,
            "Automatic invoice settlement monitoring completed"
        );

        Ok(())
    }
}
