#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::str::FromStr;

    use chrono::{Duration, Utc};
    use fedimint_core::config::FederationId;
    use fedimint_core::core::OperationId;
    use fedimint_core::Amount;
    use tempfile::tempdir;

    use crate::core::operations::{OperationStatus, OperationStore, PaymentOperation, PaymentType};

    fn test_federation_id() -> FederationId {
        FederationId::dummy()
    }

    fn test_operation_id(seed: u8) -> OperationId {
        OperationId::from_str(&format!("{:02x}", seed).repeat(32)).expect("valid operation id")
    }

    fn tracked_operation(
        operation_id: OperationId,
        created_at: chrono::DateTime<Utc>,
        status: OperationStatus,
    ) -> PaymentOperation {
        PaymentOperation {
            operation_id,
            federation_id: test_federation_id(),
            payment_type: PaymentType::LightningPay,
            amount_msat: Some(Amount::from_msats(1_000)),
            fee_msat: Some(10),
            status,
            created_at,
            updated_at: created_at,
            metadata: None,
            last_error: None,
            correlation_id: Some("test-correlation".to_string()),
            claim_attempted: false,
            ecash_claimed: false,
        }
    }

    #[tokio::test]
    async fn test_list_by_federation_returns_newest_first() {
        let temp_dir = tempdir().expect("temp dir");
        let store = OperationStore::new(10, temp_dir.path().join("operation_store.json"));
        let now = Utc::now();

        store
            .insert(tracked_operation(
                test_operation_id(1),
                now - Duration::minutes(5),
                OperationStatus::Succeeded,
            ))
            .await
            .expect("insert oldest op");
        store
            .insert(tracked_operation(
                test_operation_id(2),
                now,
                OperationStatus::Pending,
            ))
            .await
            .expect("insert newest op");

        let operations = store.list_by_federation(test_federation_id(), 10).await;

        assert_eq!(operations.len(), 2);
        assert_eq!(operations[0].operation_id, test_operation_id(2));
        assert_eq!(operations[1].operation_id, test_operation_id(1));
    }

    #[tokio::test]
    async fn test_operation_store_persists_and_reloads_history() {
        let temp_dir = tempdir().expect("temp dir");
        let storage_path = temp_dir.path().join("operation_store.json");
        let now = Utc::now();

        let store = OperationStore::new(10, storage_path.clone());
        store
            .insert(tracked_operation(
                test_operation_id(9),
                now,
                OperationStatus::TimedOut,
            ))
            .await
            .expect("persist operation");

        let reloaded_store = OperationStore::new(10, storage_path);
        let operation = reloaded_store
            .get(&test_operation_id(9))
            .await
            .expect("operation to reload");

        assert_eq!(operation.status, OperationStatus::TimedOut);
        assert_eq!(operation.fee_msat, Some(10));
        assert_eq!(
            operation.correlation_id.as_deref(),
            Some("test-correlation")
        );
    }
}
