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

/// DB key: default country code used when normalising DIDs that arrive without
/// a `+` prefix. Stored as a JSON-encoded string, e.g. `"\"US\""`.
pub const ROUTING_DEFAULT_COUNTRY_KEY: &str = "routing.default_country";

/// DB key: when true, incoming calls whose called-number cannot be matched to
/// a provisioned DID are rejected instead of falling through to the legacy
/// regex-based route table. Stored as a JSON bool (`true`/`false`).
pub const ROUTING_DID_STRICT_MODE_KEY: &str = "routing.did_strict_mode";

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

    // --- Parse base TOML (needed for seeding) ---
    let mut doc: DocumentMut = base_text
        .parse()
        .context("config.toml is not valid TOML")?;

    // --- Seed missing keys from config.toml into DB ---
    // Every startup, insert config.toml keys that don't already exist in
    // system_config. Existing DB values are never overwritten — this is the
    // authoritative source for user-customised settings. On first run the
    // entire base config is copied in; on later runs only newly-added keys.
    seed_missing_from_doc(&db, &doc).await;

    // --- Seed routing defaults (keys not present in config.toml) ---
    seed_routing_defaults(&db).await;

    // --- External IP handling (may override the seeded value) ---
    handle_external_ip(&db).await;

    // --- Load overrides ---
    let overrides = system_config::Model::get_all(&db)
        .await
        .context("cannot load system_config from DB")?;

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

/// Insert every key from `config.toml` into `system_config` that is not
/// already present. Never overwrites an existing DB row — user-customised
/// values are preserved. `database_url` is skipped (bootstrap-only value).
async fn seed_missing_from_doc(db: &DatabaseConnection, doc: &DocumentMut) {
    // Collect existing keys first so we know what to skip.
    let existing: std::collections::HashSet<String> = match system_config::Model::get_all(db).await
    {
        Ok(rows) => rows.into_iter().map(|r| r.key).collect(),
        Err(e) => {
            warn!(error = %e, "seed: failed to read existing system_config, skipping seed");
            return;
        }
    };

    let seeds = flatten_doc_for_seed(doc);
    let mut inserted = 0usize;
    for (key, json_val) in seeds {
        if existing.contains(&key) {
            continue;
        }
        let encoded = json_val.to_string();
        match system_config::Model::upsert(db, &key, &encoded, false).await {
            Ok(()) => inserted += 1,
            Err(e) => warn!(key = %key, error = %e, "seed: failed to insert"),
        }
    }

    if inserted > 0 {
        info!(count = inserted, "seeded system_config from config.toml");
    }
}

/// Walk the base TOML document and produce a list of `(dot.key, json_value)`
/// pairs that match the convention used by the console settings handlers:
///   - top-level scalars/arrays/inline tables → `key`
///   - nested table entries → `section.field`
///     (a nested table *inside* a section is stored as a JSON object)
fn flatten_doc_for_seed(doc: &DocumentMut) -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::new();
    for (key, item) in doc.as_table().iter() {
        if key == "database_url" {
            continue; // bootstrap only, never stored in DB
        }
        match item {
            toml_edit::Item::Value(v) => {
                if let Some(jv) = toml_value_to_json(v) {
                    out.push((key.to_string(), jv));
                }
            }
            toml_edit::Item::Table(t) => {
                for (sub_key, sub_item) in t.iter() {
                    let full_key = format!("{}.{}", key, sub_key);
                    if let Some(jv) = toml_item_to_json(sub_item) {
                        out.push((full_key, jv));
                    }
                }
            }
            toml_edit::Item::ArrayOfTables(arr) => {
                let items: Vec<serde_json::Value> = arr
                    .iter()
                    .map(|t| {
                        let map: serde_json::Map<String, serde_json::Value> = t
                            .iter()
                            .filter_map(|(k, v)| toml_item_to_json(v).map(|j| (k.to_string(), j)))
                            .collect();
                        serde_json::Value::Object(map)
                    })
                    .collect();
                out.push((key.to_string(), serde_json::Value::Array(items)));
            }
            toml_edit::Item::None => {}
        }
    }
    out
}

fn toml_value_to_json(v: &toml_edit::Value) -> Option<serde_json::Value> {
    match v {
        toml_edit::Value::String(s) => Some(serde_json::Value::String(s.value().clone())),
        toml_edit::Value::Integer(i) => Some(serde_json::json!(*i.value())),
        toml_edit::Value::Float(f) => Some(serde_json::json!(*f.value())),
        toml_edit::Value::Boolean(b) => Some(serde_json::json!(*b.value())),
        toml_edit::Value::Datetime(d) => Some(serde_json::Value::String(d.value().to_string())),
        toml_edit::Value::Array(arr) => {
            let items: Vec<serde_json::Value> = arr.iter().filter_map(toml_value_to_json).collect();
            Some(serde_json::Value::Array(items))
        }
        toml_edit::Value::InlineTable(t) => {
            let map: serde_json::Map<String, serde_json::Value> = t
                .iter()
                .filter_map(|(k, v)| toml_value_to_json(v).map(|jv| (k.to_string(), jv)))
                .collect();
            Some(serde_json::Value::Object(map))
        }
    }
}

fn toml_item_to_json(item: &toml_edit::Item) -> Option<serde_json::Value> {
    match item {
        toml_edit::Item::Value(v) => toml_value_to_json(v),
        toml_edit::Item::Table(t) => {
            let map: serde_json::Map<String, serde_json::Value> = t
                .iter()
                .filter_map(|(k, v)| toml_item_to_json(v).map(|jv| (k.to_string(), jv)))
                .collect();
            Some(serde_json::Value::Object(map))
        }
        toml_edit::Item::ArrayOfTables(arr) => {
            let items: Vec<serde_json::Value> = arr
                .iter()
                .map(|t| {
                    let map: serde_json::Map<String, serde_json::Value> = t
                        .iter()
                        .filter_map(|(k, v)| toml_item_to_json(v).map(|jv| (k.to_string(), jv)))
                        .collect();
                    serde_json::Value::Object(map)
                })
                .collect();
            Some(serde_json::Value::Array(items))
        }
        toml_edit::Item::None => None,
    }
}

/// Insert default rows for the `routing.*` settings if they're not already
/// present in `system_config`. Never overwrites an existing row — users can
/// edit these via the Database Config tab in Settings.
async fn seed_routing_defaults(db: &DatabaseConnection) {
    let defaults: &[(&str, &str)] = &[
        // Empty string = "no default country", forces callers to send E.164.
        (ROUTING_DEFAULT_COUNTRY_KEY, "\"\""),
        // false = legacy regex-route fallback still used when DID not found.
        (ROUTING_DID_STRICT_MODE_KEY, "false"),
    ];
    for (key, value) in defaults {
        match system_config::Model::get(db, key).await {
            Ok(Some(_)) => {} // already present, leave alone
            Ok(None) => {
                if let Err(e) = system_config::Model::upsert(db, key, value, false).await {
                    warn!(key = %key, error = %e, "seed_routing_defaults: failed to insert");
                }
            }
            Err(e) => warn!(key = %key, error = %e, "seed_routing_defaults: read failed"),
        }
    }
}

/// Read `routing.default_country`.
///
/// Returns `None` when the key is unset or blank. The value is normalised to
/// an upper-case ISO 3166-1 alpha-2 string (`"US"`, `"GB"`, …). Stored in the
/// DB as a JSON-encoded string — accepts raw strings too, to keep the reader
/// robust against hand-edited rows.
pub async fn read_default_country(db: &DatabaseConnection) -> Option<String> {
    let row = system_config::Model::get(db, ROUTING_DEFAULT_COUNTRY_KEY)
        .await
        .ok()
        .flatten()?;
    let decoded: Option<String> = if row.value.trim_start().starts_with('"') {
        serde_json::from_str(&row.value).ok()
    } else {
        Some(row.value)
    };
    decoded.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_ascii_uppercase())
        }
    })
}

/// Read `routing.did_strict_mode`. Defaults to `false` when missing or
/// unparseable.
pub async fn read_did_strict_mode(db: &DatabaseConnection) -> bool {
    let Some(row) = system_config::Model::get(db, ROUTING_DID_STRICT_MODE_KEY)
        .await
        .ok()
        .flatten()
    else {
        return false;
    };
    serde_json::from_str::<bool>(row.value.trim()).unwrap_or(false)
}
