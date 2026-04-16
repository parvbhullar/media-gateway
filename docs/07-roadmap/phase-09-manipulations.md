# Phase 9: Manipulations Engine

## Goal

Ship the Manipulations rule engine so operators can conditionally rewrite SIP headers, set variables, or hang up calls after routing resolves the trunk.

## Dependencies

Phase 8.

## Requirements

- **MAN-01**: A new `rustpbx_manipulations` table + `models/manipulation.rs` entity exists
- **MAN-02**: Operator can CRUD manipulation classes via `/api/v1/manipulations` with rules containing conditions (and/or), actions, and anti_actions
- **MAN-03**: Condition fields support `caller_number`, `destination_number`, `trunk`, `header:<name>`, `var:<name>`
- **MAN-04**: Action types `set_header`, `remove_header`, `set_var`, `log`, `hangup`, `sleep` are implemented
- **MAN-05**: Manipulation pipeline runs AFTER routing so rules can depend on the chosen trunk; runs before the outbound INVITE hits the wire
- **MAN-06**: `hangup` action short-circuits with a chosen SIP code and integrates cleanly with `proxy_call/session.rs` teardown
- **MAN-07**: Anti-actions fire on the else branch when condition_mode evaluates false
- **IT-02**: Translations and Manipulations engines each have pipeline tests that place a simulated call through the dispatch path and assert rewritten numbers and mutated headers

## Success Criteria

1. Operator can CRUD manipulation classes via `/api/v1/manipulations` with and/or conditions and actions (`set_header`, `remove_header`, `set_var`, `log`, `hangup`, `sleep`)
2. Conditions evaluate over `caller_number`, `destination_number`, `trunk`, `header:<name>`, and `var:<name>` — including the chosen trunk from the prior routing step
3. A `hangup` action short-circuits with a chosen SIP code and cleanly tears down the dialog via `proxy_call/session.rs`
4. Anti-actions fire on the else branch when `condition_mode` evaluates false
5. A pipeline integration test simulates a call through dispatch and asserts both rewritten numbers (Translations) and mutated headers (Manipulations)

## Affected Subsystems

- [proxy](../04-subsystems/)
- [call](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/09-manipulations-engine/`
**Last reviewed:** 2026-04-16
