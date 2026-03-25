// Integration test to demonstrate the observability features
// This file would be used for integration tests, not unit tests

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::{Method, StatusCode};
    use axum::middleware::from_fn;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;
    use tower::ServiceExt;
    use tracing_test::traced_test;

    use crate::error::{AppError, ErrorCategory};
    use crate::observability::{request_id_middleware, LoggingConfig, RequestContext};

    // Mock handler that demonstrates enhanced error handling
    async fn mock_handler(
        axum::extract::Extension(context): axum::extract::Extension<RequestContext>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        tracing::info!(
            correlation_id = %context.correlation_id,
            request_id = %context.request_id,
            "Processing mock request"
        );

        // Simulate some business logic
        tokio::time::sleep(Duration::from_millis(10)).await;

        Ok(Json(json!({
            "status": "success",
            "correlation_id": context.correlation_id,
            "request_id": context.request_id,
            "data": "mock response"
        })))
    }

    // Mock error handler to test error categories
    async fn mock_error_handler() -> Result<Json<serde_json::Value>, AppError> {
        Err(AppError::with_category(
            ErrorCategory::ValidationError,
            "Mock validation error for testing",
        )
        .with_details(json!({
            "field": "test_field",
            "reason": "invalid format"
        })))
    }

    #[traced_test]
    #[tokio::test]
    async fn test_correlation_id_flow() {
        // Create a router with observability middleware
        let app = Router::new()
            .route("/test", get(mock_handler))
            .route("/error", get(mock_error_handler))
            .layer(from_fn(request_id_middleware));

        // Test with correlation ID in request
        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header("X-Correlation-Id", "test-correlation-123")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();

        // Verify response has correlation and request IDs
        assert!(response.headers().contains_key("X-Correlation-Id"));
        assert!(response.headers().contains_key("X-Request-Id"));
        assert_eq!(
            response.headers().get("X-Correlation-Id").unwrap(),
            "test-correlation-123"
        );
    }

    #[traced_test]
    #[tokio::test]
    async fn test_error_handling_with_context() {
        let app = Router::new()
            .route("/error", get(mock_error_handler))
            .layer(from_fn(request_id_middleware));

        let request = Request::builder()
            .method(Method::GET)
            .uri("/error")
            .header("X-Correlation-Id", "error-test-456")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Should be 400 Bad Request for validation error
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // Should have correlation ID in headers
        assert_eq!(
            response.headers().get("X-Correlation-Id").unwrap(),
            "error-test-456"
        );
    }

    #[traced_test]
    #[tokio::test]
    async fn test_request_without_correlation_id() {
        let app = Router::new()
            .route("/test", get(mock_handler))
            .layer(from_fn(request_id_middleware));

        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Should generate both correlation ID and request ID
        let correlation_id = response.headers().get("X-Correlation-Id").unwrap();
        let request_id = response.headers().get("X-Request-Id").unwrap();

        // Both should be present and different
        assert!(!correlation_id.is_empty());
        assert!(!request_id.is_empty());
        assert_ne!(correlation_id, request_id);
    }

    #[test]
    fn test_error_category_mapping() {
        // Test that error categories map to correct HTTP status codes
        assert_eq!(
            ErrorCategory::ValidationError.status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ErrorCategory::AuthenticationError.status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(ErrorCategory::NotFound.status_code(), StatusCode::NOT_FOUND);
        assert_eq!(
            ErrorCategory::InternalError.status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );

        // Test error codes
        assert_eq!(
            ErrorCategory::ValidationError.error_code(),
            "VALIDATION_ERROR"
        );
        assert_eq!(
            ErrorCategory::InsufficientFunds.error_code(),
            "INSUFFICIENT_FUNDS"
        );
        assert_eq!(ErrorCategory::GatewayError.error_code(), "GATEWAY_ERROR");
    }

    #[test]
    fn test_logging_config() {
        // Test default configuration
        let config = LoggingConfig::default();
        assert_eq!(config.level, "info");
        assert!(config.console_output);
        assert!(config.file_output);

        // Test that we can create custom config
        let custom_config = LoggingConfig {
            level: "debug".to_string(),
            console_output: false,
            file_output: true,
            log_dir: std::path::PathBuf::from("/tmp/test-logs"),
            ..Default::default()
        };

        assert_eq!(custom_config.level, "debug");
        assert!(!custom_config.console_output);
        assert!(custom_config.file_output);
    }
}
