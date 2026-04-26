---
phase: 05-trunk-enforcement-capacity-acl-codec-filter
plan: 03
subsystem: handler/api_v1
tags: [tsub-05, acl, sub-resource, validation, trunk]
requirements: [TSUB-05]
dependency_graph:
  requires:
    - 05-01 (trunk_acl_entries table + module declaration + router merge stub)
  provides:
    - "GET /api/v1/trunks/{name}/acl"
    - "POST /api/v1/trunks/{name}/acl"
    - "DELETE /api/v1/trunks/{name}/acl/{rule}"
    - "pub fn validate_acl_rule(rule: &str) -> Result<(), String>"
  affects:
    - 05-04 (enforcement-time read of trunk_acl_entries; reuses validate_acl_rule)
tech_stack:
  added: []
  patterns:
    - "Sub-resource handler shape inherited from trunk_origination_uris.rs (multi-row + position auto-assign + DELETE-by-segment)"
    - "Stable `position` MAX+1 assignment, gaps acceptable on delete (mirrors D-06/D-12)"
    - "pub validator for cross-plan reuse (05-04 enforcement uses same parser)"
key_files:
  created:
    - tests/api_v1_trunk_acl.rs
  modified:
    - src/handler/api_v1/trunk_acl.rs
decisions:
  - "Used std::net::IpAddr parsing only — no new ipnet/cidr crate added (matches CONTEXT.md D-07 std-only convention)"
  - "Validator returns Result<(), String> rather than Result<(), ApiError> so the function is reusable from non-handler call sites in Plan 05-04 without an api_v1 dependency"
metrics:
  duration: "~6 min"
  tasks: 2
  files: 2
  tests_added: 14
  completed: 2026-04-25
---

# Phase 5 Plan 03: Trunk ACL CRUD Summary

GET/POST/DELETE handlers for `/api/v1/trunks/{name}/acl` plus a `pub` rule
validator (`validate_acl_rule`) reusable by Plan 05-04 enforcement.

## What changed

- `src/handler/api_v1/trunk_acl.rs` — replaced the Plan 05-01 6-line router
  stub with the full TSUB-05 implementation: wire types
  (`TrunkAclEntryView`, `AddTrunkAclEntryRequest`), `validate_acl_rule`
  (D-13 grammar `^(allow|deny) (all|<CIDR>|<IP>)$` via std `IpAddr`
  parsing), `lookup_trunk_group_id` helper, and three handlers (`list_acl`
  / `add_acl_entry` / `delete_acl_entry`). `pub fn router() -> Router<AppState>`
  signature preserved so the existing `.merge(trunk_acl::router())` call
  in `mod.rs` keeps compiling.
- `tests/api_v1_trunk_acl.rs` — 14 `#[tokio::test]` cases covering 401,
  list-empty, POST happy + position auto-increment, POST duplicate 409,
  POST invalid-action / invalid-CIDR / invalid-IP 400, POST `allow all`
  201, DELETE happy 204, DELETE-missing 404, parent-missing 404 across
  GET/POST/DELETE.

## Decisions made

- **D-13 implementation:** std `IpAddr` parser, not regex — same approach
  Plan 05-04 will reuse for runtime ACL evaluation.
- **Validator return shape:** `Result<(), String>` (not `Result<(), ApiError>`)
  so non-handler call sites can reuse it cleanly. The handler maps to
  `ApiError::bad_request` at the call site.
- **Position semantics:** MAX(position)+1 with first row at 0; gaps after
  delete are tolerated (mirrors trunk_origination_uris.rs D-06/D-07).
- **No new dep:** `ipnet` not added — std-only (CONTEXT.md D-07).

## Wave-2 file ownership invariant

`git diff --name-only` for this plan shows ONLY:
- `src/handler/api_v1/trunk_acl.rs`
- `tests/api_v1_trunk_acl.rs`

`src/handler/api_v1/mod.rs` was NOT touched — Plan 05-01 already declared
the module and merged the stub router, and Plan 05-02 owns mod.rs in this
wave.

## Verification

- `cargo check -p rustpbx --lib` — clean
- `cargo test -p rustpbx --test api_v1_trunk_acl` — 14/14 passed
- `git diff --name-only HEAD~2 HEAD` — only the two files above

## Acceptance-criteria check

- `pub fn router()` count: 1
- `pub fn validate_acl_rule` count: 1
- `pub struct TrunkAclEntryView` count: 1
- `pub struct AddTrunkAclEntryRequest` count: 1
- `order_by_asc` count: 1 (list ordering)
- `/trunks/{name}/acl` route count: 1
- 14/14 tests green
- `cargo check -p rustpbx --lib` exits 0
- `mod.rs` not in diff

## Threat-model coverage

All STRIDE entries from the plan's threat register are mitigated:

- T-05-03-01 (rule-string tampering) — `validate_acl_rule` rejects every
  form not matching D-13; `#[serde(deny_unknown_fields)]` on the request.
- T-05-03-02 (over-permissive rule) — closed grammar; default-allow per
  D-14 documented in the file-level doc-comment (operators must append
  `deny all` for default-deny).
- T-05-03-04 (DoS via duplicate POSTs) — UNIQUE (trunk_group_id, rule)
  pre-check + DB index.
- T-05-03-05 (DELETE silently passing) — strict 404 lookup before delete.
- T-05-03-06 (URL-decode mismatch) — round-trip POST→DELETE test
  asserts decoding parity through axum's Path extractor.

## Deviations from Plan

None — plan executed exactly as written.

## Commits

- `3f47848` — test(05-03): add failing integration tests for /trunks/{name}/acl (RED)
- `1ea7ee0` — feat(05-03): implement /trunks/{name}/acl GET/POST/DELETE (GREEN)

## Self-Check: PASSED

- FOUND: src/handler/api_v1/trunk_acl.rs
- FOUND: tests/api_v1_trunk_acl.rs
- FOUND commit: 3f47848
- FOUND commit: 1ea7ee0
- VERIFIED: src/handler/api_v1/mod.rs not in plan diff
