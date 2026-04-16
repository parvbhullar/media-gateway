# Phase 10: Security Suite

## Goal

Promote security from static file-loaded CIDR to a DB-backed runtime store with firewall, flood tracker, brute-force tracker, auto-blocks, and topology hiding.

## Dependencies

Phase 1.

## Requirements

- **SEC-01**: Firewall store is promoted from static file-loaded CIDR to a DB-backed `rustpbx_security_rules` runtime store with `GET /api/v1/security/firewall` and `PATCH /api/v1/security/firewall`
- **SEC-02**: Flood tracker maintains a per-IP sliding window and returns 503 for incoming SIP when threshold is breached; stats queryable via `GET /api/v1/security/flood-tracker`
- **SEC-03**: Brute-force tracker records auth failures keyed on `(ip, realm)`, returns 403 after threshold, writes blocks to a new `rustpbx_security_blocks` table
- **SEC-04**: `GET /api/v1/security/blocks` lists auto-blocked IPs; `DELETE /api/v1/security/blocks/{ip}` unblocks
- **SEC-05**: `GET /api/v1/security/auth-failures` exposes recent auth failure stats
- **SEC-06**: Topology hiding (strip internal Via/Record-Route) is exposed as a config flag over existing `proxy_call/session.rs` logic, toggleable at runtime

## Success Criteria

1. Operator can GET/PATCH `/api/v1/security/firewall` and edits land in `rustpbx_security_rules` and take effect without a proxy restart
2. A flooding IP is rejected with SIP 503 once its sliding window threshold is breached and the event is visible via `GET /api/v1/security/flood-tracker`
3. Repeated auth failures from `(ip, realm)` write a row to `rustpbx_security_blocks` and return 403 thereafter; `GET /api/v1/security/blocks` lists them and `DELETE /api/v1/security/blocks/{ip}` unblocks
4. Topology hiding (internal Via/Record-Route stripping) can be toggled at runtime via the new config flag over existing `proxy_call/session.rs` logic

## Affected Subsystems

- [proxy](../04-subsystems/)
- [handler](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/10-security-suite/`
**Last reviewed:** 2026-04-16
