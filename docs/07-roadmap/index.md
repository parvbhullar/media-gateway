# Roadmap — v2.0 Carrier Control Plane

## Overview

v2.0 closes the gap between SuperSip's rich data plane (rsipstack proxy, SeaORM storage, console UI) and its intentionally-incomplete `/api/v1/*` control plane. Over 13 phases we ship ~75 of the 86 routes in `docs/CARRIER-API.md` plus a 29-route Vobiz-shaped CPaaS surface — wrapping existing console/proxy logic wherever possible and building new rule engines (Translations, Manipulations, Security suite, Webhooks, Applications) where the spec demands greenfield work. Each phase is independently shippable behind a feature-flagged sub-router mount so partial delivery never regresses the stable baseline.

## Progress

Phase 1 verified. Phase 2 in progress. 8% complete (1 of 13 phases).

| Phase | Name | Status | Details |
|-------|------|--------|---------|
| 1 | API Shell & Cheap Wrappers | ✅ Shipped | [Details](phase-01-api-shell.md) |
| 2 | Trunk Groups Schema & Core CRUD | 🚧 In Progress | [Details](phase-02-trunk-groups.md) |
| 3 | Trunk Sub-Resources L1 & Routing Resolve | 📋 Planned | [Details](phase-03-trunk-sub-resources.md) |
| 4 | Active Calls & Mid-Call Control | 📋 Planned | [Details](phase-04-active-calls.md) |
| 5 | Trunk Enforcement (Capacity, ACL, Codec Filter) | 📋 Planned | [Details](phase-05-trunk-enforcement.md) |
| 6 | Routing Tables, Records & Distribution | 📋 Planned | [Details](phase-06-routing.md) |
| 7 | Webhook Pipeline | 📋 Planned | [Details](phase-07-webhooks.md) |
| 8 | Translations Engine | 📋 Planned | [Details](phase-08-translations.md) |
| 9 | Manipulations Engine | 📋 Planned | [Details](phase-09-manipulations.md) |
| 10 | Security Suite | 📋 Planned | [Details](phase-10-security.md) |
| 11 | System Polish & CDR Export | 📋 Planned | [Details](phase-11-system-polish.md) |
| 12 | Listeners Projection & Recordings First-Class | 📋 Planned | [Details](phase-12-listeners-recordings.md) |
| 13 | CPaaS Layer (Endpoints, Applications, Sub-Accounts) | 📋 Planned | [Details](phase-13-cpaas.md) |

## Phase Dependencies

Phases 1→2→3→5, 2→4, 3→6→8→9, 1→7, 1→10, 1→11, 1→12→13

Phases execute in numeric order (1 → 2 → 3 → ... → 13). Each phase is independently shippable once its dependencies are met.
