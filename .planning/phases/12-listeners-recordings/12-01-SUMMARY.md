---
phase: 12-listeners-recordings
plan: "01"
subsystem: api-v1-listeners
tags: [listeners, recordings-stub, api-v1, phase-12]
dependency_graph:
  requires: []
  provides: [LSTN-01, LSTN-02, LSTN-03, LSTN-04, ApiError::gone, pub(super)-build_cdr_filter, async_zip-dep, recordings-stub-router]
  affects: [src/handler/api_v1/mod.rs, src/handler/api_v1/error.rs, src/handler/api_v1/cdrs.rs, Cargo.toml]
tech_stack:
  added: [async_zip 0.0.17 (tokio+deflate features)]
  patterns: [read-only projection over config, locked-501 write stubs, Bearer-gated API, pure-fn projection testable without AppState]
key_files:
  created:
    - src/handler/api_v1/listeners.rs
    - src/handler/api_v1/recordings.rs
    - tests/api_v1_listeners.rs
  modified:
    - src/handler/api_v1/error.rs
    - src/handler/api_v1/cdrs.rs
    - src/handler/api_v1/mod.rs
    - Cargo.toml
    - tests/common/mod.rs
decisions:
  - "async_zip 0.0.17 with tokio+deflate features resolved cleanly — no version fallback needed"
  - "external_ip field omitted from Listener struct per planner D-04 (RtpConfig.external_ip is unrelated to SIP transport bind)"
  - "Disabled port encodes as port=0, enabled=false (not defaulted) per planner D-03"
  - "test_state_with_config_mut helper added to common/mod.rs to enable disabled-port fixture override"
  - "routing imports trimmed from delete/post/put to get-only in listeners.rs — Axum MethodRouter chaining does not need those imports"
metrics:
  duration: "~2h"
  completed: "2026-05-04T16:32:00Z"
  tasks_completed: 3
  files_changed: 8
---

# Phase 12 Plan 01: Listeners + Shared Widenings Summary

**One-liner:** Full LSTN-01..04 read-only `/api/v1/listeners` projection over ProxyConfig transports with locked 501 write stubs, plus `ApiError::gone()`, `pub(super) build_cdr_filter`, and `async_zip 0.0.17` dep ready for Wave 2 parallel plans.

## Tasks Completed

| Task | Name | Commit | Key Files |
|------|------|--------|-----------|
| 1 | Add ApiError::gone(), widen build_cdr_filter, add async_zip dep | eb57692 | error.rs, cdrs.rs, Cargo.toml |
| 2 | Implement listeners.rs (LSTN-01..04) + stub recordings.rs + register both routers | 31d6529 | listeners.rs, recordings.rs, mod.rs |
| 3 | Integration tests for /api/v1/listeners (auth + happy + 501 + 404) | 71bbf02 | tests/api_v1_listeners.rs, tests/common/mod.rs |

## What Was Built

### listeners.rs
- `GET /api/v1/listeners` — always returns exactly 4 entries in udp/tcp/tls/ws order, projected from `ProxyConfig`
- `GET /api/v1/listeners/{name}` — returns single entry or 404 with `code=not_found`
- `POST/PUT/DELETE` on any listeners path — returns 501 with locked D-05 message and `code=not_implemented`
- `build_listeners(&ProxyConfig)` is a pure function (testable without AppState)
- Disabled ports (when `{protocol}_port` is `None`) encode as `enabled: false, port: 0`
- No `external_ip` field — ProxyConfig has no such field (planner D-04 resolution)

### recordings.rs
- Empty stub router: `pub fn router() -> Router<AppState> { Router::new() }`
- Mounted in mod.rs so 12-02 and 12-03 only add handlers, never re-touch mod.rs

### Shared infrastructure
- `ApiError::gone(msg)` — returns 410 GONE with `code: "recording_missing"` (for 12-02 download)
- `pub(super) fn build_cdr_filter` — widened from private so `recordings.rs` siblings can import it (for 12-02/12-03)
- `async_zip = { version = "0.0.17", features = ["tokio", "deflate"] }` — added to Cargo.toml (for 12-03 ZIP export)

## Test Results

### Unit tests (lib)
```
handler::api_v1::error::gone_tests::gone_constructs_410_with_recording_missing_code ... ok
handler::api_v1::listeners::tests::bind_addr_copied_from_proxy_addr ... ok
handler::api_v1::listeners::tests::build_listeners_emits_four_entries_in_fixed_order ... ok
handler::api_v1::listeners::tests::disabled_port_marks_enabled_false_and_port_zero ... ok
```

### Integration tests (api_v1_listeners)
```
test list_requires_auth ... ok
test list_returns_four_entries ... ok
test get_returns_listener_by_name ... ok
test get_unknown_returns_404 ... ok
test post_returns_501 ... ok
test put_returns_501 ... ok
test delete_returns_501 ... ok
test disabled_port_marks_enabled_false ... ok

test result: ok. 8 passed; 0 failed
```

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Removed unused routing imports in listeners.rs**
- **Found during:** Task 2 build
- **Issue:** `routing::{delete, get, post, put}` — `delete`, `post`, `put` were imported but Axum MethodRouter chaining (`.post(handler)` etc.) does not require those imports. Rustc emitted an unused-import warning.
- **Fix:** Trimmed import to `routing::get` only; the `.post()`, `.put()`, `.delete()` method chains on `MethodRouter` don't need the routing constructors as imports.
- **Files modified:** `src/handler/api_v1/listeners.rs`
- **Result:** Zero warnings, zero errors

**2. [Rule 2 - Missing functionality] Added test_state_with_config_mut fixture helper**
- **Found during:** Task 3 — the plan's `disabled_port_marks_enabled_false` test required configuring `tls_port=None` at fixture time. No such config-override helper existed in `tests/common/mod.rs`.
- **Fix:** Added `test_state_with_config_mut(name, FnOnce(&mut Config))` to `tests/common/mod.rs`, matching the pattern of `test_state_with_recorder`. The helper applies a closure to the test config before building the AppState.
- **Files modified:** `tests/common/mod.rs`

## async_zip Version Notes

`async_zip 0.0.17` with `features = ["tokio", "deflate"]` resolved cleanly from crates.io without any fallback. No version change was needed. This version exposes `async_zip::tokio::write::ZipFileWriter` which is the API consumed by Plan 12-03.

## external_ip Omission Confirmation

The `Listener` struct intentionally has no `external_ip` field. `ProxyConfig` (lines 731–812 of `src/config.rs`) has no `external_ip` field. `RtpConfig.external_ip` is an unrelated RTP/media concern. This is planner-resolved decision D-04.

## D-05 Message Verbatim Verification

The locked message in `listeners.rs` is:
```
"Multi-listener configuration is intentionally unsupported in v2.0. Edit ProxyConfig and POST /api/v1/system/reload to change transports."
```
Zero deviations from the plan-specified wording. The integration test `post_returns_501` asserts `.contains("Multi-listener configuration")` to verify the message is present.

## Test Fixture Helpers Used

Pattern matches `tests/api_v1_system.rs` exactly:
- `test_state_empty()` — no-auth tests (list_requires_auth)
- `test_state_with_api_key(name)` — returns `(AppState, token)` for happy-path tests
- `test_state_with_config_mut(name, closure)` — new helper for config-override tests (disabled_port_marks_enabled_false)
- `rustpbx::app::create_router(state)` — builds the full router
- `app.oneshot(Request::builder()...)` — in-process request dispatch

## Threat Surface Scan

No new network endpoints, auth paths, or file access patterns beyond those documented in the plan's `<threat_model>`. All listeners routes inherit Bearer middleware via `.merge()` inside the `protected` sub-router (verified in mod.rs lines 64–65). No `state.config_mut()` calls exist in `listeners.rs` (T-12-01-03 mitigated).

## Self-Check: PASSED

Files exist:
- src/handler/api_v1/listeners.rs: FOUND
- src/handler/api_v1/recordings.rs: FOUND
- tests/api_v1_listeners.rs: FOUND

Commits exist:
- eb57692 (Task 1): FOUND
- 31d6529 (Task 2): FOUND
- 71bbf02 (Task 3): FOUND

All acceptance criteria met. 8/8 integration tests green. 4/4 unit tests green. cargo build --bin rustpbx clean (zero warnings, zero errors).
