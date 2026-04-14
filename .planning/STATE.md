---
gsd_state_version: 1.0
milestone: v2.0
milestone_name: Carrier Control Plane — Feature Parity
status: planning
stopped_at: Milestone v2.0 bootstrap — defining requirements
last_updated: "2026-04-14T00:00:00.000Z"
last_activity: 2026-04-14 — Milestone v2.0 initialized (media-gateway bootstrap)
progress:
  total_phases: 0
  completed_phases: 0
  total_plans: 0
  completed_plans: 0
  percent: 0
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-04-14)

**Core value:** Every SIP call — carrier-in, carrier-out, or bridged to WebRTC/WebSocket — is routed, controlled, observed, and billed through a single Rust binary with a first-class REST API.
**Current focus:** Defining v2.0 requirements

## Current Position

Phase: Not started (defining requirements)
Plan: —
Status: Defining requirements
Last activity: 2026-04-14 — Milestone v2.0 started

Progress: [░░░░░░░░░░] 0%

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- Wrap console logic via module-level `pub(crate)` data fns keyed on `&DatabaseConnection`
- Introduce `trunk_groups` + `trunk_group_members` instead of collapsing trunks into sip_trunks
- Endpoints = SIP user-agents in `/api/v1/endpoints`; SIP listeners remain config-only
- Translations run before routing; Manipulations run after routing
- Security suite moves from static file-loaded CIDR to DB-backed runtime store
- Sub-accounts default to a single "root" account so earlier phases don't retroactively need account scoping
- Production hardening deferred to v2.1

### Roadmap Evolution

- Milestone v2.0 scope derives from `docs/plans/2026-04-14-carrier-api-gap-closure.md` + `docs/plans/2026-04-14-phase-1-api-shell.md` + Vobiz CPaaS comparison (Option B)
- Split chosen (Option C): v2.0 features → v2.1 production hardening

### Pending Todos

None yet.

### Blockers/Concerns

- The `sip_trunk` / `trunk_group` dual-shape is the highest-risk schema call — must land before any trunk sub-resource work
- Proxy hot-path changes (Phases adding per-trunk capacity/codec enforcement, translations pipeline hook, manipulations action dispatch) each need integration tests before merge
- Console state ↔ api_v1 state bridge relies on extracting pure data fns without changing HTML behavior — verify each extraction with a smoke test

## Session Continuity

Last session: 2026-04-14
Stopped at: Milestone v2.0 bootstrap — awaiting research decision
Resume file: None
