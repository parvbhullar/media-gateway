---
plan: 03-05
phase: 03-trunk-sub-resources-l1-routing-resolve
status: complete
commit: 324ff0a
tests: 7/7
---

# Plan 03-05 Summary — Routing Resolve Dry-Run (RTE-03)

## Routes Implemented

| Method | Path | Status | Description |
|--------|------|--------|-------------|
| POST | `/api/v1/routing/resolve` | 200 / 400 / 401 | Dry-run route resolution via match_invite_with_trace |

## Wire Types (D-14, D-15)

```rust
pub struct ResolveRouteRequest { caller_number, destination_number, src_ip?, headers? }
pub struct ResolveTarget        { kind: "trunk_group"|"gateway"|"queue"|"application", name }
pub struct ResolveRouteResponse { result, matched_table?, matched_record_index?, match_reason?, target?, selected_gateway?, trace[] }
```

## Dispatch Reuse Path (D-13)

Handler calls `match_invite_with_trace` directly — same function production dispatch uses. By construction, dry-run cannot drift from real routing.

Config sources:
- Routes: `state.sip_server().inner.data_context.routes_snapshot()` (live in-memory snapshot)
- Trunks: `state.sip_server().inner.data_context.trunks_snapshot()` (live in-memory snapshot)
- `RoutingState::new_with_db(Some(db.clone()))` per D-17

## RouteResult → Response Mapping (D-15)

| RouteResult variant | response.result | target.kind | selected_gateway |
|---------------------|-----------------|-------------|------------------|
| Forward + trunk_group | "matched" | "trunk_group" | resolved gateway name |
| Forward + gateway | "matched" | "gateway" | null |
| NotHandled | "not_handled" | null | null |
| Abort | "abort" | null | null (match_reason = abort reason) |
| Queue | "matched" | "queue" | null |
| Application | "matched" | "application" | null |

## Trace Serialization

Added `#[derive(serde::Serialize)]` to `RouteTrace` and `RouteAbortTrace` in `src/proxy/routing/matcher.rs` (Task 1 — additive, no existing call sites affected).

Added `trunk_group_name: Option<String>` field to `RouteTrace` — set when `try_select_via_trunk_group` succeeds, holds the trunk_group name while `selected_trunk` holds the resolved gateway. Both Forward call sites in the matcher updated.

Trace serialized as single-element `Vec<serde_json::Value>` for forward-compat with multi-event traces.

## Test Inventory (7 tests)

| # | Test | Asserts |
|---|------|---------|
| 1 | `resolve_requires_auth` | 401 without Bearer (D-16) |
| 2 | `resolve_unknown_destination_returns_not_handled` | 200, result:"not_handled", target:null |
| 3 | `resolve_with_trunk_group_target_returns_selected_gateway` | matched, kind:"trunk_group", selected_gateway in {gw-alpha,gw-beta} |
| 4 | `resolve_with_gateway_target_returns_no_selected_gateway` | matched, kind:"gateway", selected_gateway:null |
| 5 | `resolve_with_reject_action_returns_abort` | result:"abort", match_reason contains "blocked" |
| 6 | `resolve_invalid_body_returns_400` | 400 for malformed JSON |
| 7 | `resolve_response_includes_trace` | trace is non-empty array with matched_rule field |

Routes injected via `data_context.reload_routes(false, Some(config_with_embedded_routes))` — same live snapshot path the handler reads.

## Final Regression

| Suite | Tests | Status |
|-------|-------|--------|
| api_v1_routing_resolve (03-05) | 7 | ok |
| api_v1_trunk_media (03-04) | 9 | ok |
| api_v1_trunk_origination_uris (03-03) | 9 | ok |
| api_v1_trunk_credentials (03-02) | 8 | ok |
| api_v1_trunks (Phase 2 baseline) | 23 | ok |
| trunk_group_dispatch (Phase 2 routing) | 13 | ok |
| **Total** | **69** | **0 failed** |

Pre-existing `did_index` test has an unrelated compile error (`from_map_for_test` missing) that pre-dates Phase 3 — not introduced by this work.

## Hand-off Note for Phase 6

When `/routing/tables` CRUD ships (RTE-01..05), populate `matched_table` and `matched_record_index` in `ResolveRouteResponse` (currently always `None`). The wire type already has these fields — Phase 6 just needs to fill them from the matched `rustpbx_routes` row.
