#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use anyhow::anyhow;
    use axum::http::StatusCode;

    use super::super::*;

    #[test]
    fn test_app_error_new() {
        let error = AppError::new(StatusCode::NOT_FOUND, anyhow!("Not found"));
        assert_eq!(error.category.status_code(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_app_error_from_anyhow() {
        let anyhow_error = anyhow!("Test error");
        let app_error = AppError::from(anyhow_error);
        assert_eq!(
            app_error.category.status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn test_app_error_with_different_status_codes() {
        let error1 = AppError::new(StatusCode::BAD_REQUEST, anyhow!("Bad request"));
        assert_eq!(error1.category.status_code(), StatusCode::BAD_REQUEST);

        let error2 = AppError::new(StatusCode::UNAUTHORIZED, anyhow!("Unauthorized"));
        assert_eq!(error2.category.status_code(), StatusCode::UNAUTHORIZED);

        let error3 = AppError::new(StatusCode::FORBIDDEN, anyhow!("Forbidden"));
        assert_eq!(error3.category.status_code(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_app_error_display() {
        let error = AppError::new(StatusCode::NOT_FOUND, anyhow!("Resource not found"));
        let display_string = format!("{}", error);
        assert!(display_string.contains("Resource not found"));
    }

    #[test]
    fn test_app_error_into_response() {
        use axum::response::IntoResponse;

        let app_error = AppError::new(StatusCode::FORBIDDEN, anyhow!("Forbidden"));
        let response = app_error.into_response();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
