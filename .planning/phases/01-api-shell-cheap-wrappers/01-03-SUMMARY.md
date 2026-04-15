---
plan: 01-03
phase: 01-api-shell-cheap-wrappers
completed_at: 2026-04-15
commits:
  - 65c5d82  # CDRs sub-router + 13 integration tests
status: gaps_found
requirements:
  - CDR-01
  - CDR-02
  - CDR-03
  - CDR-04
---

# Plan 01-03 — CDRs Sub-Router (3 live + 2 501-stub)

> **Retroactive reconciliation** — GSD state lagged behind git history; this
> SUMMARY reconstructs the execution record from commit `65c5d82` (+562 LOC).

## What was built

- `src/handler/api_v1/cdrs.rs` — new 189-line sub-router with:
  - `CdrView` wire type (`From<CallRecordModel>`).
  - `CdrListQuery` (pagination + direction/status/trunk/date filters).
  - 5 handlers: `list_cdrs`, `get_cdr`, `delete_cdr`,
    `cdr_recording_stub`, `cdr_sip_flow_stub`.
- `src/handler/api_v1/mod.rs` — `pub mod cdrs;` + `.merge(cdrs::router())`.
- `tests/api_v1_cdrs.rs` — 13 integration tests covering all 5 routes.

## Routes registered

| Method | Path | Handler |
|---|---|---|
| GET    | `/api/v1/cdrs`                     | `list_cdrs` (paginated + filters) |
| GET    | `/api/v1/cdrs/{id}`                | `get_cdr` (200/404) |
| DELETE | `/api/v1/cdrs/{id}`                | `delete_cdr` (204/404) |
| GET    | `/api/v1/cdrs/{id}/recording`      | `cdr_recording_stub` (501) |
| GET    | `/api/v1/cdrs/{id}/sip-flow`       | `cdr_sip_flow_stub` (501) |

Evidence: `src/handler/api_v1/cdrs.rs:93-98`.

## Verification results

```
cargo test --test api_v1_cdrs -> 13 passed / 0 failed
```

Both 501 stub handlers return the exact CONTEXT.md-locked bodies:
- `{"error":"recording retrieval not implemented","code":"not_implemented"}`
- `{"error":"sip flow retrieval not implemented","code":"not_implemented"}`

## Gaps found

1. **SHELL-05 partial — no `query_call_records / fetch_call_record /
   delete_call_record_row` in `console/handlers/call_record.rs`.** Same
   pattern as Plans 01-01/01-02: api_v1 handler goes directly to the
   SeaORM entity layer.

2. **MIG-03 spot-check on `/console/call-records` not documented.**
