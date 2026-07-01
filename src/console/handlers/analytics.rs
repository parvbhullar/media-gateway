//! Analytics console page (analytics-console-page) — the CDR report with
//! filters, rendered as cards + tables. Data comes from the shared
//! `compute_cdr_report` (same path as `/api/v1/cdrs/summary`), so the console
//! view and the carrier API never drift.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;

use crate::console::ConsoleState;
use crate::console::middleware::AuthRequired;
use crate::handler::api_v1::cdrs::{CdrSummaryQuery, compute_cdr_report};

/// Render the analytics page shell (Alpine fetches the data on load).
pub async fn analytics_page(
    State(state): State<Arc<ConsoleState>>,
    headers: HeaderMap,
    AuthRequired(user): AuthRequired,
) -> Response {
    let current_user = state.build_current_user_ctx(&user).await;
    state.render_with_headers(
        "console/analytics.html",
        json!({
            "nav_active": "analytics",
            "current_user": current_user,
        }),
        &headers,
    )
}

/// Report JSON for the page (session-authenticated).
pub async fn analytics_data(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_): AuthRequired,
    Query(q): Query<CdrSummaryQuery>,
) -> Response {
    match compute_cdr_report(state.db(), &q).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": format!("failed to build report: {e}") })),
        )
            .into_response(),
    }
}

/// CSV export of the per-number and international rows for the filtered window.
pub async fn analytics_export_csv(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_): AuthRequired,
    Query(q): Query<CdrSummaryQuery>,
) -> Response {
    let report = match compute_cdr_report(state.db(), &q).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to build report: {e}"),
            )
                .into_response();
        }
    };

    let mut out = String::new();
    out.push_str(
        "section,number,total,connected,answered,failed,conn_pct,talk_minutes,distinct_dests\n",
    );
    for n in &report.per_number {
        out.push_str(&format!(
            "number,{},{},{},{},{},{},{},\n",
            csv_quote(&n.number),
            n.total, n.connected, n.answered, n.failed, n.conn_pct, n.talk_minutes
        ));
    }
    for i in &report.international {
        out.push_str(&format!(
            "international,{},{},,{},{},,{},{}\n",
            csv_quote(&i.origin),
            i.total, i.answered, i.failed, i.talk_minutes, i.distinct_dests
        ));
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"cdr-analytics.csv\"",
            ),
        ],
        out,
    )
        .into_response()
}

/// RFC 4180 CSV quoting: strips control characters that break row structure
/// (CRLF folds from SIP headers would split a row in most spreadsheet tools
/// even inside quotes), then wraps in double-quotes and escapes embedded `"` as `""`.
fn csv_quote(s: &str) -> String {
    let sanitized = s.replace(['\r', '\n', '\x00'], " ");
    format!("\"{}\"", sanitized.replace('"', "\"\""))
}

/// Page routes (nested under base_path).
pub fn urls() -> axum::Router<Arc<ConsoleState>> {
    axum::Router::new()
        .route("/analytics", get(analytics_page))
        .route("/analytics/data", get(analytics_data))
        .route("/analytics/export.csv", get(analytics_export_csv))
}
