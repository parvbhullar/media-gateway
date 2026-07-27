//! Shared test fixtures for `/api/v1/*` integration tests.
//!
//! Each fixture produces a full `AppState` backed by an isolated on-disk
//! SQLite file (pure in-memory doesn't survive the multi-connection pool
//! that `AppStateBuilder::build` creates). The temp file is cleaned up
//! automatically via `TempGuard` held by a once-cell so the same process
//! reuses one DB path for a given fixture call, and the OS cleans up when
//! the test process exits.
//!
//! `test_state_with_api_key(name)` inserts one freshly issued key and
//! returns the plaintext so the caller can send it as a Bearer token.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use rustpbx::{
    app::{AppState, AppStateBuilder},
    config::{Config, RecordingPolicy},
    handler::api_v1::auth::{IssuedKey, issue_api_key},
    models::api_key,
};
use sea_orm::{ActiveModelTrait, Set};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn fresh_db_url() -> String {
    let pid = std::process::id();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("rustpbx-api-v1-test-{pid}-{n}.sqlite3"));
    // Best-effort cleanup if a stale file from a previous aborted run exists.
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

fn fresh_generated_dir() -> String {
    let pid = std::process::id();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("rustpbx-api-v1-gen-{pid}-{n}"));
    let _ = std::fs::create_dir_all(&path);
    path.display().to_string()
}

fn test_config() -> Config {
    let mut cfg = Config::default();
    cfg.database_url = fresh_db_url();
    // Silence the HTTP bind — tests call the router in-process via oneshot.
    cfg.http_addr = "127.0.0.1:0".to_string();
    // Per-test generated config dir so concurrent ProxyDataContext init
    // doesn't race on `./config/trunks/trunks.generated.toml.<ts>.bak`
    // (the timestamp is second-precision, so parallel tests in the same
    // second all try to rename the same file).
    cfg.proxy.generated_dir = fresh_generated_dir();
    cfg
}

/// Build an `AppState` against a fresh isolated SQLite DB with no API keys.
pub async fn test_state_empty() -> AppState {
    AppStateBuilder::new()
        .with_config(test_config())
        .with_skip_sip_bind()
        .build()
        .await
        .expect("failed to build test AppState")
}

/// Build an `AppState` and insert one freshly-issued API key named `name`.
/// Returns the plaintext token so the caller can send it as a Bearer.
pub async fn test_state_with_api_key(name: &str) -> (AppState, String) {
    let state = test_state_empty().await;
    let IssuedKey { plaintext, hash } = issue_api_key();
    let am = api_key::ActiveModel {
        name: Set(name.to_string()),
        hash_sha256: Set(hash),
        description: Set(None),
        created_at: Set(Utc::now()),
        ..Default::default()
    };
    am.insert(state.db())
        .await
        .expect("failed to insert test api_key");
    (state, plaintext)
}

/// Build an `AppState` with a custom absolute recorder root, plus one API key.
///
/// Returns `(state, token)`. The caller passes `recorder_root` as the absolute
/// path the recording subsystem will write into, and is responsible for
/// creating the directory with `std::fs::create_dir_all` before the test
/// needs to write into it.
pub async fn test_state_with_recorder(
    name: &str,
    recorder_root: &str,
) -> (AppState, String) {
    let mut cfg = test_config();
    cfg.recording = Some(RecordingPolicy {
        enabled: false,
        auto_start: Some(false),
        path: Some(recorder_root.to_string()),
        ..Default::default()
    });
    let state = AppStateBuilder::new()
        .with_config(cfg)
        .with_skip_sip_bind()
        .build()
        .await
        .expect("failed to build test AppState with recorder");

    let IssuedKey { plaintext, hash } = issue_api_key();
    let am = api_key::ActiveModel {
        name: Set(name.to_string()),
        hash_sha256: Set(hash),
        description: Set(None),
        created_at: Set(Utc::now()),
        ..Default::default()
    };
    am.insert(state.db())
        .await
        .expect("failed to insert test api_key");
    (state, plaintext)
}
