# Phase 12: Listeners Projection & Recordings First-Class

## Goal

Expose SIP transports as a read-only projection and promote recordings from CDR placeholders to first-class `/api/v1/recordings` resource.

## Dependencies

Phase 1.

## Requirements

- **LSTN-01**: `GET /api/v1/listeners` returns read-only projection of `ProxyConfig` transports (udp/tcp/tls/ws) with bind addr, port, enabled flag
- **LSTN-02**: `GET /api/v1/listeners/{name}` returns a single transport by name
- **LSTN-03**: Write attempts on listeners (`POST`/`PUT`/`DELETE`) return `501 Not Implemented` with a body explaining that multi-listener is not supported; transports are configured via settings
- **LSTN-04**: The `/api/v1/endpoints` path is reserved for SIP user-agents (Phase 13), NOT listeners — listeners use `/api/v1/listeners`
- **REC-01**: `GET /api/v1/recordings` lists recordings with filters and pagination
- **REC-02**: `GET /api/v1/recordings/{id}` returns recording metadata
- **REC-03**: `GET /api/v1/recordings/{id}/download` streams the recording file
- **REC-04**: `DELETE /api/v1/recordings/{id}` deletes a recording (file + DB row)
- **REC-05**: `POST /api/v1/recordings/export` exports multiple recordings as an archive
- **REC-06**: `DELETE /api/v1/recordings/bulk` deletes recordings matching criteria (date range, trunk, status)
- **REC-07**: Recording endpoints wrap existing `callrecord/storage.rs` and `callrecord/sipflow.rs` — no new storage layer
- **MIG-02**: Every migration has a documented rollback path (or is explicitly documented as forward-only)

## Success Criteria

1. `GET /api/v1/listeners` returns a read-only projection of `ProxyConfig` transports; `POST/PUT/DELETE` return `501 Not Implemented` with a clear body explaining multi-listener is unsupported
2. Operator can list, retrieve, download, and delete recordings via `/api/v1/recordings` — all routes wrap existing `callrecord/storage.rs` with no new storage layer
3. Operator can export multiple recordings as an archive via `POST /api/v1/recordings/export` and bulk-delete via criteria (date range, trunk, status)
4. Every new table shipped in this milestone has a documented rollback path (or is explicitly marked forward-only)

## Affected Subsystems

- [handler](../04-subsystems/)
- [callrecord](../04-subsystems/)
- [sipflow](../04-subsystems/)
- [storage](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/12-listeners-recordings/`
**Last reviewed:** 2026-04-16
