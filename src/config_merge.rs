//! Merges `config.toml` (base) with `system_config` DB overrides and writes
//! `config.generated.toml` alongside the base file.
//!
//! Called once at startup, before `Config::load()`. The rest of the system
//! is unmodified — it just loads the generated file instead of the base.

use anyhow::{Context, Result};
use sea_orm::DatabaseConnection;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table};
use tracing::{info, warn};

use crate::models::system_config;

/// Minimal bootstrap config: only `database_url` is required in `config.toml`.
#[derive(Deserialize)]
struct Bootstrap {
    database_url: String,
}

/// Run the merge step.
///
/// 1. Reads `database_url` from `base_path`.
/// 2. Connects to DB and runs migrations.
/// 3. Auto-detects external IP (unless overridden in DB) and saves to DB.
/// 4. Loads all DB overrides.
/// 5. Merges base TOML + overrides → writes `<dir>/config.generated.toml`.
/// 6. Returns the path to the generated file.
///
/// If anything goes wrong the error is returned and the caller falls back to
/// using the original `base_path`.
pub async fn apply(base_path: &str) -> Result<String> {
    // --- Read database_url from base config ---
    let base_text = std::fs::read_to_string(base_path)
        .with_context(|| format!("cannot read base config: {base_path}"))?;

    let bootstrap: Bootstrap = toml::from_str(&base_text)
        .context("config.toml must contain a valid database_url")?;

    // --- Connect and migrate ---
    let db = crate::models::create_db(&bootstrap.database_url)
        .await
        .context("cannot connect to database for config merge")?;

    // --- External IP handling ---
    handle_external_ip(&db).await;

    // --- Load overrides ---
    let overrides = system_config::Model::get_all(&db)
        .await
        .context("cannot load system_config from DB")?;

    // --- Parse base TOML ---
    let mut doc: DocumentMut = base_text
        .parse()
        .context("config.toml is not valid TOML")?;

    // --- Apply overrides ---
    let mut applied = 0usize;
    for row in &overrides {
        if apply_override(&mut doc, &row.key, &row.value) {
            applied += 1;
        } else {
            warn!(key = %row.key, "skipped unrecognised or unparseable system_config key");
        }
    }

    // --- Write generated file ---
    let generated_path = generated_path_for(base_path);
    std::fs::write(&generated_path, doc.to_string())
        .with_context(|| format!("cannot write {}", generated_path.display()))?;

    info!(
        base = base_path,
        generated = %generated_path.display(),
        overrides = applied,
        "config merged and written"
    );

    Ok(generated_path.to_string_lossy().into_owned())
}

/// Detect the public IP, compare to what is in DB, update if changed.
async fn handle_external_ip(db: &DatabaseConnection) {
    const KEY: &str = "external_ip";

    // Check if manually overridden
    if let Ok(Some(row)) = system_config::Model::get(db, KEY).await {
        if row.is_override {
            info!(ip = %row.value, "external_ip: using manual override, skipping detection");
            return;
        }
    }

    let detected = crate::ip_detect::detect_public_ip().await;

    let Some(ip) = detected else {
        warn!("external_ip detection produced no result — keeping existing DB value");
        return;
    };

    let encoded = serde_json::to_string(&ip).unwrap_or_else(|_| format!("\"{ip}\""));

    // Check current DB value
    let current = system_config::Model::get(db, KEY)
        .await
        .ok()
        .flatten()
        .map(|r| r.value);

    match &current {
        Some(existing) if *existing == encoded => {
            // Unchanged — silent
        }
        Some(existing) => {
            info!(old = %existing, new = %encoded, "external_ip updated");
            let _ = system_config::Model::upsert(db, KEY, &encoded, false).await;
        }
        None => {
            info!(ip = %ip, "external_ip set for first time");
            let _ = system_config::Model::upsert(db, KEY, &encoded, false).await;
        }
    }
}

/// Apply one DB override row to the TOML document.
/// Returns true if the key was recognised and the value was parseable.
fn apply_override(doc: &mut DocumentMut, key: &str, raw_json: &str) -> bool {
    let json_val: serde_json::Value = match serde_json::from_str(raw_json) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let item = match json_to_toml_item(&json_val) {
        Some(i) => i,
        None => return false,
    };

    let parts: Vec<&str> = key.splitn(2, '.').collect();

    if parts.len() == 1 {
        // Top-level key: external_ip, log_level, log_file, http_addr, etc.
        doc[key] = item;
    } else {
        let section = parts[0];
        let field = parts[1];
        // Ensure the section table exists
        if doc.get(section).is_none() {
            doc[section] = Item::Table(Table::new());
        }
        if let Some(Item::Table(t)) = doc.get_mut(section) {
            t[field] = item;
        } else {
            return false;
        }
    }

    true
}

/// Convert a serde_json::Value into a toml_edit::Item.
fn json_to_toml_item(v: &serde_json::Value) -> Option<Item> {
    match v {
        serde_json::Value::String(s) => Some(toml_edit::value(s.as_str())),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml_edit::value(i))
            } else {
                n.as_f64().map(toml_edit::value)
            }
        }
        serde_json::Value::Bool(b) => Some(toml_edit::value(*b)),
        serde_json::Value::Null => None,
        complex => {
            // Arrays and objects: round-trip through TOML serialisation
            let wrapper = serde_json::json!({ "__v__": complex });
            let toml_str = toml::to_string(&wrapper).ok()?;
            let parsed: DocumentMut = toml_str.parse().ok()?;
            parsed.get("__v__").cloned()
        }
    }
}

/// Returns `<dir>/config.generated.toml` for a given base config path.
fn generated_path_for(base_path: &str) -> PathBuf {
    let base = Path::new(base_path);
    let dir = base.parent().unwrap_or(Path::new("."));
    dir.join("config.generated.toml")
}
