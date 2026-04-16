# Phase 2: Trunk Groups Schema & Core CRUD

## Goal

Introduce the `trunk_groups` entity layer above existing `sip_trunk` rows and ship core `/api/v1/trunks` CRUD without breaking legacy data.

## Dependencies

Phase 1.

## Requirements

- **TRK-01**: A new `rustpbx_trunk_groups` table and `rustpbx_trunk_group_members` join table exist; legacy `sip_trunk` rows are untouched
- **TRK-02**: Operator can CRUD trunk groups via `/api/v1/trunks` with name, direction, distribution mode, gateway member list, credentials, acl, nofailover_sip_codes
- **TRK-03**: Creating or updating a trunk group validates that every referenced gateway exists; returns 400 on missing reference
- **TRK-04**: Deleting a trunk group is blocked with 409 if any DID or routing record references it
- **TRK-05**: Distribution modes `round_robin`, `weight_based`, `hash_callid`, `hash_src_ip`, `hash_destination` are honored in dispatch; `parallel` is feature-flagged and off by default
- **MIG-01**: All new tables ship with backward-compatible migrations that run on existing databases without data loss

## Success Criteria

1. Operator can create, list, retrieve, update, and delete trunk groups via `/api/v1/trunks` with gateway member lists and distribution mode
2. Creating a trunk group that references a non-existent gateway returns 400; deleting a trunk group still referenced by a DID or routing record returns 409
3. Dispatch honors `round_robin`, `weight_based`, `hash_callid`, `hash_src_ip`, and `hash_destination` distribution modes; `parallel` is off unless its feature flag is set
4. Migration runs on an existing production database without modifying or losing any legacy `sip_trunk` rows

## Affected Subsystems

- [handler](../04-subsystems/)
- [proxy/routing](../04-subsystems/)
- [models](../04-subsystems/)

## Current Plans

- `02-01-PLAN.md` — Schema & Read-Only Surface
- `02-02-PLAN.md` — Write Handlers & Engagement Tracking
- `02-03-PLAN.md` — Dispatch Wiring

---
**Status:** 🚧 In Progress
**Planning artifacts:** `.planning/phases/02-trunk-groups-schema-core-crud/`
**Last reviewed:** 2026-04-16
