# Phase 5: Trunk Enforcement (Capacity, ACL, Codec Filter)

## Goal

Promote per-trunk capacity, ACL, and codec filtering from schema into proxy hot-path enforcement so the sub-resources become observable in call outcomes.

## Dependencies

Phase 3.

## Requirements

- **TSUB-04**: Per-trunk capacity (max_calls, max_cps) GET/PUT at `/api/v1/trunks/{name}/capacity`, enforced by proxy dispatch before gateway selection
- **TSUB-05**: Per-trunk ACL CRUD at `/api/v1/trunks/{name}/acl` and `/api/v1/trunks/{name}/acl/{entry}`; enforced in ingress check alongside global firewall
- **TSUB-06**: Media config filtering: if a caller SDP codec intersection with the trunk codec list is empty, the call is rejected with 488 Not Acceptable Here
- **TSUB-07**: Trunk capacity enforcement is observable via `GET /api/v1/trunks/{name}/capacity` showing current active count
- **IT-03**: Trunk capacity enforcement, codec filtering (488 on mismatch), and per-trunk ACL each have a proxy integration test

## Success Criteria

1. Operator can GET/PUT per-trunk `capacity` (max_calls, max_cps) and see live active counts reflected in the response
2. A caller with no codec overlap against the trunk codec list is rejected with SIP 488 Not Acceptable Here
3. Per-trunk ACL entries are enforced on ingress alongside the global firewall, blocking unauthorized sources
4. Integration tests exercise capacity exhaustion, codec mismatch, and ACL block paths end-to-end through the dispatch flow

## Affected Subsystems

- [proxy](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/05-trunk-enforcement/`
**Last reviewed:** 2026-04-16
