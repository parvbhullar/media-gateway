# DB-Backed Configuration Design

**Date:** 2026-04-11
**Status:** Approved

---

## Problem

All configuration currently lives in `config.toml`. Changing any setting requires editing a file on disk and restarting the service. There is no way to manage configuration through the console UI in a persistent, file-free way. The TOML file mixes bootstrap values (database URL) with runtime values (external IP, SIP ports, recording paths) making deployments fragile.

---

## Goal

Move all runtime configuration into the database. The only value that must remain in `config.toml` is `database_url`. Everything else is read from DB at startup, auto-seeded on first run, and manageable through the console UI without touching any files.

---

## Design

### config.toml — Minimal Bootstrap

After this change, the only required content of `config.toml` is:

```toml
database_url = "sqlite://rustpbx.sqlite3"
```

The server will refuse to start if `database_url` is missing. All other fields in `config.toml` are ignored once DB config is seeded (first run only).

The `--conf` CLI flag continues to work as today — it points to the TOML file that contains `database_url`.

---

### Database Schema — `system_config` Table

A flat key-value table. Keys use dot notation to mirror the existing config struct hierarchy.

```sql
CREATE TABLE system_config (
    key         TEXT    PRIMARY KEY,
    value       TEXT    NOT NULL,        -- JSON-encoded value
    is_override BOOLEAN NOT NULL DEFAULT FALSE,  -- TRUE = skip auto-detection
    updated_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

**Example rows:**

| key | value | is_override |
|-----|-------|-------------|
| `http_addr` | `"0.0.0.0:8080"` | false |
| `external_ip` | `"13.202.41.67"` | false |
| `proxy.udp_port` | `5060` | false |
| `proxy.media_proxy` | `"auto"` | false |
| `rtp.start_port` | `12000` | false |
| `rtp.end_port` | `42000` | false |
| `log_level` | `"info"` | false |
| `recording.enabled` | `true` | false |
| `recording.path` | `"./config/recorders"` | false |
| `proxy.registrar_expires` | `60` | false |

All values are JSON-encoded so booleans, integers, strings, arrays, and objects are all handled uniformly.

---

### Startup Flow

```
rustpbx --conf config.toml
          │
          ├─ Parse config.toml → extract database_url only
          │   (error if missing)
          │
          ├─ Connect to DB, run migrations
          │
          ├─ First run? (system_config table is empty)
          │   YES:
          │     → Seed DB with compiled-in defaults (see Defaults section)
          │     → Auto-detect external_ip → save with is_override=false
          │     → Log: "First run: config seeded, external_ip=x.x.x.x"
          │
          ├─ Load Config from DB (all keys)
          │
          ├─ Auto-detect external_ip
          │   is_override=false:
          │     → Detect current public IP
          │     → If changed from DB value → update DB, log "IP updated: old→new"
          │     → Use detected IP
          │   is_override=true:
          │     → Skip detection, use DB value as-is
          │
          └─ Build full Config struct → start server
```

---

### External IP Auto-Detection

Detection is attempted on every startup when `is_override=false`. A chain of sources is tried in order with per-source timeouts. The entire detection has a 5-second total budget.

```
1. AWS EC2 metadata:  http://169.254.169.254/latest/meta-data/public-ipv4   (1s)
2. GCP metadata:      http://metadata.google.internal/computeMetadata/v1/
                        instance/network-interfaces/0/access-configs/0/externalIp  (1s)
3. Public API:        https://api.ipify.org                                 (3s)
4. Fallback:          first non-loopback, non-link-local interface IP
```

The fallback (step 4) always succeeds — it guarantees the server starts even with no internet access.

When the detected IP differs from the DB value, the DB is updated and an `INFO` log line is emitted. No restart is required for the detection itself since it runs before the server binds ports.

---

### Config Load Priority

```
compiled-in defaults  (lowest)
       ↓
  DB values
       ↓
  env vars  (highest)
```

Env var naming: `RUSTPBX_` prefix + key uppercased with dots replaced by underscores.
Examples: `RUSTPBX_EXTERNAL_IP=1.2.3.4`, `RUSTPBX_PROXY_UDP_PORT=5080`, `RUSTPBX_LOG_LEVEL=debug`.

Env vars allow CI/CD and container deployments to override specific values without a DB write.

---

### Compiled-In Defaults

These are the values seeded into DB on first run. They match current `Config::default()` behaviour exactly so existing deployments see no change in behaviour.

| Key | Default |
|-----|---------|
| `http_addr` | `"0.0.0.0:8080"` |
| `log_level` | `"info"` |
| `proxy.addr` | `"0.0.0.0"` |
| `proxy.udp_port` | `5060` |
| `proxy.media_proxy` | `"auto"` |
| `proxy.registrar_expires` | `60` |
| `proxy.modules` | `["acl","auth","registrar","call"]` |
| `proxy.nat_fix` | `true` |
| `proxy.ensure_user` | `true` |
| `proxy.generated_dir` | `"./config"` |
| `rtp.start_port` | `12000` |
| `rtp.end_port` | `42000` |
| `recording.enabled` | `false` |
| `recording.auto_start` | `true` |
| `recording.path` | `"./config/recorders"` |
| `callrecord.type` | `"local"` |
| `callrecord.root` | `"./config/cdr"` |
| `external_ip` | *(auto-detected)* |

---

### Console UI — Settings Page

The existing settings page handlers (`/settings/config/platform`, `/settings/config/proxy`, `/settings/config/storage`, etc.) are updated to read from and write to `system_config` instead of calling `toml_edit` on `config.toml`.

**Read:** `GET /settings` — query all keys from `system_config`, build response JSON (same shape as today).

**Write:** `PATCH /settings/config/*` — validate input, upsert into `system_config`. Return `requires_restart` flag for settings that need it (same as today).

**External IP override:** A toggle on the platform settings page sets `is_override=true/false` for the `external_ip` key. When override is off, the displayed value shows the auto-detected IP with a note "auto-detected, updates on restart".

---

### Migration Path for Existing Deployments

Existing deployments have a fully configured `config.toml`. On the first restart after this change:

1. Server reads `database_url` from `config.toml`
2. `system_config` table is empty (first run for this feature)
3. **Seed from existing `config.toml`** — parse the full file, extract known keys, insert into DB
4. Auto-detect external IP, update DB if changed
5. Log: "Config migrated from config.toml to database"

This means existing deployments automatically carry over their current settings. No manual intervention required.

---

### Error Handling

| Scenario | Behaviour |
|----------|-----------|
| `config.toml` missing | Fatal error: "config.toml not found — at minimum, database_url must be set" |
| `database_url` missing from TOML | Fatal error with message pointing to docs |
| DB connection fails | Fatal error — server cannot start without DB |
| `system_config` table empty, first run | Seed defaults + auto-detect IP, continue |
| External IP detection fails entirely | Use `""` (empty), log warning "external_ip not detected — set manually in console" |
| Unknown key in `system_config` | Ignored (forward compatibility for future keys) |
| Env var override malformed | Log warning, use DB value |

---

### Files Affected

| File | Change |
|------|--------|
| `src/models/mod.rs` | Add `system_config` module |
| `src/models/migration/` | New migration: create `system_config` table |
| `src/models/system_config.rs` | New: sea-orm entity + CRUD helpers |
| `src/config.rs` | `Config::load()` reads `database_url` only from TOML; new `Config::load_from_db()` |
| `src/bin/rustpbx.rs` | Startup flow updated: connect DB first, then load config from DB |
| `src/console/handlers/setting.rs` | All `toml_edit` writes replaced with DB upserts |
| `src/ip_detect.rs` | New: external IP auto-detection with fallback chain |

---

### Out of Scope

- Live hot-reload of all settings without restart (future work — `requires_restart` flag unchanged)
- Config history / audit log (can be added later to `system_config_history` table)
- Multi-node config sync (single-node only for now)
- Secrets management / encryption of sensitive values in DB
