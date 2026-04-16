# Phase 3: Trunk Sub-Resources L1 & Routing Resolve

## Goal

Ship schema-level trunk sub-resources (credentials, origination URIs, media config) and the routing dry-run endpoint, all without touching the proxy hot path.

## Dependencies

Phase 2.

## Requirements

- **TSUB-01**: Per-trunk credentials CRUD at `/api/v1/trunks/{name}/credentials` and `/api/v1/trunks/{name}/credentials/{realm}`
- **TSUB-02**: Per-trunk origination URIs CRUD at `/api/v1/trunks/{name}/origination_uris` and `/api/v1/trunks/{name}/origination_uris/{uri}`
- **TSUB-03**: Per-trunk media config (codec list, dtmf mode, srtp, media mode) GET/PUT at `/api/v1/trunks/{name}/media`
- **RTE-03**: `POST /api/v1/routing/resolve` dry-runs a caller/destination against the live routing engine and returns the chosen target(s) without placing a call

## Success Criteria

1. Operator can CRUD per-trunk credentials and origination URIs via `/api/v1/trunks/{name}/credentials` and `/origination_uris`
2. Operator can GET and PUT per-trunk media config (codec list, dtmf mode, srtp, media mode)
3. `POST /api/v1/routing/resolve` dry-runs a caller/destination pair against the live routing engine and returns the chosen target(s) without placing a call

## Affected Subsystems

- [handler](../04-subsystems/)
- [proxy/routing](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/03-trunk-sub-resources-l1-routing-resolve/`
**Last reviewed:** 2026-04-16
