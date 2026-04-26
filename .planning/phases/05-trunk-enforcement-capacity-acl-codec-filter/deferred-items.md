# Deferred items — Phase 5 Plan 05-04

## Pre-existing issues (out of scope, unrelated to Phase 5)

### `tests/did_index.rs:13` — `DidIndex::from_map_for_test` missing

Compile error in `tests/did_index.rs` calls `DidIndex::from_map_for_test(...)` which
no longer exists in the `DidIndex` API. Pre-existed before Plan 05-04 work began.
Out of scope for this plan; tracked here for visibility.

## Plan 05-04 follow-ups (non-blocking)

### Wire-level `Retry-After:` SIP header injection

`RouteResult::Reject { retry_after_secs: Some(_), .. }` carries the value but the
session-dispatch path currently surfaces only `(StatusCode, Option<String>)` to the
SIP response builder. Adding a wire-level `Retry-After: <n>` header on 503 responses
requires extending `RouteError` (or its consumer) to carry an optional headers map.
Tests assert `RouteResult::Reject { retry_after_secs: Some(5), .. }` directly which
proves the gate behavior is correct; only the wire-level surface is deferred.

### Per-trunk ACL caching (D-17 future hardening)

D-17 explicitly chose fresh per-INVITE DB reads of ACL rules for v2.0 correctness.
A v2.1 caching layer is a noted deferred candidate.
