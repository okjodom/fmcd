use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::Utc;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_core::Amount;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Type of tracked payment-related operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PaymentType {
    LightningReceive,
    LightningPay,
    OnchainDeposit,
    OnchainWithdraw,
}

/// Normalized information about a tracked payment-related operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentOperation {
    pub operation_id: OperationId,
    pub federation_id: FederationId,
    pub payment_type: PaymentType,
    pub amount_msat: Option<Amount>,
    pub created_at: chrono::DateTime<Utc>,
    pub metadata: Option<serde_json::Value>,
    pub correlation_id: Option<String>,
    /// Track if we've already attempted to claim the ecash.
    pub claim_attempted: bool,
    /// Track if ecash was successfully claimed.
    pub ecash_claimed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationStoreStats {
    pub total_active_operations: usize,
    pub operations_by_type: HashMap<PaymentType, usize>,
    pub operations_by_federation: HashMap<FederationId, usize>,
}

/// Authoritative owner of in-memory tracked operation state.
#[derive(Debug, Clone)]
pub struct OperationStore {
    max_operations_per_federation: usize,
    operations: Arc<RwLock<HashMap<OperationId, PaymentOperation>>>,
}

impl OperationStore {
    pub fn new(max_operations_per_federation: usize) -> Self {
        Self {
            max_operations_per_federation,
            operations: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn insert(&self, operation: PaymentOperation) -> Result<()> {
        let mut operations = self.operations.write().await;
        let federation_count = operations
            .values()
            .filter(|op| op.federation_id == operation.federation_id)
            .count();

        if !operations.contains_key(&operation.operation_id)
            && federation_count >= self.max_operations_per_federation
        {
            return Err(anyhow!(
                "Federation {} has reached maximum operations limit ({})",
                operation.federation_id,
                self.max_operations_per_federation
            ));
        }

        operations.insert(operation.operation_id, operation);
        Ok(())
    }

    pub async fn snapshot(&self) -> HashMap<OperationId, PaymentOperation> {
        self.operations.read().await.clone()
    }

    pub async fn upsert(&self, operation: PaymentOperation) {
        self.operations
            .write()
            .await
            .insert(operation.operation_id, operation);
    }

    pub async fn remove(&self, operation_id: &OperationId) -> Option<PaymentOperation> {
        self.operations.write().await.remove(operation_id)
    }

    pub async fn remove_many(&self, operation_ids: &[OperationId]) {
        let mut operations = self.operations.write().await;
        for operation_id in operation_ids {
            operations.remove(operation_id);
        }
    }

    pub async fn stats(&self) -> OperationStoreStats {
        let operations = self.operations.read().await;
        let mut by_type: HashMap<PaymentType, usize> = HashMap::new();
        let mut by_federation: HashMap<FederationId, usize> = HashMap::new();

        for operation in operations.values() {
            *by_type.entry(operation.payment_type.clone()).or_insert(0) += 1;
            *by_federation.entry(operation.federation_id).or_insert(0) += 1;
        }

        OperationStoreStats {
            total_active_operations: operations.len(),
            operations_by_type: by_type,
            operations_by_federation: by_federation,
        }
    }
}
