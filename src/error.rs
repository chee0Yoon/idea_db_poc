//! API error type and JSON rendering. See `docs/api.md` §2.1.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{json, Value};

/// A single domain validation issue. `code` values are a stable contract
/// documented in `docs/api.md` §6.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Issue {
    pub path: String,
    pub code: &'static str,
    pub message: String,
}

impl Issue {
    pub fn new(path: impl Into<String>, code: &'static str, message: impl Into<String>) -> Self {
        Issue {
            path: path.into(),
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn unsupported_protocol_version(got: u32) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_protocol_version",
            format!("protocol_version {got} is not supported, expected 1"),
        )
    }

    pub fn validation(issues: Vec<Issue>) -> Self {
        let message = issues
            .first()
            .map(|i| format!("{}: {}", i.path, i.message))
            .unwrap_or_else(|| "validation failed".to_string());
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            message,
        )
        .with_details(json!({ "issues": issues }))
    }

    pub fn head_conflict(details: Value) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "head_conflict",
            "expected head does not match current head",
        )
        .with_details(details)
    }

    pub fn idempotency_conflict(key: &str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            format!("idempotency_key {key} was already used with a different request body"),
        )
    }

    pub fn import_not_empty() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "import_not_empty",
            "import requires an empty namespace",
        )
    }

    pub fn digest_mismatch(expected: &str, actual: &str) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "digest_mismatch",
            format!("document digest {expected} does not match recomputed {actual}"),
        )
    }

    pub fn context_too_large(loaded: usize, limit: usize) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "context_too_large",
            format!(
                "validation context needs more than {limit} existing records (loaded {loaded})"
            ),
        )
    }

    pub fn not_ready(last_error: Option<String>) -> Self {
        let mut err = Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
            "neo4j is not reachable or not initialised",
        );
        if let Some(e) = last_error {
            err.details = Some(json!({ "last_error": e }));
        }
        err
    }

    pub fn storage(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "storage_error", message)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "code": self.code, "message": self.message });
        if let Some(details) = self.details {
            body["details"] = details;
        }
        (self.status, axum::Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
