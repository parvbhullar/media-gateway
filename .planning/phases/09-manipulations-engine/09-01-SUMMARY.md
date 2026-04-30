---
phase: 09-manipulations-engine
plan: 01
subsystem: manipulations-engine
tags: [phase-9, wave-1, manipulations, schema, scaffolding, stub]
requires: [TRN-01]
provides: [MAN-01]
affects:
  - src/proxy/manipulation/*
  - src/handler/api_v1/manipulations.rs
  - src/models/manipulations.rs
  - AppState (manipulation_engine handle)
tech-stack:
  added: []
  patterns: [Phase-8-engine-replication, additive-migration, file-ownership-seal]
key-files:
  created:
    - src/models/manipulations.rs
    - src/proxy/manipulation/mod.rs
    - src/proxy/manipulation/engine.rs
    - src/handler/api_v1/manipulations.rs
  modified:
    - src/models/mod.rs
    - src/models/migration.rs
    - src/handler/api_v1/mod.rs
    - src/proxy/mod.rs
    - src/proxy/server.rs
    - src/app.rs
    - src/proxy/tests/common.rs
    - src/proxy/tests/test_auth.rs
decisions:
  - "Wire types live in engine.rs (single-file pattern matching Phase 8) — Wave 2 imports via re-exports from proxy::manipulation"
  - "Stub router returns 501 (NOT_IMPLEMENTED) on all five endpoints; 09-02 swaps in CRUD bodies"
  - "AppState exposes manipulation_engine via accessor that delegates to SipServer.inner (mirrors translation_engine plumbing)"
metrics:
  duration: ~ (continuation of in-progress work)
  completed: 2026-05-01
---

# Phase 9 Plan 01: Manipulations Engine Scaffolding Summary

Schema migration, runtime scaffolding, AppState plumbing, and stub HTTP router for the SIP manipulations engine — wave-1 file-ownership boundary that seals mod.rs / migration.rs / server.rs / app.rs for the rest of Phase 9.

## What Was Built

### Task 1 — Schema migration + entity (commit `185974d`)
- New `src/models/manipulations.rs` (306 lines): `supersip_manipulations` Entity + `Migration` per D-01..D-04.
- Columns: `id` (UUID String PK), `name` (UNIQUE, lowercase+dashes), `description` (Option), `direction` (text default `"both"`), `priority` (i32 default 100), `is_active` (bool default true), `rules` (Json default `[]`), `created_at`, `updated_at` (DateTimeUtc).
- Forward-only migration (Phase 6 D-05 convention). UNIQUE index `idx_supersip_manipulations_name`.
- Registered in `src/models/migration.rs::Migrator::migrations()` after Phase 8 entry; `pub mod manipulations;` added to `src/models/mod.rs`.
- 4 in-memory SQLite migration tests pass: table creation, name uniqueness, direction enum parse, rules JSON round-trip.

### Task 2 — Engine + plumbing + stub router (commit `598e632`)
- New `src/proxy/manipulation/{mod.rs, engine.rs}` with `ManipulationEngine` struct holding `regex_cache: Arc<DashMap<String, Arc<Regex>>>` and `var_scope: Arc<DashMap<String, HashMap<String,String>>>`.
- Stubs:
  - `pub async fn manipulate(&self, &mut InviteOption, ManipulationContext, &DatabaseConnection) -> Result<ManipulationOutcome>` returns `Continue { trace: default }` (marked `// STUB — implementation lands in 09-03`).
  - `pub fn invalidate_class(&self, class_id: &str)` removes cache keys with `{class_id}::` prefix (final behavior — reused unchanged by 09-03).
  - `pub fn cleanup_session(&self, session_id: &str)` removes the var_scope entry (final behavior).
- Frozen wire types (Rule, Condition, Action tagged enum, ConditionMode, ConditionOp, LogLevel, ManipulationContext, ManipulationOutcome, ManipulationTrace) — see contract block below.
- `AppStateInner::manipulation_engine()` accessor delegates to `SipServer.inner.manipulation_engine` (mirrors `translation_engine` pattern).
- Engine constructed eagerly in `SipServerBuilder::build()` next to translation engine.
- Stub router `src/handler/api_v1/manipulations.rs`: `GET / POST /manipulations`, `GET / PUT / DELETE /manipulations/{name}` — all return `(StatusCode::NOT_IMPLEMENTED, "not yet implemented")`. Merged into protected api_v1_router (auth-gated; T-09-01-04 mitigated).
- Test fixtures (`proxy/tests/common.rs`, `proxy/tests/test_auth.rs`) updated to construct the new engine field.
- 9 engine unit tests pass: empty caches on `new()`, stub returns Continue + empty trace, `invalidate_class` prefix-match removal, idempotent `cleanup_session`, full serde round-trips for Rule / Action / Condition variants.

## Frozen Wire-Type Contract (Wave 2 reads verbatim)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConditionMode {
    #[serde(rename = "and")] And,
    #[serde(rename = "or")] Or,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConditionOp { Equals, NotEquals, Regex, NotRegex, StartsWith, Contains }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LogLevel {
    #[serde(rename = "info")] Info,
    #[serde(rename = "warn")] Warn,
    #[serde(rename = "error")] Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition { pub source: String, pub op: ConditionOp, pub value: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    SetHeader   { name: String, value: String },
    RemoveHeader{ name: String },
    SetVar      { name: String, value: String },
    Log         { level: LogLevel, message: String },
    Hangup      { sip_code: u16, reason: String },
    Sleep       { duration_ms: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)] pub name: Option<String>,
    #[serde(default)] pub conditions: Vec<Condition>,
    #[serde(default = "default_condition_mode")] pub condition_mode: ConditionMode,
    #[serde(default)] pub actions: Vec<Action>,
    #[serde(default)] pub anti_actions: Vec<Action>,
}

pub struct ManipulationContext {
    pub caller_number: String,
    pub destination_number: String,
    pub trunk_name: String,
    pub direction: crate::call::DialDirection,
    pub session_id: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ManipulationTrace {
    pub applied_rules: Vec<String>,
    pub triggered_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub enum ManipulationOutcome {
    Continue { trace: ManipulationTrace },
    Hangup   { code: u16, reason: String, trace: ManipulationTrace },
}
```

## File Ownership Seal (Phase 9)

This plan was the LAST in Phase 9 to touch the four high-traffic shared files:
- `src/handler/api_v1/mod.rs` — sealed (Wave 2 09-02 swaps router body inside `manipulations.rs`, NOT the merge line).
- `src/models/migration.rs` — sealed.
- `src/proxy/mod.rs` — sealed.
- `src/proxy/server.rs` — sealed.
- `src/app.rs` — sealed (accessor present; consumers call `state.manipulation_engine()`).
- `src/models/mod.rs` — sealed (`pub mod manipulations;` registered).

Wave 2 plans (09-02 CRUD, 09-03 engine impl, 09-04 hot-path insertion) MUST stay inside their own files.

## Files Modified (12 total — Tasks 1 + 2 combined)

```
src/app.rs
src/handler/api_v1/manipulations.rs        (NEW)
src/handler/api_v1/mod.rs
src/models/manipulations.rs                (NEW)
src/models/migration.rs
src/models/mod.rs
src/proxy/manipulation/engine.rs           (NEW)
src/proxy/manipulation/mod.rs              (NEW)
src/proxy/mod.rs
src/proxy/server.rs
src/proxy/tests/common.rs
src/proxy/tests/test_auth.rs
```

The plan frontmatter listed 8 `files_modified`; the realized set is 12 because (a) `src/models/mod.rs` was implicitly required to register the new module, and (b) two test fixtures (`common.rs`, `test_auth.rs`) construct `SipServerInner` literals and had to gain the new `manipulation_engine` field for the workspace to compile. These are Rule 3 (blocking issue) auto-fixes.

## Migration Verification

`cargo test -p rustpbx --lib models::manipulations::` — 4/4 pass (in-memory SQLite migrator):
- `supersip_manipulations_migration_creates_table`
- `supersip_manipulations_name_is_unique`
- `direction_enum_parses_inbound_outbound`
- `rules_json_roundtrip`

`cargo test -p rustpbx --lib proxy::manipulation::` — 9/9 pass:
- new_engine_has_empty_caches
- manipulate_stub_returns_continue_with_empty_trace
- invalidate_class_removes_only_matching_prefix
- cleanup_session_removes_session_only
- rule_serde_round_trip_with_defaults
- rule_minimal_uses_defaults
- action_variants_serialize_with_snake_case_tag
- condition_mode_or_round_trips
- condition_op_snake_case_round_trip

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Test fixtures missing `manipulation_engine` field**
- **Found during:** Task 2 cargo check
- **Issue:** `src/proxy/tests/common.rs` and `src/proxy/tests/test_auth.rs` construct `SipServerInner { ... }` literals; adding the new field broke compilation in test profile.
- **Fix:** Added `manipulation_engine: Arc::new(crate::proxy::manipulation::ManipulationEngine::new())` to both fixtures.
- **Files modified:** `src/proxy/tests/common.rs`, `src/proxy/tests/test_auth.rs`
- **Commit:** `598e632`

**2. [Rule 3 - Blocking] `src/models/mod.rs` registration**
- **Found during:** Task 1
- **Issue:** Plan listed `src/models/migration.rs` but not `src/models/mod.rs`; the new module also needed `pub mod manipulations;` there.
- **Fix:** Registered alongside `pub mod translations;`.
- **Commit:** `185974d`

## Pre-existing Test Failures (Out of Scope)

The full lib suite reports 11 RTP/media e2e test failures under `proxy::tests::test_media_e2e`, `proxy::tests::test_rtp_e2e`, `proxy::tests::test_wholesale_e2e`. Verified at parent commit `185974d` (before any Task 2 work) that the same suite fails identically — these are Phase 8 baseline failures unrelated to manipulations work. Logged here for traceability; deferred per Scope Boundary rule (do NOT fix unrelated failures).

## Authentication Gates

None — all work autonomous.

## Acceptance Criteria

- [x] `src/models/manipulations.rs` exists with `supersip_manipulations` literal (306 lines).
- [x] `manipulations::Migration` registered in `src/models/migration.rs`.
- [x] `cargo check -p rustpbx --lib` exits 0.
- [x] `cargo test -p rustpbx --lib models::manipulations::` 4/4 pass.
- [x] `cargo test -p rustpbx --lib proxy::manipulation::` 9/9 pass.
- [x] `pub struct ManipulationEngine` defined; `manipulate`, `invalidate_class`, `cleanup_session` exposed.
- [x] All 9 wire types `pub` in `src/proxy/manipulation/engine.rs`.
- [x] `manipulation_engine` field in `SipServerInner` plus accessor on `AppStateInner` and `SipServer`.
- [x] `ManipulationEngine::new` invoked once in `src/proxy/server.rs`.
- [x] `manipulations::router` merged in `src/handler/api_v1/mod.rs`.
- [x] No ALTER/DROP statements introduced (forward-only migration).

## Self-Check: PASSED

Verified files exist:
- FOUND: src/models/manipulations.rs (306 lines)
- FOUND: src/proxy/manipulation/mod.rs
- FOUND: src/proxy/manipulation/engine.rs
- FOUND: src/handler/api_v1/manipulations.rs

Verified commits exist on `sip_fix`:
- FOUND: 185974d (Task 1)
- FOUND: 598e632 (Task 2)

Verified `cargo check -p rustpbx --lib` clean.
