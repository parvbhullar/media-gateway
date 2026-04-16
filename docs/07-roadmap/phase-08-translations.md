# Phase 8: Translations Engine

## Goal

Ship the Translations rule engine so inbound calls are normalized (e.g., `02079460123 → +442079460123`) before the router sees them.

## Dependencies

Phase 6.

## Requirements

- **TRN-01**: A new `rustpbx_translations` table + `models/translation.rs` entity exists
- **TRN-02**: Operator can CRUD translation classes via `/api/v1/translations` with caller/destination regex patterns, replacements, and direction (`inbound`/`outbound`/`both`)
- **TRN-03**: `proxy/translation/engine.rs` compiles and caches regex rules keyed on rule id
- **TRN-04**: Inbound call pipeline applies matching translation rules to caller and destination numbers BEFORE routing
- **TRN-05**: Translation engine honors direction filter — inbound-only rules do not fire on outbound legs
- **TRN-06**: An integration test exercises `02079460123 → +442079460123` and `4155551234 → +14155551234` end-to-end through the pipeline

## Success Criteria

1. Operator can CRUD translation classes via `/api/v1/translations` with caller/destination regex patterns, replacements, and direction
2. An inbound call hitting a matching translation rule arrives at the routing stage with rewritten caller and destination numbers
3. An `inbound`-scoped rule does NOT fire on an outbound leg (direction filter observed)
4. End-to-end integration test asserts `02079460123 → +442079460123` and `4155551234 → +14155551234` through the live dispatch path

## Affected Subsystems

- [proxy/routing](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/08-translations-engine/`
**Last reviewed:** 2026-04-16
