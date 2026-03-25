#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use fedimint_core::config::FederationId;
    use tempfile::TempDir;

    use super::super::*;

    async fn create_test_state() -> AppState {
        let temp_dir = TempDir::new().unwrap();
        let work_dir = temp_dir.path().to_path_buf();
        AppState::new(work_dir).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_app_state_new() {
        let temp_dir = TempDir::new().unwrap();
        let work_dir = temp_dir.path().to_path_buf();

        let state = AppState::new(work_dir.clone()).await;
        assert!(state.is_ok(), "Should create AppState successfully");

        let state = state.unwrap();
        assert_eq!(state.multimint().clients.lock().await.len(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_get_client_not_found() {
        let state = create_test_state().await;
        let federation_id = FederationId::dummy();

        let result = state.get_client(federation_id).await;
        assert!(
            result.is_err(),
            "Should return error for non-existent federation"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_multimint_clients_empty() {
        let state = create_test_state().await;
        let clients = state.multimint().clients.lock().await;

        assert!(clients.is_empty(), "Should have no clients initially");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_get_client_by_prefix() {
        let state = create_test_state().await;
        let federation_id = FederationId::dummy();
        let prefix = federation_id.to_prefix();

        let result = state.get_client_by_prefix(&prefix).await;
        assert!(
            result.is_err(),
            "Should return error for non-existent federation"
        );
    }
}
