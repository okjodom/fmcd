#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::{Method, StatusCode};
    use axum::middleware::from_fn;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    use crate::observability::correlation::{
        request_id_middleware, CORRELATION_ID_HEADER, REQUEST_ID_HEADER,
    };

    async fn ok_handler() -> StatusCode {
        StatusCode::OK
    }

    #[tokio::test]
    async fn test_request_id_middleware_with_correlation_id() {
        let app = Router::new()
            .route("/test", get(ok_handler))
            .layer(from_fn(request_id_middleware));

        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header(CORRELATION_ID_HEADER, "test-correlation-123")
            .body(Body::empty())
            .expect("Failed to build test request");

        let response = app
            .oneshot(request)
            .await
            .expect("Middleware should succeed");
        assert_eq!(response.status(), StatusCode::OK);

        // Check headers are set
        let correlation_header = response
            .headers()
            .get(CORRELATION_ID_HEADER)
            .expect("Correlation ID header should be present");
        assert_eq!(correlation_header, "test-correlation-123");

        let request_header = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .expect("Request ID header should be present");
        assert!(!request_header.is_empty());
    }

    #[tokio::test]
    async fn test_request_id_middleware_without_correlation_id() {
        let app = Router::new()
            .route("/api/test", get(ok_handler))
            .layer(from_fn(request_id_middleware));

        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/test")
            .body(Body::empty())
            .expect("Failed to build test request");

        let response = app
            .oneshot(request)
            .await
            .expect("Middleware should succeed");
        assert_eq!(response.status(), StatusCode::OK);

        // Both headers should be present
        let correlation_header = response
            .headers()
            .get(CORRELATION_ID_HEADER)
            .expect("Correlation ID should be generated");
        let request_header = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .expect("Request ID should be generated");

        assert!(!correlation_header.is_empty());
        assert!(!request_header.is_empty());
        // They should be different values
        assert_ne!(correlation_header, request_header);
    }

    #[tokio::test]
    async fn test_invalid_correlation_id_rejected() {
        let app = Router::new()
            .route("/test", get(ok_handler))
            .layer(from_fn(request_id_middleware));

        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header(CORRELATION_ID_HEADER, "invalid-id-with-@#$%")
            .body(Body::empty())
            .expect("Failed to build test request");

        let response = app.oneshot(request).await.expect("Request should complete");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_too_long_correlation_id_rejected() {
        let long_id = "a".repeat(201); // Exceeds MAX_CORRELATION_ID_LENGTH
        let app = Router::new()
            .route("/test", get(ok_handler))
            .layer(from_fn(request_id_middleware));

        let request = Request::builder()
            .method(Method::GET)
            .uri("/test")
            .header(CORRELATION_ID_HEADER, long_id)
            .body(Body::empty())
            .expect("Failed to build test request");

        let response = app.oneshot(request).await.expect("Request should complete");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
