//! IT-04: E2E smoke test — answer_url fetch + TwiML parse + EntryAction output.
//!
//! Spins up a wiremock server that returns canned TwiML, then calls
//! `fetch_answer_url` + `parse_twiml` directly (no HTTP router involved) and
//! asserts:
//!  1. The mock endpoint was hit with the expected form fields.
//!  2. At least one `EntryAction` is returned.
//!  3. The first action is a `Play` action with `prompt_text = "Hello from TwiML"`.

use rustpbx::call::app::{
    answer_url::{AnswerUrlParams, fetch_answer_url},
    twiml::parse_twiml,
};
use rustpbx::call::app::ivr_config::EntryAction;
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Canned TwiML returned by the mock webhook endpoint.
const CANNED_TWIML: &str =
    r#"<Response><Say voice="alice">Hello from TwiML</Say><Hangup/></Response>"#;

#[tokio::test]
async fn answer_url_fetch_executes_verbs() {
    // ── 1. Start wiremock ───────────────────────────────────────────────────
    let mock_server = MockServer::start().await;

    // Mount POST /twiml → 200 + CANNED_TWIML with Content-Type text/xml
    Mock::given(method("POST"))
        .and(path("/twiml"))
        // Verify that caller and application_id fields are present in the body
        .and(body_string_contains("caller="))
        .and(body_string_contains("application_id="))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(CANNED_TWIML)
                .insert_header("content-type", "text/xml"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    // ── 2. Build call params ────────────────────────────────────────────────
    let answer_url = format!("{}/twiml", mock_server.uri());
    let auth_headers = json!({});

    let params = AnswerUrlParams {
        caller: "+15550001111",
        callee: "+15550002222",
        call_id: "call-test-it04",
        application_id: "app-uuid-1234",
        account_id: "acme",
        direction: "inbound",
    };

    // ── 3. Fetch answer URL ─────────────────────────────────────────────────
    let twiml_body = fetch_answer_url(&answer_url, &auth_headers, 5000, &params)
        .await
        .expect("fetch_answer_url should succeed against wiremock");

    // ── 4. Parse TwiML → EntryActions ───────────────────────────────────────
    let actions = parse_twiml(&twiml_body).expect("parse_twiml must succeed on canned XML");

    // ── 5. Assertions ────────────────────────────────────────────────────────
    assert!(
        !actions.is_empty(),
        "expected ≥1 EntryAction from canned TwiML, got none"
    );

    // First action must be Play with prompt_text = "Hello from TwiML"
    match &actions[0] {
        EntryAction::Play { prompt_text, .. } => {
            assert_eq!(
                prompt_text.as_deref(),
                Some("Hello from TwiML"),
                "expected Say text to be 'Hello from TwiML'"
            );
        }
        other => panic!("expected EntryAction::Play as first action, got {:?}", other),
    }

    // Second action must be Hangup
    assert_eq!(actions.len(), 2, "expected exactly 2 actions (Say + Hangup)");
    assert!(
        matches!(actions[1], EntryAction::Hangup { .. }),
        "expected second action to be Hangup"
    );

    // wiremock verifies the mock was hit exactly once when the server drops
}
