# Phase 7 Deferred Items

## Pre-existing baseline failures (not caused by Phase 7)

### tests/did_index.rs — missing `DidIndex::from_map_for_test`
- **File:** `tests/did_index.rs:13`
- **Issue:** Calls `DidIndex::from_map_for_test(map)` but no such associated item exists on `DidIndex`.
- **Discovered during:** 07-01 verification (`cargo build -p rustpbx --lib --tests`).
- **Status:** Pre-existing on parent commit `e0242fd` (and earlier — references a function that does not exist anywhere in the codebase).
- **Action:** Out of scope for Phase 7. Tracked here for a future cleanup pass.
