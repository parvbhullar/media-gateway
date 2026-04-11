# DB-Backed Configuration Design

**Date:** 2026-04-11
**Status:** Approved

---

## Problem

All configuration currently lives in `config.toml`. Changing any setting requires editing a file on disk and restarting the service. There is no way to manage configuration through the console UI in a persistent way. The TOML file is the only source of truth, so console-applied changes are lost on restart.

---

## Goal

Allow runtime settings to be managed via the console UI and persisted in the database. On every startup the server merges the base `config.toml` with DB overrides and boots from a generated final config file. **No changes are required anywhere else in the system** — `Config::load()`, hot-reload, and the service file all stay exactly as they are today.

---

## Core Idea

```
config.toml  (base — operator-edited template)
     +
system_config table  (DB overrides — console UI writes here)
     │
     └──── merge step (runs at startup, before Config::load)
                  │
                  ▼
         config.generated.toml  (final merged config)
                  │
                  ▼
          Config::load("config.generated.toml")
          ← everything else unchanged
```

`config.toml` remains the human-editable base. The DB holds only the delta — values changed via the console UI. `config.generated.toml` is the single file the server actually boots from. It is always present and inspectable.

---

## Startup Flow

```
rustpbx --conf config.toml
          │
          ├─ Minimal parse of config.toml → extract database_url only
          │   (fatal error if missing)
          │
          ├─ Connect to DB, run migrations
          │
          ├─ First run? (system_config table is empty)
          │   YES:
          │     → Auto-detect external_ip → insert into system_config
          │     → Log: "First run: external_ip detected as x.x.x.x"
          │   NO:
          │     → Auto-detect external_ip (unless is_override=true)
          │     → If changed from DB value → update system_config
          │     → Log: "external_ip updated: old → new"  (only if changed)
          │
          ├─ Load full config.toml
          ├─ Load overrides from system_config table
          ├─ Merge: DB values overwrite matching config.toml values
          ├─ Write merged result to config.generated.toml
          │
          └─ Config::load("config.generated.toml")
             → normal server startup, nothing else changes
```

The `--conf config.toml` argument is preserved. The server uses it to locate both the base config and the generated output path (`config.generated.toml` lives alongside `config.toml`).

---

## Database Schema — `system_config` Table

A flat key-value store. Keys use dot notation mirroring the TOML structure.

```sql
CREATE TABLE system_config (
    key         TEXT    PRIMARY KEY,
    value       TEXT    NOT NULL,               -- JSON-encoded value
    is_override BOOLEAN NOT NULL DEFAULT FALSE, -- TRUE = skip auto-detection
    updated_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

**Example rows after a user changes settings via console:**

| key | value | is_override |
|-----|-------|-------------|
| `external_ip` | `"13.202.41.67"` | false |
| `proxy.udp_port` | `15060` | false |
| `log_level` | `"debug"` | false |
| `recording.enabled` | `true` | false |
| `proxy.registrar_expires` | `120` | false |

Only keys that differ from `config.toml` need to be in this table. The table is empty on first run — the base `config.toml` values are used as-is except for `external_ip` which is auto-detected.

---

## Merge Rules

1. Start with the full parsed `config.toml` as a TOML document (using `toml_edit` to preserve formatting/comments is not required here — we just need the values)
2. For each row in `system_config`: set the corresponding key in the document to the DB value
3. Write the resulting document to `config.generated.toml`
4. `config.generated.toml` is a complete, valid TOML file — every key is present

**Key mapping** (dot notation → TOML path):

| DB key | TOML path |
|--------|-----------|
| `external_ip` | `external_ip` |
| `log_level` | `log_level` |
| `proxy.udp_port` | `[proxy] udp_port` |
| `proxy.media_proxy` | `[proxy] media_proxy` |
| `rtp.start_port` | `rtp_start_port` |
| `rtp.end_port` | `rtp_end_port` |
| `recording.enabled` | `[recording] enabled` |
| `recording.path` | `[recording] path` |
| `proxy.registrar_expires` | `[proxy] registrar_expires` |
| `proxy.realms` | `[proxy] realms` |

---

## External IP Auto-Detection

Runs on every startup when `is_override=false` (or key not in DB yet).

```
1. AWS EC2 metadata:  http://169.254.169.254/latest/meta-data/public-ipv4    (1s timeout)
2. GCP metadata:      http://metadata.google.internal/...                     (1s timeout)
3. Public API:        https://api.ipify.org                                   (3s timeout)
4. Fallback:          first non-loopback, non-link-local network interface IP
```

Total budget: 5 seconds. Step 4 always succeeds — server never blocks on network.

Result is written to `system_config` (key=`external_ip`, `is_override=false`) and included in `config.generated.toml`.

When `is_override=true`: skip detection entirely, use the DB value as-is.

---

## Console UI — Settings Page

The existing settings page handlers are updated to write to `system_config` instead of using `toml_edit` to patch `config.toml` on disk.

**Read:** Query `system_config` for current overrides. For keys not in DB, read from the last `config.generated.toml` (or `config.toml` as fallback) to show current effective values.

**Write:** Validate input → upsert into `system_config`. Return `requires_restart: true` (unchanged from today — changes take effect on next restart when the generated file is rebuilt).

**External IP override toggle:** Sets `is_override=true/false` on the `external_ip` row. When off, the UI displays the auto-detected value with a note: *"auto-detected — updates on restart"*.

---

## Migration Path for Existing Deployments

Existing deployments have a fully configured `config.toml`. On the first restart after this change:

1. `system_config` table is empty — first run
2. Auto-detect external IP → insert into `system_config`
3. Merge: `config.toml` + `system_config` (only external_ip) → write `config.generated.toml`
4. Server boots from `config.generated.toml`

All existing settings in `config.toml` are preserved unchanged in the generated file. The operator sees no difference in behaviour. On subsequent console UI changes, those values accumulate in `system_config` and are reflected in the generated file on next restart.

---

## Error Handling

| Scenario | Behaviour |
|----------|-----------|
| `config.toml` missing | Fatal: "config.toml not found" (unchanged from today) |
| `database_url` missing from `config.toml` | Fatal: "database_url required in config.toml" |
| DB connection fails | Fatal: server cannot generate config without DB |
| `system_config` empty (first run) | Auto-detect IP, write generated file from base config |
| External IP detection fails (all sources) | Use `config.toml` value if present, else empty string; log warning |
| DB key has malformed JSON value | Skip that key, log warning, use `config.toml` value |
| `config.generated.toml` write fails (disk full, permissions) | Fatal: cannot start without writable config |

---

## Files Affected

| File | Change |
|------|--------|
| `src/models/mod.rs` | Add `system_config` module |
| `src/models/migration/` | New migration: create `system_config` table |
| `src/models/system_config.rs` | New: sea-orm entity + CRUD helpers |
| `src/config_merge.rs` | New: merge logic (base TOML + DB overrides → generated file) |
| `src/ip_detect.rs` | New: external IP auto-detection with fallback chain |
| `src/bin/rustpbx.rs` | Add pre-boot merge step before `Config::load()` (~30 lines) |
| `src/console/handlers/setting.rs` | Replace `toml_edit` writes with DB upserts |

**Everything else stays unchanged:** `Config::load()`, `SipServerBuilder`, hot-reload, service file, all call/media/proxy code.

---

## Out of Scope

- Live hot-reload without restart (future work)
- Config history / audit log (add `system_config_history` table later)
- Multi-node config sync
- Secrets encryption in DB
- Removing `config.toml` entirely (it remains the base template)
