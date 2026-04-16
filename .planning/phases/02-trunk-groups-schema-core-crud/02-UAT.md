---
status: complete
phase: 02-trunk-groups-schema-core-crud
source:
  - 02-01-SUMMARY.md
  - 02-02-SUMMARY.md
  - 02-03-SUMMARY.md
started: 2026-04-16T22:00:00Z
updated: 2026-04-16T22:15:00Z
---

## Current Test

[testing complete]

## Tests

### 1. Schema migrations compile and register
expected: `cargo check -p rustpbx` succeeds. Three new migrations registered in order (trunk_group -> trunk_group_member -> add_did_trunk_group_name_column). Zero ALTER/CREATE on `rustpbx_sip_trunks` in new files (MIG-01 compliance).
result: pass

### 2. List trunk groups (GET /api/v1/trunks)
expected: Returns paginated JSON with `items` array and `total` count. Empty DB returns `{"items":[],"total":0}`. After seeding a group with members, list returns the group with full `TrunkView` shape including nested `members` array.
result: pass

### 3. Get trunk group by name (GET /api/v1/trunks/{name})
expected: Returns full `TrunkView` JSON (name, display_name, direction, distribution_mode, members, credentials, acl, nofailover_sip_codes, is_active, created_at, updated_at). Returns 404 with `ApiError` body for unknown name.
result: pass

### 4. Create trunk group (POST /api/v1/trunks)
expected: Returns 201 with created `TrunkView`. Members stored with position derived from array order. Credentials and ACL round-trip losslessly through GET. Transaction rolls back on failure.
result: pass

### 5. Create validation rejects bad input (TRK-03)
expected: Returns 400 for: empty members, unknown gateway names, invalid name (regex `^[a-zA-Z0-9_-]{1,64}$`), trunk group name colliding with existing gateway, and `parallel` distribution mode without the `parallel-trunk-dial` feature.
result: pass

### 6. Update trunk group (PUT /api/v1/trunks/{name})
expected: Patches scalar fields (display_name, direction, distribution_mode). Atomically replaces all member rows within a transaction. Returns updated `TrunkView`.
result: pass

### 7. Delete trunk group with engagement blocking (TRK-04)
expected: Returns 204 on successful delete. Returns 409 when a DID references the trunk group via `trunk_group_name`. Returns 409 when a routing record's `target_trunks` JSON contains the group name. Unrelated routes do not block delete.
result: pass

### 8. Distribution mode dispatch (TRK-05)
expected: `round_robin` -> "rr", `weight_based` -> "weighted", `hash_callid` -> "hash"+"call-id", `hash_src_ip` -> "hash"+"from.user", `hash_destination` -> "hash"+"to.user". Hash modes produce deterministic gateway selection across repeated calls with same input. `parallel` without feature returns error.
result: pass

### 9. RoutingState DB threading
expected: `RoutingState::new_with_db(Some(db))` threads the database connection to the matcher. Production call site at `src/proxy/call.rs` uses `new_with_db(server.database.clone())`. 18 test sites + 1 diagnostics site unchanged (still use `new()`).
result: pass

### 10. Matcher-level trunk group integration
expected: `matcher_level_trunk_group_dispatch` test seeds a real SQLite DB with a trunk_group + 2 members, constructs RoutingState with DB, and verifies the full dispatch path selects a member gateway through `match_invite_with_trace`.
result: pass

### 11. Auth protection on all routes
expected: All 5 trunk endpoints (list, get, create, update, delete) return 401 without a valid Bearer token. Tests: `list_trunks_requires_auth`, `get_trunk_requires_auth`, `create_trunk_requires_auth`, `delete_trunk_requires_auth`.
result: pass

### 12. Phase 1 regression (zero breakage)
expected: All 78 Phase 1 tests pass unchanged. Full suite (Phase 1 + Phase 2) reaches 114 tests with 0 failures.
result: pass

## Summary

total: 12
passed: 12
issues: 0
pending: 0
skipped: 0
blocked: 0

## Gaps

[none]
