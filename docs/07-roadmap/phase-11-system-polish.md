# Phase 11: System Polish & CDR Export

## Goal

Finish the `/api/v1/system/*` group and ship CDR search/recent/export surface at Vobiz parity.

## Dependencies

Phase 1.

## Requirements

- **SYS-03**: `GET /api/v1/system/info` returns version + build info from `version.rs`
- **SYS-04**: `GET /api/v1/system/config` returns a non-sensitive subset of effective `ProxyConfig` + `system_config` rows
- **SYS-05**: `GET /api/v1/system/stats` returns JSON stats derived from the existing `metrics.rs` Prometheus registry
- **SYS-06**: `GET /api/v1/system/cluster` returns a hardcoded single-node response documented as intentional
- **CDR-05**: CDR search returns a filter summary alongside results (Vobiz parity)
- **CDR-06**: CDR recent returns the N most recent CDRs without requiring a date range
- **CDR-07**: CDR export streams results as CSV with all documented columns
- **MIG-04**: Existing `ami.rs` endpoints continue responding but `/api/v1/system/*` is documented as the supported surface going forward

## Success Criteria

1. `GET /api/v1/system/info`, `/config`, `/stats`, and `/cluster` all return their documented payloads (including the hardcoded single-node cluster response)
2. `GET /api/v1/cdrs` supports search-with-filter-summary, recent-without-date-range, and CSV export streaming with all documented columns
3. `handler/ami.rs` endpoints continue responding but `/api/v1/system/*` is documented as the supported surface going forward

## Affected Subsystems

- [handler](../04-subsystems/)
- [callrecord](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/11-system-polish-cdr-export/`
**Last reviewed:** 2026-04-16
