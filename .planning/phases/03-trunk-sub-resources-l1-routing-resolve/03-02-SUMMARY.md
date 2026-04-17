---
phase: 03-trunk-sub-resources-l1-routing-resolve
plan: 02
completed_at: 2026-04-17
status: complete
closes_requirements:
  - TSUB-01
commits:
  - hash: f199e05
    message: "feat(03-02): Task 1 — trunk_credentials handlers (list/add/delete + validation)"
  - hash: d7ffc84
    message: "test(03-02): Task 2 — trunk_credentials integration tests"
key-files:
  created:
    - tests/api_v1_trunk_credentials.rs
  modified:
    - src/handler/api_v1/trunk_credentials.rs
duration_seconds: 900
tasks_completed: 2
tasks_total: 2
dependency_graph:
  requires:
    - Plan 03-01 schema (supersip_trunk_credentials table, UNIQUE (trunk_group_id, realm))
    - Plan 03-01 stub mount (router wired into /api/v1 protected namespace in mod.rs)
    - Phase 2 trunk_group entity + Bearer auth middleware
  provides:
    - "/api/v1/trunks/{name}/credentials GET/POST fully implemented"
    - "/api/v1/trunks/{name}/credentials/{realm} DELETE fully implemented"
    - "Wire types: TrunkCredentialView + AddTrunkCredentialRequest (exported from module)"
    - "Reference sub-resource handler shape for Plans 03-03 (origination_uris) and 03-04 (media)"
  affects:
    - Closes TSUB-01 (operator can CRUD credentials per trunk group)
    - Restores credentials round-trip coverage lost in Plan 03-01 Phase 2 test split
tech-stack:
  patterns:
    - "Parent-lookup -> child-query -> view-projection -> 4xx-mapping (sub-resource template)"
    - "Pre-check UNIQUE for friendly 409; DB constraint as safety net"
    - "Strict 404-on-miss for DELETE (D-04 consistency with Phase 2 /trunks)"
    - "Path extractor auto-URL-decode for {realm} segment"
key-decisions:
  - "D-01 LOCKED: UNIQUE (trunk_group_id, realm) enforced by DB + handler pre-check"
  - "D-03 LOCKED: plaintext password retained for Phase 3 (v2.1 hardening milestone owns encryption)"
  - "D-04 LOCKED: DELETE-by-realm returns 404 on miss (strict, not idempotent)"
  - "D-05 LOCKED: realm is 1-255 chars, no '/' (router-path conflict guard)"
  - "validate_realm is a local helper — same-file co-location pattern matches trunks.rs"
---

# Phase 3 Plan 03-02: Trunk Credentials Full Handlers Summary

Replaced the three Plan 03-01 501 stubs in `src/handler/api_v1/trunk_credentials.rs` with full handlers backed by `supersip_trunk_credentials`, plus shipped the 8-test IT-01 matrix in `tests/api_v1_trunk_credentials.rs`. TSUB-01 is fully closed. The file is now the reference template for Plans 03-03 and 03-04 (origination_uris, media).

## Routes Implemented

| Method | Route | Returns | 4xx surface |
|---|---|---|---|
| GET | `/api/v1/trunks/{name}/credentials` | `200` + `[TrunkCredentialView]` (ordered by created_at ASC, `[]` when empty) | 401 no-auth; 404 parent missing |
| POST | `/api/v1/trunks/{name}/credentials` | `201` + `TrunkCredentialView` | 400 invalid realm/username/password; 401 no-auth; 404 parent missing; 409 duplicate realm |
| DELETE | `/api/v1/trunks/{name}/credentials/{realm}` | `204` (no body) | 401 no-auth; 404 parent missing OR realm not found (D-04 strict) |

## Wire Types

```rust
#[derive(Debug, Serialize)]
pub struct TrunkCredentialView {
    pub realm: String,
    pub username: String,
    pub password: String,   // TODO(v2.1): strip from GET once rotation API lands
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddTrunkCredentialRequest {
    pub realm: String,
    pub username: String,
    pub password: String,
}
```

`TcModel -> TrunkCredentialView` via `From`, preserving SHELL-04 (SeaORM Model is never serialized directly).

## Validation Rules Applied

| Field | Rule | Source decision |
|---|---|---|
| realm | trimmed non-empty, ≤ 255 chars | D-05 |
| realm | must not contain `/` | D-05 (router conflict with DELETE path) |
| username | trimmed non-empty | local rule (parity with gateways.rs) |
| password | non-empty (not trimmed — passwords may have leading/trailing whitespace) | local rule |
| parent trunk existence | looked up via `lookup_trunk_group_id` before any sub-resource I/O | consistent 404 contract across Phase 3 sub-resources |
| duplicate `(trunk_group_id, realm)` | pre-check (handler) + UNIQUE index (DB safety net) | D-01 |

## Test Inventory (IT-01)

| # | Test | Asserts |
|---|---|---|
| 1 | `list_credentials_requires_auth` | GET without Bearer -> 401 |
| 2 | `list_credentials_empty_returns_empty_array` | GET on parent-with-zero-credentials -> 200 + `[]` |
| 3 | `add_credential_happy_returns_201_and_round_trips_via_get` | POST -> 201 + body matches input; follow-up GET lists it |
| 4 | `add_credential_duplicate_realm_returns_409` | Two POSTs, same realm -> 201 then 409 with `code: "conflict"` and realm in error |
| 5 | `delete_credential_happy_returns_204` | POST then DELETE `/credentials/to-delete.example.com` -> 204; follow-up GET -> 200 + `[]` |
| 6 | `delete_credential_missing_realm_returns_404` | DELETE unknown realm on existing parent -> 404 + `code: "not_found"` + realm in error (D-04 strict) |
| 7 | `list_credentials_parent_missing_returns_404` | GET on non-existent trunk name -> 404 + `code: "not_found"` + trunk name in error |
| 8 | `add_credential_invalid_realm_returns_400` | POST `"realm": "has/slash"` -> 400 + `code: "bad_request"` + error mentions `/` (D-05) |

All 8 pass in 0.56s in a single `cargo test -p rustpbx --test api_v1_trunk_credentials` run.

## Regression Result

Full Phase 2 + Plan 03-01 baseline verified with scoped `--test <name>` sweep:

```
cargo test -p rustpbx --test api_v1_auth --test api_v1_mount --test api_v1_error_shape \
  --test api_v1_middleware --test api_v1_dids --test api_v1_gateways --test api_v1_cdrs \
  --test api_v1_diagnostics --test api_v1_system --test api_v1_trunks \
  --test trunk_group_dispatch --test api_v1_trunk_credentials
```

| Test binary | Passed | Failed |
|---|---:|---:|
| api_v1_auth | 2 | 0 |
| api_v1_cdrs | 13 | 0 |
| api_v1_dids | 12 | 0 |
| api_v1_diagnostics | 20 | 0 |
| api_v1_error_shape | 1 | 0 |
| api_v1_gateways | 19 | 0 |
| api_v1_middleware | 3 | 0 |
| api_v1_mount | 1 | 0 |
| api_v1_system | 7 | 0 |
| api_v1_trunk_credentials | **8 (new)** | 0 |
| api_v1_trunks | 23 | 0 |
| trunk_group_dispatch | 13 | 0 |
| **Total** | **122** | **0** |

Phase 2 baseline of 114 tests preserved (23 in `api_v1_trunks` + 91 across the other pre-existing binaries = 114). +8 new TSUB-01 tests = 122 total with zero regressions.

## Verification Results

| Check | Result |
|---|---|
| `cargo check -p rustpbx --lib` | Clean, zero errors, Finished dev in 26.96s |
| `cargo test -p rustpbx --test api_v1_trunk_credentials` | 8 passed; 0 failed |
| `cargo test -p rustpbx --test api_v1_trunks` (Phase 2 baseline) | 23 passed; 0 failed |
| `grep -c 'not_implemented' src/handler/api_v1/trunk_credentials.rs` | 0 (all stubs gone) |
| `grep '#\[tokio::test\]' tests/api_v1_trunk_credentials.rs \| wc -l` | 8 |
| File length `wc -l src/handler/api_v1/trunk_credentials.rs` | 230 lines (≥ 180 minimum) |
| Grep for `pub struct TrunkCredentialView` | Present (line 40) |
| Grep for `pub struct AddTrunkCredentialRequest` | Present (line 60) |
| Grep for `fn validate_realm` | Present (line 70) |
| Grep for `lookup_trunk_group_id` | Present at 3 call sites (list, add, delete) |
| Grep for `StatusCode::CREATED` / `StatusCode::NO_CONTENT` | Present |
| Grep for `ApiError::conflict` | Present (line 174) |

## Deviations from Plan

None. Plan executed exactly as written. Both tasks converged on first implementation:
- Task 1 `cargo check --lib` passed cleanly after the first write — column names on `trunk_credentials::Column` matched the plan's assumptions (`TrunkGroupId`, `Realm`, `AuthUsername`, `AuthPassword`, `CreatedAt`).
- Task 2 all 8 tests passed on first run — handler pre-check for duplicate realm produced the expected 409; `axum` `Path<(String, String)>` extracted `{name, realm}` with the dot in `to-delete.example.com` intact (no slash, so no router conflict); `deny_unknown_fields` on `AddTrunkCredentialRequest` is available for future stricter rejection cases (not exercised in this plan's 8 tests).

## Threat Flags

None. All handlers operate within the Plan 03-02 `<threat_model>` surface:

- T-03-CRED-01 (plaintext password leak on GET) — accepted, documented in source with `TODO(v2.1)` markers at both the wire-type declaration and the module header.
- T-03-CRED-02 (realm tampering) — mitigated by `validate_realm` (Test 8 asserts the slash rejection; DB VARCHAR(255) safety-nets length).
- T-03-CRED-03 (duplicate-realm race) — pre-check + DB UNIQUE index (safety net for concurrent writes, rare).
- T-03-CRED-04 (unauthenticated access) — Bearer middleware inherited; Test 1 asserts 401.
- T-03-CRED-05 (parent-exists vs realm-missing disclosure) — accepted per plan.

No new security surface introduced beyond the registered threats.

## Hand-off Notes for Plans 03-03..03-05

1. **Reference handler template.** `src/handler/api_v1/trunk_credentials.rs` is the canonical sub-resource shape — mirror its structure in:
   - Plan 03-03 (`trunk_origination_uris.rs`): parent lookup -> child query ordered by `position` -> view projection; POST assigns next `position`; DELETE-by-uri strict 404.
   - Plan 03-04 (`trunk_media.rs`): parent lookup -> read/update `rustpbx_trunk_groups.media_config` JSON; GET returns `{codecs: [], dtmf_mode: null, srtp: null, media_mode: null}` on NULL per D-11.

2. **`lookup_trunk_group_id` is a local helper.** It is NOT exported. Plans 03-03 and 03-04 should inline the same 5-line helper in their own files (follows same-file validation-helper convention from trunks.rs). If a third sub-resource needs it, consider hoisting into a shared module — DRY once, not twice.

3. **`..Default::default()` on `ActiveModel` confirmed working.** Plan 03-01's hand-off note was accurate; the `id` column is `auto_increment = true` and the sea_orm derive fills the default cleanly.

4. **URL-decoded path segments are safe for typical realms.** `to-delete.example.com` (a FQDN-style realm with dots) routed through the `{realm}` path param without any URL-encoding in the test client. If Plan 03-03 uses URIs like `sip:alice@example.com:5060;transport=udp`, the `;` character (unreserved in percent-encoding but reserved in path-param URIs per RFC 3986) may require encoding by the operator — document in the OpenAPI note.

5. **Pre-check pattern is load-bearing for the 409 message.** Axum's default serialization of a SeaORM UNIQUE-violation error is unfriendly. Every sub-resource that has a UNIQUE should pre-check and emit `ApiError::conflict` with a message naming the conflicting value — Test 4 asserts the realm is named in the error body.

6. **Phase 2 credentials test split is now fully replaced.** The inline round-trip assertion in `create_trunk_persists_credentials_acl_nofailover` (Phase 2, removed in Plan 03-01) is now covered at the sub-resource layer by Test 3 (`add_credential_happy_returns_201_and_round_trips_via_get`). The acceptance criteria for the D-02 test split are satisfied.

7. **`api_v1_trunk_credentials` test binary has zero reliance on `tests/common` helpers beyond `test_state_empty` / `test_state_with_api_key`.** Plans 03-03 and 03-04 can copy the `insert_trunk` / `insert_trunk_group` / `body_json` helpers inline without touching `common/mod.rs`, preserving the same per-binary isolation.

## Self-Check: PASSED

- `src/handler/api_v1/trunk_credentials.rs`: FOUND (230 lines, zero stubs)
- `tests/api_v1_trunk_credentials.rs`: FOUND (483 lines, 8 `#[tokio::test]` functions)
- commit `f199e05` (Task 1 handlers): FOUND
- commit `d7ffc84` (Task 2 tests): FOUND
- `cargo check -p rustpbx --lib`: passed
- `cargo test -p rustpbx --test api_v1_trunk_credentials`: 8 passed
- `cargo test -p rustpbx --test api_v1_trunks`: 23 passed (Phase 2 baseline preserved)
- Full regression sweep across 12 test binaries: 122 passed, 0 failed

## EXECUTION COMPLETE

SUMMARY: `/Users/parvbhullar/Drives/Vault/Projects/Unpod/super-voice/media-gateway/.planning/phases/03-trunk-sub-resources-l1-routing-resolve/03-02-SUMMARY.md`
