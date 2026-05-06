//! Shared error envelope for the `/api/v1/*` REST surface.
//!
//! Every response emitted by Plan 0+ carrier-API handlers uses the same
//! JSON shape: `{"error": "<message>", "code": "<machine_code>"}`. Handlers
//! return `ApiResult<T>`; Axum's `IntoResponse` renders it into an HTTP
//! response with the correct status code and JSON body.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: msg.into(),
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: msg.into(),
        }
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "conflict",
            message: msg.into(),
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: msg.into(),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: msg.into(),
        }
    }

    pub fn unavailable(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "unavailable",
            message: msg.into(),
        }
    }

    pub fn not_implemented(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            code: "not_implemented",
            message: msg.into(),
        }
    }

    /// 400 Bad Request with `code: "not_supported"`.
    ///
    /// Used by pre-dispatch feature probes (Phase 4 Plan 04-04) when the
    /// request shape is valid but the addressed feature is not wired in the
    /// current build — e.g. `/play` with a `url` source, or `/speak` (TTS
    /// not wired). Returns 400 (not 501) because the request is malformed
    /// for THIS deployment: the operator can fix it by switching to a
    /// supported variant.
    pub fn not_supported(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "not_supported",
            message: msg.into(),
        }
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden_cross_account",
            message: msg.into(),
        }
    }

    /// 410 Gone with `code: "recording_missing"`.
    ///
    /// Phase 12 D-14: returned by `/api/v1/recordings/{id}/download` when
    /// the CDR row says `recording_url` is local but the file is absent
    /// from disk. The CDR exists; the file does not — distinct from 404.
    pub fn gone(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GONE,
            code: "recording_missing",
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": self.message, "code": self.code})),
        )
            .into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

impl From<crate::handler::api_v1::reload_steps::ReloadStepError> for ApiError {
    fn from(err: crate::handler::api_v1::reload_steps::ReloadStepError) -> Self {
        ApiError::internal(err.to_string())
    }
}

#[cfg(test)]
mod gone_tests {
    use super::*;
    #[test]
    fn gone_constructs_410_with_recording_missing_code() {
        let e = ApiError::gone("file deleted");
        assert_eq!(e.status, StatusCode::GONE);
        assert_eq!(e.code, "recording_missing");
        assert_eq!(e.message, "file deleted");
    }
}
