# DB-Backed Configuration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist runtime configuration in the database so the console UI can manage all settings without touching files; on every startup the server merges `config.toml` (base) with DB overrides to produce `config.generated.toml` which is what actually boots.

**Architecture:** A new `system_config` key-value DB table stores overrides. A `config_merge` module runs at startup: it reads `database_url` from the TOML file, connects to DB, auto-detects external IP, applies overrides, and writes `config.generated.toml`. The server then loads the generated file normally — zero changes to `Config::load()` or any call/media/proxy code. Console settings handlers replace `toml_edit` + `persist_document` with DB upserts.

**Tech Stack:** Rust, sea-orm 1.1.20 (sqlx-sqlite), toml_edit (already in Cargo.toml), reqwest 0.13 (already in Cargo.toml), serde_json.

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `src/models/system_config.rs` | Create | sea-orm entity, migration, upsert/get helpers |
| `src/models/mod.rs` | Modify | register system_config module |
| `src/models/migration.rs` | Modify | add system_config migration to Migrator |
| `src/ip_detect.rs` | Create | public IP auto-detection with 4-source fallback chain |
| `src/config_merge.rs` | Create | merge config.toml + DB overrides → config.generated.toml |
| `src/lib.rs` | Modify | expose ip_detect and config_merge modules |
| `src/bin/rustpbx.rs` | Modify | call config_merge::apply() before Config::load() |
| `src/console/handlers/setting.rs` | Modify | replace persist_document with DB upserts in 5 handlers |

---

## Task 1: system_config DB Entity + Migration

**Files:**
- Create: `src/models/system_config.rs`
- Modify: `src/models/mod.rs`
- Modify: `src/models/migration.rs`

- [ ] **Step 1: Create `src/models/system_config.rs`**

```rust
use chrono::Utc;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, DatabaseConnection};
use sea_orm_migration::prelude::{ColumnDef as MigrationColumnDef, *};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "system_config")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    /// JSON-encoded value (string, number, bool, array, or object)
    pub value: String,
    /// When true, auto-detection (e.g. external_ip) is skipped for this key
    pub is_override: bool,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Fetch all rows.
    pub async fn get_all(db: &DatabaseConnection) -> Result<Vec<Self>, DbErr> {
        Entity::find().all(db).await
    }

    /// Insert or update a single key.
    pub async fn upsert(
        db: &DatabaseConnection,
        key: &str,
        value: &str,
        is_override: bool,
    ) -> Result<(), DbErr> {
        let active = ActiveModel {
            key: Set(key.to_owned()),
            value: Set(value.to_owned()),
            is_override: Set(is_override),
            updated_at: Set(Utc::now()),
        };
        Entity::insert(active)
            .on_conflict(
                OnConflict::column(Column::Key)
                    .update_columns([Column::Value, Column::IsOverride, Column::UpdatedAt])
                    .to_owned(),
            )
            .exec(db)
            .await?;
        Ok(())
    }

    /// Fetch a single key.
    pub async fn get(db: &DatabaseConnection, key: &str) -> Result<Option<Self>, DbErr> {
        Entity::find_by_id(key.to_owned()).one(db).await
    }

    /// True when the table has no rows (first-run detection).
    pub async fn is_empty(db: &DatabaseConnection) -> Result<bool, DbErr> {
        use sea_orm::PaginatorTrait;
        Ok(Entity::find().count(db).await? == 0)
    }
}

// ─── Migration ───────────────────────────────────────────────────────────────

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260411_000001_create_system_config"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Entity)
                    .if_not_exists()
                    .col(
                        MigrationColumnDef::new(Column::Key)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::Value)
                            .text()
                            .not_null(),
                    )
                    .col(
                        MigrationColumnDef::new(Column::IsOverride)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        MigrationColumnDef::new(Column::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Entity).to_owned())
            .await
    }
}
```

- [ ] **Step 2: Register module in `src/models/mod.rs`**

Add after the existing `pub mod system_notification;` line:
```rust
pub mod system_config;
```

- [ ] **Step 3: Register migration in `src/models/migration.rs`**

Add at the end of the `vec![...]` in `migrations()`:
```rust
Box::new(super::system_config::Migration),
```

- [ ] **Step 4: Build and verify it compiles**

```bash
cargo build 2>&1 | grep -E "^error" | head -20
```
Expected: no errors.

---

## Task 2: External IP Auto-Detection

**Files:**
- Create: `src/ip_detect.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Create `src/ip_detect.rs`**

```rust
use std::time::Duration;
use tracing::{info, warn};

/// Detect the public IP of this machine.
///
/// Tries sources in order with individual timeouts. Total budget is ~5 s.
/// Always returns *something* — falls back to the local interface IP so the
/// server can start even with no internet access.
pub async fn detect_public_ip() -> Option<String> {
    // 1. AWS EC2 instance metadata (works inside AWS VPC, fast)
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .danger_accept_invalid_certs(false)
        .build()
        .ok()?;

    if let Ok(resp) = client
        .get("http://169.254.169.254/latest/meta-data/public-ipv4")
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                let ip = text.trim().to_string();
                if looks_like_ip(&ip) {
                    info!(ip, "external_ip detected via AWS EC2 metadata");
                    return Some(ip);
                }
            }
        }
    }

    // 2. GCP instance metadata
    let gcp = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .ok()?;
    if let Ok(resp) = gcp
        .get(
            "http://metadata.google.internal/computeMetadata/v1/\
             instance/network-interfaces/0/access-configs/0/externalIp",
        )
        .header("Metadata-Flavor", "Google")
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                let ip = text.trim().to_string();
                if looks_like_ip(&ip) {
                    info!(ip, "external_ip detected via GCP metadata");
                    return Some(ip);
                }
            }
        }
    }

    // 3. Public API — works on any internet-connected host
    let pub_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    if let Ok(resp) = pub_client.get("https://api.ipify.org").send().await {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                let ip = text.trim().to_string();
                if looks_like_ip(&ip) {
                    info!(ip, "external_ip detected via api.ipify.org");
                    return Some(ip);
                }
            }
        }
    }

    // 4. Local interface fallback — UDP connect trick (no packets sent)
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip().to_string();
                warn!(ip, "external_ip falling back to local interface address");
                return Some(ip);
            }
        }
    }

    warn!("external_ip detection failed — all sources unavailable");
    None
}

fn looks_like_ip(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}
```

- [ ] **Step 2: Add module to `src/lib.rs`**

Add after the last `pub mod` line:
```rust
pub mod ip_detect;
pub mod config_merge;
```

- [ ] **Step 3: Build check**

```bash
cargo build 2>&1 | grep -E "^error" | head -20
```
Expected: error about `config_merge` not existing yet — that's fine, Task 3 creates it. The `ip_detect` module itself should compile cleanly.

---

## Task 3: Config Merge Logic

**Files:**
- Create: `src/config_merge.rs`

- [ ] **Step 1: Create `src/config_merge.rs`**

```rust
//! Merges `config.toml` (base) with `system_config` DB overrides and writes
//! `config.generated.toml` alongside the base file.
//!
//! Called once at startup, before `Config::load()`. The rest of the system
//! is unmodified — it just loads the generated file instead of the base.

use anyhow::{Context, Result};
use sea_orm::DatabaseConnection;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value};
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
```

- [ ] **Step 2: Build and verify it compiles**

```bash
cargo build 2>&1 | grep -E "^error" | head -20
```
Expected: no errors.

---

## Task 4: Pre-Boot Merge Step in rustpbx.rs

**Files:**
- Modify: `src/bin/rustpbx.rs`

The change is in two places:
1. The initial config load (lines ~88-95) — run merge, get effective path
2. The restart loop (lines ~298-328) — re-run merge on each restart

- [ ] **Step 1: Add `get_effective_config_path` helper and update initial load**

Find this block in `src/bin/rustpbx.rs` (around line 88):
```rust
    let config_path = cli.conf.clone();
    let config = if let Some(ref path) = config_path {
        println!("Loading config from: {}", path);
        Config::load(path).expect("Failed to load config")
    } else {
        println!("Loading default config");
        Config::default()
    };
```

Replace with:
```rust
    let config_path = cli.conf.clone();

    // Merge base config with DB overrides → config.generated.toml.
    // Falls back to the original path if the merge step fails (e.g. no DB yet).
    let effective_config_path = if let Some(ref path) = config_path {
        match rustpbx::config_merge::apply(path).await {
            Ok(generated) => {
                println!("Config merged: {}", generated);
                Some(generated)
            }
            Err(e) => {
                println!("Config merge skipped ({}), using base config.", e);
                Some(path.clone())
            }
        }
    } else {
        None
    };

    let config = if let Some(ref path) = effective_config_path {
        println!("Loading config from: {}", path);
        Config::load(path).expect("Failed to load config")
    } else {
        println!("Loading default config");
        Config::default()
    };
```

- [ ] **Step 2: Update the restart loop to re-run merge on each restart**

Find the restart loop block (around line 298):
```rust
        let config = if let Some(cfg) = cached_config.take() {
            cfg
        } else if let Some(ref path) = next_config_path {
            match Config::load(path) {
                Ok(cfg) => cfg,
                Err(err) => {
```

Replace with:
```rust
        let config = if let Some(cfg) = cached_config.take() {
            cfg
        } else if let Some(ref path) = next_config_path {
            // Re-run merge on restart to pick up any new DB overrides
            let effective = match rustpbx::config_merge::apply(path).await {
                Ok(generated) => generated,
                Err(e) => {
                    tracing::warn!("Config merge skipped on restart ({}), using base.", e);
                    path.clone()
                }
            };
            match Config::load(&effective) {
                Ok(cfg) => cfg,
                Err(err) => {
```

Also update `next_config_path` to hold the ORIGINAL `config.toml` path (not the generated one) so the merge always starts from the base. Find:
```rust
    let mut next_config_path = config_path.clone();
```
Change to:
```rust
    let mut next_config_path = config_path.clone(); // always base config.toml
```
(No change needed — `config_path` is already the original path. Just confirm `next_config_path` uses `config_path`, not `effective_config_path`.)

- [ ] **Step 3: Build and verify**

```bash
cargo build 2>&1 | grep -E "^error" | head -20
```
Expected: no errors.

- [ ] **Step 4: Smoke test — run with existing config.toml**

```bash
cargo run --bin rustpbx -- --conf config.toml 2>&1 | head -20
```
Expected output includes:
```
Config merged: ./config.generated.toml
Loading config from: ./config.generated.toml
```
And `config.generated.toml` is created alongside `config.toml`.

---

## Task 5: Console Settings Handlers → DB Writes

**Files:**
- Modify: `src/console/handlers/setting.rs`

There are 5 handlers to update. The pattern for each is:
- Keep `load_document` + TOML mutations for **validation only**
- Replace the final `persist_document(...)` call with DB upserts
- Remove the `get_config_path` call if the handler no longer needs to write the file

Add this import at the top of `setting.rs` (with the existing imports):
```rust
use crate::models::system_config;
```

And add this helper at the bottom of `setting.rs` (before `get_config_path`):
```rust
/// Get the DB connection from the console state, returning an error Response if unavailable.
fn get_db(state: &ConsoleState) -> Result<sea_orm::DatabaseConnection, Response> {
    state
        .app_state()
        .map(|s| s.db().clone())
        .ok_or_else(|| json_error(StatusCode::SERVICE_UNAVAILABLE, "Application state unavailable"))
}
```

### 5a: `update_platform_settings`

- [ ] **Step 1: Rewrite `update_platform_settings`**

Find `pub(crate) async fn update_platform_settings` and replace it with:

```rust
pub(crate) async fn update_platform_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<PlatformSettingsPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }

    let db = match get_db(&state) {
        Ok(db) => db,
        Err(r) => return r,
    };

    // Load base config for validation only (not written back to disk)
    let config_path = match get_config_path(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut doc = match load_document(&config_path) {
        Ok(d) => d,
        Err(r) => return r,
    };

    let mut overrides: Vec<(&'static str, serde_json::Value)> = Vec::new();
    let mut modified = false;

    if let Some(level_opt) = payload.log_level {
        if let Some(level) = normalize_opt_string(level_opt) {
            doc["log_level"] = value(level.clone());
            overrides.push(("log_level", serde_json::json!(level)));
        } else {
            doc.remove("log_level");
        }
        modified = true;
    }

    if let Some(file_opt) = payload.log_file {
        if let Some(path) = normalize_opt_string(file_opt) {
            doc["log_file"] = value(path.clone());
            overrides.push(("log_file", serde_json::json!(path)));
        } else {
            doc.remove("log_file");
        }
        modified = true;
    }

    if let Some(ext_opt) = payload.external_ip {
        if let Some(ip) = normalize_opt_string(ext_opt) {
            doc["external_ip"] = value(ip.clone());
            overrides.push(("external_ip", serde_json::json!(ip)));
        } else {
            doc.remove("external_ip");
        }
        modified = true;
    }

    if let Some(start_opt) = payload.rtp_start_port {
        if let Some(port) = start_opt {
            if port == 0 {
                return json_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "rtp_start_port must be greater than 0",
                );
            }
            doc["rtp_start_port"] = value(i64::from(port));
            overrides.push(("rtp_start_port", serde_json::json!(port)));
        } else {
            doc.remove("rtp_start_port");
        }
        modified = true;
    }

    if let Some(end_opt) = payload.rtp_end_port {
        if let Some(port) = end_opt {
            if port == 0 {
                return json_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "rtp_end_port must be greater than 0",
                );
            }
            doc["rtp_end_port"] = value(i64::from(port));
            overrides.push(("rtp_end_port", serde_json::json!(port)));
        } else {
            doc.remove("rtp_end_port");
        }
        modified = true;
    }

    // Validate the merged document as a Config
    let doc_text = doc.to_string();
    let config = match parse_config_from_str(&doc_text) {
        Ok(cfg) => cfg,
        Err(resp) => return resp,
    };

    if let (Some(start), Some(end)) = (config.rtp_start_port, config.rtp_end_port) {
        if start > end {
            return json_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "rtp_start_port must be less than or equal to rtp_end_port",
            );
        }
    }

    // Persist to DB (not to disk)
    if modified {
        for (key, val) in overrides {
            let is_override = key == "external_ip"; // manual IP = skip auto-detection
            if let Err(e) =
                system_config::Model::upsert(&db, key, &val.to_string(), is_override).await
            {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to save setting '{key}': {e}"),
                );
            }
        }
    }

    Json(json!({
        "status": "ok",
        "requires_restart": true,
        "message": "Platform settings saved. Restart RustPBX to apply changes.",
        "platform": {
            "log_level": config.log_level,
            "log_file": config.log_file,
        },
        "rtp": {
            "external_ip": config.external_ip,
            "start_port": config.rtp_start_port,
            "end_port": config.rtp_end_port,
        }
    }))
    .into_response()
}
```

### 5b: `update_proxy_settings`

- [ ] **Step 2: Rewrite `update_proxy_settings`**

Find `pub(crate) async fn update_proxy_settings` and replace the body from `let config_path = ...` through `persist_document` call. Keep the permission check at the top. Replace the `get_config_path` / `load_document` / `persist_document` block with:

```rust
pub(crate) async fn update_proxy_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<ProxySettingsPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }

    let db = match get_db(&state) {
        Ok(db) => db,
        Err(r) => return r,
    };

    let config_path = match get_config_path(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut doc = match load_document(&config_path) {
        Ok(d) => d,
        Err(r) => return r,
    };

    let mut overrides: Vec<(&'static str, serde_json::Value)> = Vec::new();
    let mut modified = false;
    let table = ensure_table_mut(&mut doc, "proxy");

    if let Some(realms) = payload.realms {
        set_string_array(table, "realms", realms.clone());
        overrides.push(("proxy.realms", serde_json::json!(realms)));
        modified = true;
    }

    if let Some(webhook) = payload.locator_webhook {
        let toml_s = toml::to_string(&webhook).unwrap_or_default();
        if let Ok(new_doc) = toml_s.parse::<DocumentMut>() {
            table["locator_webhook"] = new_doc.as_item().clone();
        }
        overrides.push(("proxy.locator_webhook", serde_json::to_value(&webhook).unwrap_or_default()));
        modified = true;
    }

    if let Some(backends) = payload.user_backends {
        let toml_s = toml::to_string(&json!({ "b": backends })).unwrap_or_default();
        if let Ok(new_doc) = toml_s.parse::<DocumentMut>() {
            table["user_backends"] = new_doc["b"].clone();
        }
        overrides.push(("proxy.user_backends", serde_json::to_value(&backends).unwrap_or_default()));
        modified = true;
    }

    if let Some(router) = payload.http_router {
        let toml_s = toml::to_string(&router).unwrap_or_default();
        if let Ok(new_doc) = toml_s.parse::<DocumentMut>() {
            table["http_router"] = new_doc.as_item().clone();
        }
        overrides.push(("proxy.http_router", serde_json::to_value(&router).unwrap_or_default()));
        modified = true;
    }

    if modified {
        let doc_text = doc.to_string();
        if let Err(resp) = parse_config_from_str(&doc_text) {
            return resp;
        }
        for (key, val) in overrides {
            if let Err(e) =
                system_config::Model::upsert(&db, key, &val.to_string(), false).await
            {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to save setting '{key}': {e}"),
                );
            }
        }
    }

    Json(json!({
        "status": "ok",
        "message": "Proxy settings saved. Restart required to apply.",
    }))
    .into_response()
}
```

### 5c: `update_storage_settings`

- [ ] **Step 3: Replace `persist_document` in `update_storage_settings`**

Find this block inside `update_storage_settings` (around line 1820):
```rust
    if modified {
        if let Err(resp) = persist_document(&config_path, doc_text) {
            return resp;
        }
    }
```

Replace with:
```rust
    if modified {
        // Collect effective storage values from the validated config for DB persistence
        let db = match get_db(&state) {
            Ok(db) => db,
            Err(r) => return r,
        };
        let storage_overrides: &[(&str, serde_json::Value)] = &[
            ("recording.path", serde_json::json!(config.recorder_path())),
            ("media_cache_path", serde_json::json!(config.media_cache_path.clone().unwrap_or_default())),
        ];
        for (key, val) in storage_overrides {
            if !val.as_str().map(|s| s.is_empty()).unwrap_or(true) {
                if let Err(e) = system_config::Model::upsert(&db, key, &val.to_string(), false).await {
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to save setting '{key}': {e}"),
                    );
                }
            }
        }
    }
```

### 5d: `update_security_settings`

- [ ] **Step 4: Replace `persist_document` in `update_security_settings`**

Find the `persist_document` call inside `update_security_settings` (around line 1882):
```rust
    if modified {
        if let Err(resp) = persist_document(&config_path, doc_text) {
            return resp;
        }
    }
```

Replace with:
```rust
    if modified {
        let db = match get_db(&state) {
            Ok(db) => db,
            Err(r) => return r,
        };
        let rules = config.proxy.acl_rules.clone().unwrap_or_default();
        if let Err(e) = system_config::Model::upsert(
            &db,
            "proxy.acl_rules",
            &serde_json::to_string(&rules).unwrap_or_default(),
            false,
        )
        .await
        {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to save acl_rules: {e}"),
            );
        }
    }
```

### 5e: `update_rwi_settings`

- [ ] **Step 5: Read `update_rwi_settings` and replace `persist_document`**

Find the `persist_document` call inside `update_rwi_settings`. Replace the entire final `if modified { persist_document... }` block with:

```rust
    if modified {
        let db = match get_db(&state) {
            Ok(db) => db,
            Err(r) => return r,
        };
        if let Err(e) = system_config::Model::upsert(
            &db,
            "rwi",
            &serde_json::to_string(&config.rwi).unwrap_or_default(),
            false,
        )
        .await
        {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to save rwi settings: {e}"),
            );
        }
    }
```

- [ ] **Step 6: Build and verify all handlers compile**

```bash
cargo build 2>&1 | grep -E "^error" | head -30
```
Expected: no errors.

---

## Task 6: End-to-End Verification

- [ ] **Step 1: Start the server with the existing config.toml**

```bash
cargo run --bin rustpbx -- --conf config.toml 2>&1 | head -30
```

Expected log lines:
```
Config merged: ./config.generated.toml
Loading config from: ./config.generated.toml
SQLite tuned for high concurrency...
trunks reloaded total=1 ...
```

- [ ] **Step 2: Verify `config.generated.toml` was created**

```bash
cat config.generated.toml | head -20
```
Expected: valid TOML with all existing settings from `config.toml` plus `external_ip` filled in.

- [ ] **Step 3: Verify `system_config` table was seeded**

```bash
sqlite3 rustpbx.sqlite3 "SELECT key, value FROM system_config ORDER BY key;"
```
Expected: at minimum an `external_ip` row.

- [ ] **Step 4: Simulate a console settings change**

```bash
sqlite3 rustpbx.sqlite3 \
  "INSERT OR REPLACE INTO system_config(key,value,is_override,updated_at) \
   VALUES('log_level','\"debug\"',0,datetime('now'));"
```

Restart the server and verify:
```bash
cargo run --bin rustpbx -- --conf config.toml 2>&1 | grep log_level
```
Expected: `config.generated.toml` now contains `log_level = "debug"`.

- [ ] **Step 5: Verify external IP override flag**

```bash
sqlite3 rustpbx.sqlite3 \
  "UPDATE system_config SET is_override=1 WHERE key='external_ip';"
```
Restart and verify in logs: `external_ip: using manual override, skipping detection`.
