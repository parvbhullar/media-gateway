# Testing

## Running Tests

```bash
# Full test suite
cargo test

# Specific test
cargo test test_name

# With output
cargo test -- --nocapture

# Integration tests only (requires the feature flag)
cargo test --features integration-test
```

## Test Organization

- `src/*/tests/` — Module-level unit tests (inline `#[cfg(test)]` modules)
- `tests/` — Top-level integration tests
- `src/fixtures.rs` — Seed data for demo mode and test scenarios

### Integration Test Files

The `tests/` directory contains integration tests grouped by API resource:

| File | Coverage |
|------|----------|
| `tests/api_v1_gateways.rs` | Gateway CRUD |
| `tests/api_v1_dids.rs` | DID CRUD |
| `tests/api_v1_cdrs.rs` | CDR list/search |
| `tests/api_v1_trunks.rs` | Trunk group CRUD |
| `tests/api_v1_auth.rs` | Authentication endpoints |
| `tests/api_v1_diagnostics.rs` | Route evaluate, trunk probe |
| `tests/api_v1_system.rs` | Health, reload |
| `tests/api_v1_error_shape.rs` | Error envelope consistency |
| `tests/api_v1_middleware.rs` | Auth middleware behavior |
| `tests/api_v1_mount.rs` | Sub-router mounting |
| `tests/rwi_*.rs` | RWI WebSocket protocol tests |
| `tests/gateway_health_*.rs` | Health-check probing |

## Integration Test Conventions

From Phase 1 (IT-01), every API sub-router has integration tests covering:

1. **401 without auth** — Request without Bearer token returns 401
2. **Happy path** — Valid request returns expected response
3. **404 missing** — Request for non-existent resource returns 404
4. **400/409 bad input** — Invalid data returns 400, conflicting state returns 409

## Adding Tests for a New API Route

1. Create test in `tests/api_v1_<group>.rs`
2. Use the test fixture pattern from existing tests (see `tests/helpers/` for shared utilities)
3. Cover all 4 integration test cases above
4. Run `cargo test` to verify

## Unit Tests

Inline unit tests live in `#[cfg(test)] mod tests` blocks within the source file. See `src/handler/api_v1/common.rs` for an example covering pagination defaults, offset calculation, limit clamping, and response serialization shape.
