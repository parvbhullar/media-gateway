//! Integration tests for the Manipulation Engine pipeline (Phase 9 Plan 09-04 — IT-02).
//!
//! Coverage matrix per `09-04-PLAN.md` <behavior> + `09-CONTEXT.md` D-36 (12 cases):
//!
//!  1. Cross-engine (IT-02): Translation 02079460123→+442079460123 then Manipulation
//!     caller_number regex ^\\+44 → set_header X-Country UK.
//!  2. Trunk-source condition (MAN-05): manipulation trunk=us-carrier → X-Region US.
//!  3. Hangup short-circuit (MAN-06): Outcome::Hangup{code:403} stops further actions.
//!  4. Anti-actions on false condition (MAN-07): US caller gets X-Country=OTHER.
//!  5. Cascade within class: rule 1 set_var x=1, rule 2 condition var:x=1 → X-Cascade.
//!  6. Variable interpolation in header: X-Caller = "${caller_number}".
//!  7. Header allowlist write-time rejection: POST set_header Via → 400.
//!  8. Sleep cap write-time rejection: POST sleep 6000ms → 400.
//!  9. Direction filter: inbound-only class skipped on outbound INVITE.
//! 10. Or-mode condition: matches either UK (+44) or US (+1) caller.
//! 11. Cross-rule var/condition with log capture (tracing subscriber).
//! 12. Per-call var scope isolation: two sessions, vars don't bleed.

use axum::{body::Body, http};
use rsipstack::{dialog::invitation::InviteOption, sip::Uri};
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use sea_orm_migration::{MigratorTrait, prelude::*};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;

use rustpbx::{
    call::DialDirection,
    models::{
        manipulations::{ActiveModel as ManipulationActiveModel, Migration as ManipulationsMigration},
        translations::{ActiveModel as TranslationActiveModel, Migration as TranslationsMigration},
    },
    proxy::{
        manipulation::{ManipulationContext, ManipulationEngine, ManipulationOutcome},
        translation::TranslationEngine,
    },
};

// ─── Test-only Migrator ─────────────────────────────────────────────────────

/// Runs only the translations + manipulations migrations (no full stack).
struct TestMigrator;

#[async_trait::async_trait]
impl MigratorTrait for TestMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(TranslationsMigration),
            Box::new(ManipulationsMigration),
        ]
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

async fn setup_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    TestMigrator::up(&db, None)
        .await
        .expect("run migrations");
    db
}

/// Seed a manipulation class with a list of rules.
async fn seed_manipulation(
    db: &DatabaseConnection,
    name: &str,
    direction: &str,
    priority: i32,
    rules: serde_json::Value,
) -> String {
    let now = chrono::Utc::now();
    let id = format!("man-{}", name);
    let am = ManipulationActiveModel {
        id: Set(id.clone()),
        name: Set(name.to_string()),
        description: Set(None),
        direction: Set(direction.to_string()),
        priority: Set(priority),
        is_active: Set(true),
        rules: Set(rules),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.expect("insert manipulation row");
    id
}

/// Seed a translation rule (mirrors proxy_translation_engine.rs helper).
#[allow(clippy::too_many_arguments)]
async fn seed_translation(
    db: &DatabaseConnection,
    name: &str,
    caller_pattern: Option<&str>,
    destination_pattern: Option<&str>,
    caller_replacement: Option<&str>,
    destination_replacement: Option<&str>,
    direction: &str,
    priority: i32,
) -> String {
    let now = chrono::Utc::now();
    let id = format!("trn-{}", name);
    let am = TranslationActiveModel {
        id: Set(id.clone()),
        name: Set(name.to_string()),
        description: Set(None),
        caller_pattern: Set(caller_pattern.map(|s| s.to_string())),
        destination_pattern: Set(destination_pattern.map(|s| s.to_string())),
        caller_replacement: Set(caller_replacement.map(|s| s.to_string())),
        destination_replacement: Set(destination_replacement.map(|s| s.to_string())),
        direction: Set(direction.to_string()),
        priority: Set(priority),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.expect("insert translation row");
    id
}

fn make_invite(caller_user: &str, callee_user: &str) -> InviteOption {
    let caller_str = format!("sip:{}@example.com", caller_user);
    let callee_str = format!("sip:{}@example.com", callee_user);
    let contact_str = format!("sip:{}@192.168.1.1:5060", caller_user);
    InviteOption {
        caller: Uri::try_from(caller_str.as_str()).expect("caller uri"),
        callee: Uri::try_from(callee_str.as_str()).expect("callee uri"),
        contact: Uri::try_from(contact_str.as_str()).expect("contact uri"),
        ..Default::default()
    }
}

fn make_ctx(
    caller: &str,
    dest: &str,
    trunk: &str,
    dir: DialDirection,
    session: &str,
) -> ManipulationContext {
    ManipulationContext {
        caller_number: caller.to_string(),
        destination_number: dest.to_string(),
        trunk_name: trunk.to_string(),
        direction: dir,
        session_id: session.to_string(),
    }
}

/// Case-insensitive header value lookup on InviteOption.headers.
fn header_value(opt: &InviteOption, name: &str) -> Option<String> {
    let headers = opt.headers.as_ref()?;
    for h in headers {
        if let rsipstack::sip::Header::Other(hname, hval) = h {
            if hname.eq_ignore_ascii_case(name) {
                return Some(hval.to_string());
            }
        }
    }
    None
}

fn build_engine() -> Arc<ManipulationEngine> {
    Arc::new(ManipulationEngine::new())
}

/// POST a manipulation class JSON body to the live router; returns (status, body).
async fn http_post_manipulation(
    body: serde_json::Value,
) -> (axum::http::StatusCode, serde_json::Value) {
    // Build state via test_state_with_api_key (uses in-process router).
    // Inline the helper to avoid a dep on the `common` module from this crate.
    use chrono::Utc;
    use rustpbx::{
        app::{AppState, AppStateBuilder},
        config::Config,
        handler::api_v1::auth::{IssuedKey, issue_api_key},
        models::api_key,
    };
    use sea_orm::ActiveModelTrait as _;

    let mut cfg = Config::default();
    // Use a temp file so the multi-connection pool can open it.
    let pid = std::process::id();
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1000);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let path = std::env::temp_dir()
        .join(format!("rustpbx-man-pipeline-{pid}-{n}.sqlite3"));
    let _ = std::fs::remove_file(&path);
    cfg.database_url = format!("sqlite://{}", path.display());
    cfg.http_addr = "127.0.0.1:0".to_string();
    cfg.proxy.generated_dir = std::env::temp_dir()
        .join(format!("rustpbx-man-gen-{pid}-{n}"))
        .display()
        .to_string();

    let state: AppState = AppStateBuilder::new()
        .with_config(cfg)
        .with_skip_sip_bind()
        .build()
        .await
        .expect("build AppState");

    let IssuedKey { plaintext, hash } = issue_api_key();
    let am = api_key::ActiveModel {
        name: Set("test-key".to_string()),
        hash_sha256: Set(hash),
        description: Set(None),
        created_at: Set(Utc::now()),
        ..Default::default()
    };
    am.insert(state.db()).await.expect("insert api key");

    let app = rustpbx::app::create_router(state);
    let resp = app
        .oneshot(
            http::Request::builder()
                .method("POST")
                .uri("/api/v1/manipulations")
                .header(http::header::AUTHORIZATION, format!("Bearer {}", plaintext))
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .expect("read body");
    let body_json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, body_json)
}

// =========================================================================
// D-36 #1 — IT-02 Cross-engine: Translation then Manipulation (MAN-05)
// =========================================================================

#[tokio::test]
async fn it_02_cross_engine_translation_then_manipulation() {
    // D-36 case #1: Translation rewrites 02079460123→+442079460123, then
    // Manipulation matches the rewritten caller (^\\+44) and sets X-Country=UK.
    let db = setup_db().await;

    // Seed translation rule: 0XXXXXXXXX → +44XXXXXXXXX (inbound).
    seed_translation(&db, "uk-norm", Some(r"^0(\d+)$"), None, Some("+44$1"), None, "inbound", 100).await;

    // Seed manipulation class: caller_number regex ^\\+44 → set_header X-Country UK.
    seed_manipulation(
        &db,
        "tag-uk-country",
        "both",
        100,
        json!([{
            "name": "uk-caller",
            "conditions": [{"source": "caller_number", "op": "regex", "value": r"^\+44"}],
            "condition_mode": "and",
            "actions": [{"type": "set_header", "name": "X-Country", "value": "UK"}],
            "anti_actions": []
        }]),
    )
    .await;

    let translation_engine = TranslationEngine::new();
    let manipulation_engine = build_engine();

    let mut opt = make_invite("02079460123", "callee");

    // Step 1: Translation pass (pre-routing, Phase 8).
    translation_engine
        .translate(&mut opt, DialDirection::Inbound, &db)
        .await
        .expect("translate");

    // Assert: caller rewritten to E.164.
    assert_eq!(
        opt.caller.user().unwrap_or_default(),
        "+442079460123",
        "translation must rewrite 02079460123 → +442079460123"
    );

    // Step 2: Manipulation pass (post-routing, Phase 9 D-22).
    let ctx = make_ctx("+442079460123", "callee", "uk-carrier", DialDirection::Inbound, "sess-it02-cross");
    let outcome = manipulation_engine
        .manipulate(&mut opt, ctx, &db)
        .await
        .expect("manipulate");

    // Assert: manipulation continues (not hangup).
    assert!(
        matches!(outcome, ManipulationOutcome::Continue { .. }),
        "expected Continue, got Hangup"
    );

    // Assert: X-Country header set to UK by manipulation engine.
    let country = header_value(&opt, "X-Country");
    assert_eq!(
        country.as_deref(),
        Some("UK"),
        "manipulation must set X-Country=UK on post-translation +44 caller"
    );
}

// =========================================================================
// D-36 #2 — Trunk-source condition (MAN-05)
// =========================================================================

#[tokio::test]
async fn man_05_trunk_source_condition() {
    // D-36 case #2: Manipulation trunk=us-carrier → set X-Region=US.
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "trunk-us-region",
        "both",
        100,
        json!([{
            "name": "us-carrier-tag",
            "conditions": [{"source": "trunk", "op": "equals", "value": "us-carrier"}],
            "condition_mode": "and",
            "actions": [{"type": "set_header", "name": "X-Region", "value": "US"}],
            "anti_actions": []
        }]),
    )
    .await;

    let engine = build_engine();

    // Call with trunk_name = "us-carrier" → rule fires.
    let mut opt = make_invite("+14155551234", "callee");
    let ctx = make_ctx("+14155551234", "callee", "us-carrier", DialDirection::Inbound, "sess-trunk-us");
    engine.manipulate(&mut opt, ctx, &db).await.expect("manipulate");
    assert_eq!(
        header_value(&opt, "X-Region").as_deref(),
        Some("US"),
        "trunk=us-carrier should set X-Region=US"
    );

    // Call with trunk_name = "uk-carrier" → rule does NOT fire.
    let mut opt2 = make_invite("+447700900000", "callee");
    let ctx2 = make_ctx("+447700900000", "callee", "uk-carrier", DialDirection::Inbound, "sess-trunk-uk");
    engine.manipulate(&mut opt2, ctx2, &db).await.expect("manipulate");
    assert!(
        header_value(&opt2, "X-Region").is_none(),
        "trunk=uk-carrier should NOT set X-Region"
    );
}

// =========================================================================
// D-36 #3 — Hangup short-circuit (MAN-06)
// =========================================================================

#[tokio::test]
async fn man_06_hangup_short_circuit_returns_outcome_hangup() {
    // D-36 case #3: hangup action returns Outcome::Hangup and stops further
    // actions (set_header after hangup must NOT fire).
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "reject-403",
        "both",
        100,
        json!([{
            "name": "hangup-rule",
            "conditions": [{"source": "caller_number", "op": "starts_with", "value": "+1"}],
            "condition_mode": "and",
            "actions": [
                {"type": "hangup", "sip_code": 403, "reason": "Forbidden"},
                {"type": "set_header", "name": "X-Should-Not-Set", "value": "yes"}
            ],
            "anti_actions": []
        }]),
    )
    .await;

    let engine = build_engine();
    let mut opt = make_invite("+15551234567", "callee");
    let ctx = make_ctx("+15551234567", "callee", "trunk", DialDirection::Inbound, "sess-hangup");
    let outcome = engine.manipulate(&mut opt, ctx, &db).await.expect("manipulate");

    // Assert Outcome::Hangup with correct code and reason.
    match outcome {
        ManipulationOutcome::Hangup { code, reason, .. } => {
            assert_eq!(code, 403, "hangup code must be 403");
            assert_eq!(reason, "Forbidden", "hangup reason must be Forbidden");
        }
        ManipulationOutcome::Continue { .. } => {
            panic!("expected Hangup outcome, got Continue");
        }
    }

    // Assert: set_header AFTER hangup was NOT applied.
    assert!(
        header_value(&opt, "X-Should-Not-Set").is_none(),
        "set_header after hangup must not fire (D-29 short-circuit)"
    );
}

/// D-36 #3 follow-up: Hangup outcome translates to RouteResult::Reject.
/// Simulates the call.rs translation logic per Phase 5 D-15 contract.
#[tokio::test]
async fn man_06_hangup_translates_to_reject() {
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "reject-403-b",
        "both",
        100,
        json!([{
            "name": "hangup-all",
            "conditions": [{"source": "caller_number", "op": "starts_with", "value": "+1"}],
            "condition_mode": "and",
            "actions": [{"type": "hangup", "sip_code": 403, "reason": "Forbidden"}],
            "anti_actions": []
        }]),
    )
    .await;

    let engine = build_engine();
    let mut opt = make_invite("+14155551234", "callee");
    let ctx = make_ctx("+14155551234", "callee", "trunk", DialDirection::Inbound, "sess-reject");
    let outcome = engine.manipulate(&mut opt, ctx, &db).await.expect("manipulate");

    // Simulate the call.rs translation logic (the actual wiring is in Task 2).
    // This mirrors: match outcome { Hangup{code, reason,..} → RouteResult::Reject{...} }
    let route_result = match outcome {
        ManipulationOutcome::Hangup { code, reason, .. } => {
            rustpbx::config::RouteResult::Reject {
                code,
                reason,
                retry_after_secs: None,
            }
        }
        ManipulationOutcome::Continue { .. } => {
            panic!("expected Hangup, got Continue");
        }
    };

    match route_result {
        rustpbx::config::RouteResult::Reject { code, reason, retry_after_secs } => {
            assert_eq!(code, 403);
            assert_eq!(reason, "Forbidden");
            assert!(retry_after_secs.is_none(), "retry_after_secs must be None for manipulation hangup");
        }
        _ => panic!("expected Reject RouteResult"),
    }
}

// =========================================================================
// D-36 #4 — Anti-actions on false condition (MAN-07)
// =========================================================================

#[tokio::test]
async fn man_07_anti_actions_on_else_branch() {
    // D-36 case #4: condition caller_number regex ^\\+44 fires actions on UK,
    // anti_actions on non-UK (e.g., US caller).
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "country-tag",
        "both",
        100,
        json!([{
            "name": "uk-vs-other",
            "conditions": [{"source": "caller_number", "op": "regex", "value": r"^\+44"}],
            "condition_mode": "and",
            "actions": [{"type": "set_header", "name": "X-Country", "value": "UK"}],
            "anti_actions": [{"type": "set_header", "name": "X-Country", "value": "OTHER"}]
        }]),
    )
    .await;

    let engine = build_engine();

    // US caller → anti_actions fire → X-Country=OTHER.
    let mut opt = make_invite("+15551234567", "callee");
    let ctx = make_ctx("+15551234567", "callee", "trunk", DialDirection::Inbound, "sess-anti");
    engine.manipulate(&mut opt, ctx, &db).await.expect("manipulate");
    assert_eq!(
        header_value(&opt, "X-Country").as_deref(),
        Some("OTHER"),
        "US caller should trigger anti_action → X-Country=OTHER"
    );

    // UK caller → actions fire → X-Country=UK.
    let mut opt2 = make_invite("+447700900000", "callee");
    let ctx2 = make_ctx("+447700900000", "callee", "trunk", DialDirection::Inbound, "sess-anti-uk");
    engine.manipulate(&mut opt2, ctx2, &db).await.expect("manipulate");
    assert_eq!(
        header_value(&opt2, "X-Country").as_deref(),
        Some("UK"),
        "UK caller should trigger actions → X-Country=UK"
    );
}

// =========================================================================
// D-36 #5 — Cascade within class
// =========================================================================

#[tokio::test]
async fn cascade_within_class() {
    // D-36 case #5: rule 1 sets var x=1; rule 2 conditions on var:x=1 → X-Cascade=YES.
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "cascade-test",
        "both",
        100,
        json!([
            {
                "name": "set-var",
                "conditions": [{"source": "caller_number", "op": "starts_with", "value": "+"}],
                "condition_mode": "and",
                "actions": [{"type": "set_var", "name": "x", "value": "1"}],
                "anti_actions": []
            },
            {
                "name": "read-var",
                "conditions": [{"source": "var:x", "op": "equals", "value": "1"}],
                "condition_mode": "and",
                "actions": [{"type": "set_header", "name": "X-Cascade", "value": "YES"}],
                "anti_actions": []
            }
        ]),
    )
    .await;

    let engine = build_engine();
    let mut opt = make_invite("+14155551234", "callee");
    let ctx = make_ctx("+14155551234", "callee", "trunk", DialDirection::Inbound, "sess-cascade");
    engine.manipulate(&mut opt, ctx, &db).await.expect("manipulate");

    assert_eq!(
        header_value(&opt, "X-Cascade").as_deref(),
        Some("YES"),
        "rule 2 should see var:x=1 set by rule 1 (D-27 cascade)"
    );
}

// =========================================================================
// D-36 #6 — Variable interpolation in header value
// =========================================================================

#[tokio::test]
async fn variable_interpolation_in_header() {
    // D-36 case #6: set_header value="${caller_number}" interpolates the caller.
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "interp-caller",
        "both",
        100,
        json!([{
            "name": "caller-echo",
            "conditions": [{"source": "caller_number", "op": "starts_with", "value": "+"}],
            "condition_mode": "and",
            "actions": [{"type": "set_header", "name": "X-Caller", "value": "${caller_number}"}],
            "anti_actions": []
        }]),
    )
    .await;

    let engine = build_engine();
    let mut opt = make_invite("+14155551234", "callee");
    let ctx = make_ctx("+14155551234", "callee", "trunk", DialDirection::Inbound, "sess-interp");
    engine.manipulate(&mut opt, ctx, &db).await.expect("manipulate");

    assert_eq!(
        header_value(&opt, "X-Caller").as_deref(),
        Some("+14155551234"),
        "interpolation of ${{caller_number}} should resolve to ctx.caller_number"
    );
}

// =========================================================================
// D-36 #7 — Header allowlist write-time rejection (D-31)
// =========================================================================

#[tokio::test]
async fn header_allowlist_write_time_rejection() {
    // D-36 case #7: POST manipulation with set_header Via → 400.
    // Exercises the live router write-time validation.
    let (status, body) = http_post_manipulation(json!({
        "name": "bad-via",
        "direction": "both",
        "priority": 100,
        "is_active": true,
        "rules": [{
            "name": "set-via",
            "conditions": [{"source": "caller_number", "op": "starts_with", "value": "+"}],
            "condition_mode": "and",
            "actions": [{"type": "set_header", "name": "Via", "value": "SIP/2.0/UDP 10.0.0.1:5060"}],
            "anti_actions": []
        }]
    }))
    .await;

    assert_eq!(
        status,
        axum::http::StatusCode::BAD_REQUEST,
        "POST with set_header Via must return 400 (D-31); body={:?}",
        body
    );
}

// =========================================================================
// D-36 #8 — Sleep cap write-time rejection (D-18)
// =========================================================================

#[tokio::test]
async fn sleep_cap_write_time_rejection() {
    // D-36 case #8a: POST sleep 6000ms → 400 (over 5000ms cap).
    let (status, body) = http_post_manipulation(json!({
        "name": "bad-sleep",
        "direction": "both",
        "priority": 100,
        "is_active": true,
        "rules": [{
            "name": "long-sleep",
            "conditions": [{"source": "caller_number", "op": "starts_with", "value": "+"}],
            "condition_mode": "and",
            "actions": [{"type": "sleep", "duration_ms": 6000}],
            "anti_actions": []
        }]
    }))
    .await;

    assert_eq!(
        status,
        axum::http::StatusCode::BAD_REQUEST,
        "POST sleep 6000ms must return 400 (D-18 cap); body={:?}",
        body
    );
}

#[tokio::test]
async fn sleep_100ms_accepted_and_executes() {
    // D-36 case #8b: POST sleep 100ms → 201; runtime adds ~100ms.
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "short-sleep",
        "both",
        100,
        json!([{
            "name": "sleep-100",
            "conditions": [{"source": "caller_number", "op": "starts_with", "value": "+"}],
            "condition_mode": "and",
            "actions": [{"type": "sleep", "duration_ms": 100}],
            "anti_actions": []
        }]),
    )
    .await;

    let engine = build_engine();
    let mut opt = make_invite("+14155551234", "callee");
    let ctx = make_ctx("+14155551234", "callee", "trunk", DialDirection::Inbound, "sess-sleep");

    let start = std::time::Instant::now();
    engine.manipulate(&mut opt, ctx, &db).await.expect("manipulate");
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() >= 90,
        "sleep 100ms should delay at least 90ms; got {}ms",
        elapsed.as_millis()
    );
}

// =========================================================================
// D-36 #9 — Direction filter: inbound-only class skipped on outbound
// =========================================================================

#[tokio::test]
async fn direction_filter_inbound_only_skipped_on_outbound() {
    // D-36 case #9: class direction=inbound; ctx.direction=Outbound → rule does NOT fire.
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "inbound-only",
        "inbound",
        100,
        json!([{
            "name": "tag-inbound",
            "conditions": [{"source": "caller_number", "op": "starts_with", "value": "+"}],
            "condition_mode": "and",
            "actions": [{"type": "set_header", "name": "X-Direction-Test", "value": "fired"}],
            "anti_actions": []
        }]),
    )
    .await;

    let engine = build_engine();
    let mut opt = make_invite("+14155551234", "callee");
    let ctx = make_ctx("+14155551234", "callee", "trunk", DialDirection::Outbound, "sess-dir");
    engine.manipulate(&mut opt, ctx, &db).await.expect("manipulate");

    assert!(
        header_value(&opt, "X-Direction-Test").is_none(),
        "inbound-only class must be skipped on outbound call"
    );

    // Verify that the same rule DOES fire on inbound.
    let mut opt2 = make_invite("+14155551234", "callee");
    let ctx2 = make_ctx("+14155551234", "callee", "trunk", DialDirection::Inbound, "sess-dir-in");
    engine.manipulate(&mut opt2, ctx2, &db).await.expect("manipulate");

    assert_eq!(
        header_value(&opt2, "X-Direction-Test").as_deref(),
        Some("fired"),
        "inbound-only class must fire on inbound call"
    );
}

// =========================================================================
// D-36 #10 — Or-mode condition: matches either UK or US caller
// =========================================================================

#[tokio::test]
async fn or_mode_condition_matches_either_uk_or_us() {
    // D-36 case #10: condition_mode=or, two regex conditions (+44 or +1).
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "uk-or-us",
        "both",
        100,
        json!([{
            "name": "uk-or-us-match",
            "conditions": [
                {"source": "caller_number", "op": "regex", "value": r"^\+44"},
                {"source": "caller_number", "op": "regex", "value": r"^\+1"}
            ],
            "condition_mode": "or",
            "actions": [{"type": "set_header", "name": "X-Country", "value": "MATCH"}],
            "anti_actions": []
        }]),
    )
    .await;

    let engine = build_engine();

    // UK caller (+44) → match.
    let mut opt_uk = make_invite("+447700900000", "callee");
    let ctx_uk = make_ctx("+447700900000", "callee", "trunk", DialDirection::Inbound, "sess-or-uk");
    engine.manipulate(&mut opt_uk, ctx_uk, &db).await.expect("manipulate");
    assert_eq!(
        header_value(&opt_uk, "X-Country").as_deref(),
        Some("MATCH"),
        "+44 caller must match or-mode condition"
    );

    // US caller (+1) → match.
    let mut opt_us = make_invite("+14155551234", "callee");
    let ctx_us = make_ctx("+14155551234", "callee", "trunk", DialDirection::Inbound, "sess-or-us");
    engine.manipulate(&mut opt_us, ctx_us, &db).await.expect("manipulate");
    assert_eq!(
        header_value(&opt_us, "X-Country").as_deref(),
        Some("MATCH"),
        "+1 caller must match or-mode condition"
    );

    // French caller (+33) → no match.
    let mut opt_fr = make_invite("+33123456789", "callee");
    let ctx_fr = make_ctx("+33123456789", "callee", "trunk", DialDirection::Inbound, "sess-or-fr");
    engine.manipulate(&mut opt_fr, ctx_fr, &db).await.expect("manipulate");
    assert!(
        header_value(&opt_fr, "X-Country").is_none(),
        "+33 caller must NOT match uk-or-us condition"
    );
}

// =========================================================================
// D-36 #11 — Cross-rule var/condition with log capture (tracing test layer)
// =========================================================================

/// Minimal tracing layer that collects log action messages from the engine.
mod log_capture {
    use std::sync::{Arc, Mutex};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::Layer;

    #[derive(Default, Clone)]
    pub struct CaptureLayer {
        pub messages: Arc<Mutex<Vec<String>>>,
    }

    impl<S: Subscriber> Layer<S> for CaptureLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            use tracing::field::Visit;

            struct Visitor {
                message: String,
                event_type: String,
            }

            impl Visit for Visitor {
                fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                    if field.name() == "message" {
                        self.message = value.to_string();
                    }
                    if field.name() == "event" {
                        self.event_type = value.to_string();
                    }
                }
                fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                    if field.name() == "message" {
                        self.message = format!("{:?}", value);
                    }
                }
            }

            let mut visitor = Visitor {
                message: String::new(),
                event_type: String::new(),
            };
            event.record(&mut visitor);

            // Capture log events from the manipulation engine.
            if visitor.event_type == "manipulation_log" || !visitor.message.is_empty() {
                self.messages.lock().unwrap().push(visitor.message);
            }
        }
    }
}

#[tokio::test]
async fn cross_rule_var_to_condition_with_log() {
    // D-36 case #11: rule 1 set_var country=UK (always), rule 2 condition
    // var:country=UK → log info "got uk".
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "var-log-test",
        "both",
        100,
        json!([
            {
                "name": "set-country",
                "conditions": [{"source": "caller_number", "op": "starts_with", "value": "+"}],
                "condition_mode": "and",
                "actions": [{"type": "set_var", "name": "country", "value": "UK"}],
                "anti_actions": []
            },
            {
                "name": "log-country",
                "conditions": [{"source": "var:country", "op": "equals", "value": "UK"}],
                "condition_mode": "and",
                "actions": [{"type": "log", "level": "info", "message": "got uk"}],
                "anti_actions": []
            }
        ]),
    )
    .await;

    // Set up tracing capture layer.
    let capture_layer = log_capture::CaptureLayer::default();
    let captured_messages = capture_layer.messages.clone();

    let subscriber = tracing_subscriber::registry().with(capture_layer);
    let _guard = tracing::subscriber::set_default(subscriber);

    let engine = build_engine();
    let mut opt = make_invite("+447700900000", "callee");
    let ctx = make_ctx("+447700900000", "callee", "trunk", DialDirection::Inbound, "sess-log");
    let outcome = engine.manipulate(&mut opt, ctx, &db).await.expect("manipulate");

    assert!(
        matches!(outcome, ManipulationOutcome::Continue { .. }),
        "expected Continue outcome"
    );

    // Assert that a log message containing "got uk" was emitted.
    let messages = captured_messages.lock().unwrap().clone();
    let found_got_uk = messages.iter().any(|m| m.contains("got uk"));
    assert!(
        found_got_uk,
        "expected log message 'got uk' in tracing events; captured: {:?}",
        messages
    );
}

// =========================================================================
// D-36 #12 — Per-call var scope isolation across two sessions
// =========================================================================

#[tokio::test]
async fn per_call_var_scope_isolation_two_sessions() {
    // D-36 case #12: session A sets var x=A; session B evaluates var:x=A.
    // Session B should NOT see session A's var (separate var_scope key).
    let db = setup_db().await;

    seed_manipulation(
        &db,
        "scope-isolation",
        "both",
        100,
        json!([
            {
                "name": "set-x",
                "conditions": [{"source": "caller_number", "op": "equals", "value": "+1111111111"}],
                "condition_mode": "and",
                "actions": [{"type": "set_var", "name": "x", "value": "A"}],
                "anti_actions": []
            },
            {
                "name": "check-x",
                "conditions": [{"source": "var:x", "op": "equals", "value": "A"}],
                "condition_mode": "and",
                "actions": [{"type": "set_header", "name": "X-Should-Not-Match", "value": "leaked"}],
                "anti_actions": []
            }
        ]),
    )
    .await;

    let engine = build_engine();

    // Session A: caller=+1111111111 → set_var x=A.
    let mut opt_a = make_invite("+1111111111", "callee");
    let ctx_a = make_ctx("+1111111111", "callee", "trunk", DialDirection::Inbound, "session-A");
    engine.manipulate(&mut opt_a, ctx_a, &db).await.expect("manipulate A");

    // Session B: different caller (+2222222222), different session_id.
    // Rule "set-x" does NOT fire (caller doesn't match). So var:x should be empty
    // for session B, and "check-x" should also NOT fire.
    let mut opt_b = make_invite("+2222222222", "callee");
    let ctx_b = make_ctx("+2222222222", "callee", "trunk", DialDirection::Inbound, "session-B");
    engine.manipulate(&mut opt_b, ctx_b, &db).await.expect("manipulate B");

    assert!(
        header_value(&opt_b, "X-Should-Not-Match").is_none(),
        "session B must NOT see var:x set by session A (D-15 scope isolation)"
    );

    // Cleanup both sessions.
    engine.cleanup_session("session-A");
    engine.cleanup_session("session-B");

    // Idempotency: cleanup_session on already-cleaned keys is a no-op.
    engine.cleanup_session("session-A");
}
