//! Async HTTP POST fetcher for the application `answer_url`.
//!
//! When a call arrives for an application, this module POSTs call metadata to
//! the application's `answer_url` and returns the raw response body (TwiML).
//! The caller is responsible for parsing the body with [`crate::call::app::twiml`].
//!
//! # Auth headers
//!
//! The application's `auth_headers` column holds a JSON object whose keys are
//! HTTP header names and whose values must be strings.  Non-string values are
//! silently skipped.
//!
//! # Timeout
//!
//! The timeout is taken from `app.answer_timeout_ms` (i32 → u64 millis).
//! If the value is ≤ 0 a default of 5 000 ms is used.

use std::time::Duration;

use reqwest::Client;
use serde_json::Value as JsonValue;
use thiserror::Error;

/// Errors produced by [`fetch_answer_url`].
#[derive(Debug, Error)]
pub enum AnswerUrlError {
    #[error("answer_url request timed out")]
    Timeout,
    #[error("answer_url returned HTTP {0}")]
    HttpStatus(u16),
    #[error("answer_url request failed: {0}")]
    Request(String),
}

impl AnswerUrlError {
    /// Map the error to a CDR `failure_reason` string.
    pub fn cdr_failure_reason(&self) -> &'static str {
        match self {
            AnswerUrlError::Timeout => "answer_url_timeout",
            AnswerUrlError::HttpStatus(_) => "answer_url_http_error",
            AnswerUrlError::Request(_) => "answer_url_request_error",
        }
    }
}

/// Parameters describing the incoming call sent to the answer URL.
#[derive(Debug, Clone)]
pub struct AnswerUrlParams<'a> {
    pub caller: &'a str,
    pub callee: &'a str,
    pub call_id: &'a str,
    pub application_id: &'a str,
    pub account_id: &'a str,
    pub direction: &'a str,
}

/// POST call metadata to `url` and return the response body on success.
///
/// # Arguments
///
/// - `url` — the application's `answer_url`
/// - `auth_headers` — JSON object from `supersip_applications.auth_headers`
/// - `timeout_ms` — from `app.answer_timeout_ms`; clamped to ≥ 1 000 ms
/// - `params` — call context fields sent as `application/x-www-form-urlencoded`
pub async fn fetch_answer_url(
    url: &str,
    auth_headers: &JsonValue,
    timeout_ms: i32,
    params: &AnswerUrlParams<'_>,
) -> Result<String, AnswerUrlError> {
    let effective_timeout = if timeout_ms > 0 {
        timeout_ms as u64
    } else {
        5_000_u64
    };

    let client = Client::builder()
        .timeout(Duration::from_millis(effective_timeout))
        .build()
        .map_err(|e| AnswerUrlError::Request(e.to_string()))?;

    let mut request = client.post(url).form(&[
        ("caller", params.caller),
        ("callee", params.callee),
        ("call_id", params.call_id),
        ("application_id", params.application_id),
        ("account_id", params.account_id),
        ("direction", params.direction),
    ]);

    // Apply auth_headers from the JSON object
    if let Some(obj) = auth_headers.as_object() {
        for (key, value) in obj {
            if let Some(v) = value.as_str() {
                request = request.header(key.as_str(), v);
            }
            // Non-string values are silently skipped
        }
    }

    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            AnswerUrlError::Timeout
        } else {
            AnswerUrlError::Request(e.to_string())
        }
    })?;

    let status = response.status();
    if !status.is_success() {
        return Err(AnswerUrlError::HttpStatus(status.as_u16()));
    }

    response
        .text()
        .await
        .map_err(|e| AnswerUrlError::Request(e.to_string()))
}
