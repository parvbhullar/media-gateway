---
plan: 03-04
phase: 03-trunk-sub-resources-l1-routing-resolve
status: complete
commit: 1012f58
tests: 9/9
---

# Plan 03-04 Summary — Trunk Media GET/PUT (TSUB-03)

## Routes Implemented

| Method | Path | Status | Description |
|--------|------|--------|-------------|
| GET | `/api/v1/trunks/{name}/media` | 200 / 404 | Read media_config; returns defaults if NULL (D-11) |
| PUT | `/api/v1/trunks/{name}/media` | 200 / 400 / 404 | Replace media_config atomically; always stores Some(json) |

## Wire Type (D-09)

```rust
pub struct TrunkMediaConfig {
    pub codecs: Vec<String>,          // default []
    pub dtmf_mode: Option<String>,    // default null
    pub srtp: Option<String>,         // default null
    pub media_mode: Option<String>,   // default null
}
```

`deny_unknown_fields` catches operator typos.

## Validation Rules

| Rule | Behavior |
|------|----------|
| D-10: codecs lowercase | `validate_codec` rejects any uppercase char; returns 400 with "lowercase" in error |
| D-12: dtmf_mode enum | `{rfc2833, info, inband}` or null; other values → 400 naming the field |
| D-12: srtp enum | `{srtp, srtp_optional}` or null |
| D-12: media_mode enum | `{relay, transcode}` or null |

## D-11 Storage Behavior

- **GET on NULL column**: returns `{codecs:[], dtmf_mode:null, srtp:null, media_mode:null}` — never 404
- **PUT with all-null enums**: stores `Some(serde_json::Value::Object{...})`, NOT `None` — keeps schema observable for Phase 5
- Verified by test 9: direct DB column inspection asserts `row.media_config.is_some()`

## Test Inventory (9 tests)

| # | Test | Asserts |
|---|------|---------|
| 1 | `get_media_requires_auth` | 401 without token |
| 2 | `get_media_returns_defaults_when_column_null` | 200, `codecs:[]`, all nulls (D-11) |
| 3 | `put_media_happy_round_trips_full_config` | 200 PUT echo + GET round-trip |
| 4 | `put_media_invalid_codec_uppercase_returns_400` | 400, `code:bad_request`, "lowercase" in error (D-10) |
| 5 | `put_media_invalid_dtmf_mode_returns_400` | 400, "dtmf_mode" in error |
| 6 | `put_media_invalid_srtp_returns_400` | 400, "srtp" in error |
| 7 | `put_media_invalid_media_mode_returns_400` | 400, "media_mode" in error |
| 8 | `get_media_parent_missing_returns_404` | 404, `code:not_found` |
| 9 | `put_media_with_all_nulls_stores_some_not_null` | DB column `is_some()` after all-null PUT (D-11) |

## Cross-Plan Regression

| Suite | Result |
|-------|--------|
| `api_v1_trunk_media` (this plan) | 9/9 ok |
| `api_v1_trunk_credentials` (03-02) | 8/8 ok |
| `api_v1_trunk_origination_uris` (03-03) | 9/9 ok |
| `api_v1_trunks` (Phase 2 baseline) | 23/23 ok |

## Hand-off Note for Phase 5

Phase 5 enforcement layer reads `media_config` column to filter SDP codec lists in the proxy hot path (488 rejection on mismatch). This plan guarantees:
- Column is either `NULL` (no config set) or a valid `TrunkMediaConfig` serialization (set via PUT)
- All codec strings are lowercase per D-10 wire format — Phase 5 can translate to rsipstack's uppercase RFC 3551 form without case-folding guards
- Empty codec list (`[]`) is stored as-is; Phase 5 must treat empty as "allow all"
