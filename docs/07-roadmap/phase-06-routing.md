# Phase 6: Routing Tables, Records & Distribution

## Goal

Ship full `/api/v1/routing/*` CRUD including the routing records sub-route adapter for console's embedded-document storage.

## Dependencies

Phase 3.

## Requirements

- **RTE-01**: Operator can CRUD routing tables via `/api/v1/routing/tables`
- **RTE-02**: Operator can CRUD routing records within a table via `/api/v1/routing/tables/{name}/records` and `/records/{index}`, even though console stores records as an embedded document (adapter-only)
- **RTE-04**: Match types `Lpm`, `ExactMatch`, `Regex`, `Compare`, `HttpQuery` are all supported and covered by integration tests
- **RTE-05**: A routing table can designate a default record via `is_default: true`; resolve returns the default when no match

## Success Criteria

1. Operator can CRUD routing tables via `/api/v1/routing/tables`
2. Operator can CRUD individual routing records via `/api/v1/routing/tables/{name}/records` and `/records/{index}` even though console stores them as embedded documents
3. All five match types (`Lpm`, `ExactMatch`, `Regex`, `Compare`, `HttpQuery`) resolve correctly against integration tests
4. A routing table marked `is_default: true` returns its default record when no rule matches

## Affected Subsystems

- [handler](../04-subsystems/)
- [proxy/routing](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/06-routing-tables-records-distribution/`
**Last reviewed:** 2026-04-16
