use std::collections::HashMap;
use std::path::PathBuf;
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

/// Normalized status for tracked operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OperationStatus {
    Created,
    Pending,
    Succeeded,
    Failed,
    Refunded,
    TimedOut,
}

impl OperationStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OperationStatus::Succeeded
                | OperationStatus::Failed
                | OperationStatus::Refunded
                | OperationStatus::TimedOut
        )
    }
}

/// Normalized information about a tracked payment-related operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentOperation {
    pub operation_id: OperationId,
    pub federation_id: FederationId,
    pub payment_type: PaymentType,
    pub amount_msat: Option<Amount>,
    pub fee_msat: Option<u64>,
    pub status: OperationStatus,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    pub metadata: Option<serde_json::Value>,
    pub last_error: Option<String>,
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
    storage_path: PathBuf,
    operations: Arc<RwLock<HashMap<OperationId, PaymentOperation>>>,
}

impl OperationStore {
    pub fn new(max_operations_per_federation: usize, storage_path: PathBuf) -> Self {
        let operations = Self::load_from_disk(&storage_path).unwrap_or_default();
        Self {
            max_operations_per_federation,
            storage_path,
            operations: Arc::new(RwLock::new(operations)),
        }
    }

    pub async fn insert(&self, operation: PaymentOperation) -> Result<()> {
        let mut operations = self.operations.write().await;
        let federation_count = operations
            .values()
            .filter(|op| !op.status.is_terminal())
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
        self.persist_locked(&operations)?;
        Ok(())
    }

    pub async fn snapshot(&self) -> HashMap<OperationId, PaymentOperation> {
        self.operations.read().await.clone()
    }

    pub async fn active_snapshot(&self) -> HashMap<OperationId, PaymentOperation> {
        self.operations
            .read()
            .await
            .iter()
            .filter(|(_, operation)| !operation.status.is_terminal())
            .map(|(operation_id, operation)| (*operation_id, operation.clone()))
            .collect()
    }

    pub async fn get(&self, operation_id: &OperationId) -> Option<PaymentOperation> {
        self.operations.read().await.get(operation_id).cloned()
    }

    pub async fn list_by_federation(
        &self,
        federation_id: FederationId,
        limit: usize,
    ) -> Vec<PaymentOperation> {
        let mut operations = self
            .operations
            .read()
            .await
            .values()
            .filter(|op| op.federation_id == federation_id)
            .cloned()
            .collect::<Vec<_>>();
        operations.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        operations.truncate(limit);
        operations
    }

    pub async fn upsert(&self, operation: PaymentOperation) -> Result<()> {
        let mut operations = self.operations.write().await;
        operations.insert(operation.operation_id, operation);
        self.persist_locked(&operations)?;
        Ok(())
    }

    pub async fn remove(&self, operation_id: &OperationId) -> Option<PaymentOperation> {
        let mut operations = self.operations.write().await;
        let removed = operations.remove(operation_id);
        if removed.is_some() {
            let _ = self.persist_locked(&operations);
        }
        removed
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

        for operation in operations
            .values()
            .filter(|operation| !operation.status.is_terminal())
        {
            *by_type.entry(operation.payment_type.clone()).or_insert(0) += 1;
            *by_federation.entry(operation.federation_id).or_insert(0) += 1;
        }

        OperationStoreStats {
            total_active_operations: operations.len(),
            operations_by_type: by_type,
            operations_by_federation: by_federation,
        }
    }

    fn load_from_disk(path: &PathBuf) -> Result<HashMap<OperationId, PaymentOperation>> {
        if !path.exists() {
            return Ok(HashMap::new());
        }

        let contents = std::fs::read_to_string(path)?;
        if contents.trim().is_empty() {
            return Ok(HashMap::new());
        }

        serde_json::from_str(&contents)
            .map_err(|e| anyhow!("Failed to deserialize operation store at {:?}: {}", path, e))
    }

    fn persist_locked(&self, operations: &HashMap<OperationId, PaymentOperation>) -> Result<()> {
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(operations)?;
        std::fs::write(&self.storage_path, contents)?;
        Ok(())
    }
}
