---
plan: 01-01
phase: 01-api-shell-cheap-wrappers
completed_at: 2026-04-15
commits:
  - 6f24907  # Task 1 scaffolding (error + common + mod)
  - 8db5954  # Task 2 DIDs sub-router + 20 integration tests
status: gaps_found
requirements:
  - SHELL-01
  - SHELL-02
  - SHELL-03
  - SHELL-04
  - SHELL-05
  - DID-01
  - DID-02
  - DID-03
  - DID-04
  - IT-01
  - MIG-03
---

# Plan 01-01 — Scaffolding + DIDs Sub-Router

> **Retroactive reconciliation** — GSD state lagged behind git history; this
> SUMMARY reconstructs the execution record from commits `6f24907` and
> `8db5954` (author: Parvinder, 2026-04-15). No SUMMARY existed at the time
> each commit landed.

## What was built

**Scaffolding (commit `6f24907`, +141 LOC)**
- `src/handler/api_v1/error.rs` — added `ApiError::not_implemented` helper
  (501 + `code: "not_implemented"`).
- `src/handler/api_v1/common.rs` — new module with `Pagination` query
  extractor (`page` default 1, `page_size` default 20, `offset()` math) and
  `PaginatedResponse<T>` envelope shaped as `{items, page, page_size, total}`.
  5 inline unit tests cover defaults, offset, clamps, and envelope shape.
- `src/handler/api_v1/mod.rs` — declared `pub mod common;`.
- Root `Cargo.toml` — added empty `[workspace]` so `media-gateway/` is a
  standalone workspace root (parent repo was claiming it).

**DIDs sub-router (commit `8db5954`, +864 LOC)**
- `src/handler/api_v1/dids.rs` — new 350-line sub-router with:
  - `DidView` wire type (never serializes `DidModel` directly — SHELL-04).
  - `DidListQuery` (pagination + `trunk`, `mode` filters).
  - `CreateDidRequest`, `UpdateDidRequest` input types.
  - 5 handlers: `list_dids`, `create_did`, `get_did`, `update_did`, `delete_did`.
  - `pub fn router()` merged in `api_v1/mod.rs`.
- `src/handler/api_v1/mod.rs` — appended `pub mod dids;` + `.merge(dids::router())`.
- `tests/api_v1_dids.rs` — 20 integration tests (4+ per route, 401/happy/404/400).

## Routes registered

| Method | Path | Handler |
|---|---|---|
| GET    | `/api/v1/dids`              | `list_dids`   (paginated + filters) |
| POST   | `/api/v1/dids`              | `create_did`  (201 on success, 409 dup) |
| GET    | `/api/v1/dids/{number}`     | `get_did`     (200/404) |
| PUT    | `/api/v1/dids/{number}`     | `update_did`  (200/404) |
| DELETE | `/api/v1/dids/{number}`     | `delete_did`  (204/404) |

Evidence: `src/handler/api_v1/dids.rs:160-167`.

## Verification results

```
cargo test --test api_v1_dids         -> 20 passed / 0 failed
cargo test --test api_v1_error_shape  ->  1 passed / 0 failed
```

## Gaps found

1. **SHELL-05 partial — adapter pattern not via console handler extraction.**
   The plan promised `pub(crate) async fn query_dids / fetch_did /
   create_did_row / update_did_row / delete_did_row` in
   `src/console/handlers/did.rs`. None of these exist (`grep` confirms zero
   matches). The dids.rs module docstring at
   `src/handler/api_v1/dids.rs:1-12` documents the deliberate deviation:
   "no console handler extraction is needed because the model layer is
   already the shared sink between HTML and JSON handlers" — i.e. both
   paths call `models::did::Model` methods directly. The *intent* of
   SHELL-05 (no duplicate SQL, shared sink) is satisfied by the model layer
   instead, but the literal truth "pure fns exist in `console/handlers/did.rs`"
   is **NOT met**. Recorded as partial, not missing, because the underlying
   invariant holds.

2. **MIG-03 spot-check not performed.** Commit message for `8db5954`
   states: "console DID pages not spot-checked". No template touched and no
   console code touched, so render-parity risk is zero, but the explicit
   manual checkpoint was skipped.
