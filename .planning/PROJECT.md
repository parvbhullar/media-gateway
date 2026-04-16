# Media Gateway

## What This Is

Media Gateway is a Rust-based carrier-grade SIP proxy and B2BUA with an admin console. It terminates SIP on one side, relays or bridges RTP media, and handles routing, gateway health, CDR capture, and call recording. It is stable and shipping in production today under a rich console UI, but its programmatic REST control plane (`/api/v1/*`) is intentionally incomplete — only a Plan 0 auth shell plus three carrier routes are exposed.

## Core Value

Every SIP call — carrier-in, carrier-out, or bridged to WebRTC/WebSocket — is routed, controlled, observed, and billed through a single Rust binary with a first-class REST API that operators and tenant developers can both build against.

## Requirements

### Validated

<!-- Shipped in v1.0 baseline or validated through milestone phases. -->

- rsipstack-based SIP proxy with B2BUA dual-dialog architecture
- Media bridge with RTP relay, codec negotiation, transcoding
- SIP-to-WebRTC and SIP-to-WebSocket bridge modes
- RFC 4028 session timers on carrier-path calls
- Parallel dialer for concurrent gateway attempts (distribution)
- SIP trunk CRUD, health monitoring, failover via console
- DID assignment with routing modes (ai_agent, sip_proxy, webrtc_bridge, ws_bridge)
- Routing tables with LPM / regex / HTTP query matching
- Active call registry with hangup/transfer/command dispatch
- Call record storage, sipflow capture, streaming recording
- Console UI with sessions, RBAC, roles, users, settings
- SQL-backed storage via SeaORM with migrations
- AMI management router for health, reload, diagnostics, shutdown
- `/api/v1/*` bootstrap: Bearer-token auth middleware + gateway read + trunk-test
- Trunk groups entity layer (`trunk_groups` + `trunk_group_members`) with full CRUD -- Phase 2
- Distribution mode dispatch (round_robin, weight_based, hash_callid, hash_src_ip, hash_destination) -- Phase 2
- Gateway validation on trunk group membership + engagement-tracked delete -- Phase 2
- Additive migration preserving all legacy `sip_trunk` rows -- Phase 2
- Addons framework: archive, queue, voicemail, telemetry, observability, IVR editor, enterprise auth
- Docker multi-stage builds (aarch64, x86_64, commerce variant)
- TLS/ACME for the console surface

### Active

<!-- Current scope: v2.0 Carrier Control Plane — Feature Parity. -->

- Close the 86-route CARRIER-API gap so `/api/v1/*` reaches parity with operator-class carriers
- Add a CPaaS-shaped surface (~29 routes) modeled after Vobiz so tenant developers can integrate
- Introduce `trunk_groups` as a new entity layer above existing `sip_trunk` rows, preserving data
- Expose existing console logic as JSON adapters without breaking the HTML console
- Build the two new rule engines required by CARRIER-API: Translations (number rewrite) and Manipulations (SIP header rewrite)
- Promote global static security (firewall/flood/brute-force) to runtime-managed stores
- Expose SIP user-agent endpoints as a first-class `/api/v1/*` resource (distinct from SIP listeners)
- Expose Applications (Answer/Hangup/Message XML URLs) as a CPaaS routing primitive
- Expose mid-call REST control: play, speak, dtmf, record
- Expose CDR search, recent, CSV export; promote recording placeholders to real handlers
- Add Sub-accounts with per-tenant credentials for multi-tenancy
- Per-phase integration tests sufficient to merge changes against the proxy hot path

### Out of Scope

<!-- Explicit boundaries. -->

- Rewriting storage from SQL to Redis — the SeaORM model stays as-is
- Multi-listener SIP (static transports stay; endpoint write routes return `501`)
- True clustering — single-node only, `/system/cluster` hardcoded
- `parallel` distribution mode graduating beyond feature-flagged
- Porting super-voice (`src/`) Sofia/pjsip FFI work back into media-gateway
- Production hardening cross-cuts (load testing, OpenAPI publish, Dockerfile.carrier, systemd units, TLS/mTLS for api_v1, observability rollout) — deferred to v2.1
- Replacing the existing console UI
- Video / SMS / conferencing / voicemail beyond what the addons framework already ships

## Context

### Existing Codebase

- **`src/`**: main binary — proxy, media, handlers, models, console, addons
- **`src/handler/api_v1/`**: the target surface for this milestone (currently auth + `gateways` stub)
- **`src/console/handlers/`**: HTML admin routes that already own most CRUD logic we will wrap
- **`src/proxy/`**: B2BUA, routing, registrar, trunk_registrar, acl, locator, active_call_registry
- **`src/models/`**: SeaORM entities (sip_trunk, did, routing, call_record, api_key, frequency_limit, rbac, system_config, etc.)
- **`src/callrecord/`**: CDR storage, sipflow, recording upload
- **`docs/CARRIER-API.md`** (at super-voice root): 86-route carrier control plane spec — the primary contract
- **`docs/CARRIER-ARCHITECTURE.md`** (at super-voice root): architecture doc — reference only, not a binding plan
- **`docs/plans/2026-04-14-carrier-api-gap-closure.md`**: 12-phase gap closure plan driving this milestone
- **`docs/plans/2026-04-14-phase-1-api-shell.md`**: Phase 1 (API shell + wrappers) detailed plan
- **Vobiz comparison** (from docs.vobiz.ai): CPaaS reference shape driving Phase 13 scope

### Architectural Anchors

- One Rust SIP stack (rsipstack). No Sofia-SIP / pjsip FFI in this binary.
- One DB (SeaORM/SQL). No Redis dependency added by this milestone.
- One binary, one console, one api_v1 namespace. No microservices.
- `AppStateInner.console: Option<Arc<ConsoleState>>` is the pre-existing bridge between api_v1 and console logic — api_v1 and console handlers share the same `&DatabaseConnection`.
- Pure data-fetch fns live at module level in `console/handlers/*.rs`; both HTML and JSON handlers call them. Never serialize SeaORM models directly — view types live in `api_v1/*.rs`.

### Key Documents

- [docs/plans/2026-04-14-carrier-api-gap-closure.md](../docs/plans/2026-04-14-carrier-api-gap-closure.md) — gap closure design
- [docs/plans/2026-04-14-phase-1-api-shell.md](../docs/plans/2026-04-14-phase-1-api-shell.md) — Phase 1 detail
- `../docs/CARRIER-API.md` — 86-route contract
- `../docs/CARRIER-ARCHITECTURE.md` — architecture reference

## Constraints

- **Tech stack**: Rust. rsipstack only. SeaORM only. Axum for HTTP.
- **No regressions**: existing console UI must render identically; existing proxy call path must pass its existing tests throughout.
- **Schema additivity**: new columns/tables are additive; legacy data must survive every migration.
- **API contract source of truth**: `docs/CARRIER-API.md` for the 86 carrier routes; Vobiz docs for the 29 CPaaS routes. No silent deviations from documented shapes.
- **Bearer auth** is the api_v1 auth scheme. Sub-accounts add scoping, not a different auth scheme.
- **Feature gating**: every api_v1 sub-router ships behind a compile or runtime flag so partial delivery is safe.
- **`AppState` never becomes Redis-aware** in this milestone. If a phase needs shared runtime state, it uses in-memory `DashMap` or SQL.

## Current Milestone: v2.0 Carrier Control Plane — Feature Parity

**Goal:** Close the 86-route CARRIER-API gap and add the 29-route Vobiz-shaped CPaaS surface at `/api/v1/*`, wrapping existing console and proxy logic wherever possible and building new rule engines where the spec demands them — without regressing the stable baseline.

**Target features:**

- Core trunk CRUD with new `trunk_groups` schema
- Gateways write routes (create/update/delete) with engagement tracking
- DIDs, CDRs, Diagnostics, System/health+reload JSON wrappers
- Routing tables, records sub-routes, routing/resolve dry-run
- Translations engine (number rewrite before routing)
- Manipulations engine (conditional SIP header rewrite after routing)
- Security suite (firewall store, flood tracker, brute-force tracker, auto-blocks)
- Webhook pipeline for CDR delivery
- Active call control + mid-call REST (play/speak/dtmf/record)
- SIP user-agent endpoints as first-class `/api/v1/endpoints` (distinct from listeners)
- Applications (XML Answer/Hangup/Message URLs) as CPaaS routing primitive
- CDR search/recent/export, Recordings CRUD
- Sub-accounts + per-account auth scoping (opt-in, defaults to root account)
- Per-phase integration tests on every new sub-router

**Deferred to v2.1 (Production Hardening) — not in this milestone:**

- Load testing at 8k concurrent SIP-to-SIP
- OpenAPI 3.1 spec publication
- Dockerfile.carrier + systemd units + health probe rollout
- OpenTelemetry traces + metrics rollout across proxy hot path
- TLS/mTLS for api_v1
- Migration runbook + zero-downtime deploy validation
- Admin guide + tenant integration docs

## Key Decisions

<!-- Decisions that constrain future work. Add throughout project lifecycle. -->

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Wrap console logic via module-level `pub(crate)` data fns keyed on `&DatabaseConnection` | Both ConsoleState and AppState already share the same DB handle; no new bridge needed | — Pending |
| Introduce `trunk_groups` + `trunk_group_members` instead of collapsing trunks into sip_trunks | Collapse breaks the 19-route CARRIER-API trunk sub-resource contract; additive migration keeps existing rows intact | Validated Phase 2 |
| Endpoints = SIP user-agents in `/api/v1/endpoints`; SIP listeners remain config-only | Vobiz and media-gateway's own registrar model treat endpoint as user-agent; CARRIER-API listener routes become a read-only projection under a different path | — Pending |
| Translations run before routing; Manipulations run after routing | Routing must see normalized numbers; manipulations may depend on the chosen trunk | — Pending |
| Security suite moves from static file-loaded CIDR to DB-backed runtime store | Required for GET/PATCH firewall routes and auto-block lifecycle | — Pending |
| Sub-accounts default to a single "root" account so earlier phases don't retroactively need account scoping | Lets v2.0 phases 1-12 ship without touching every handler for multi-tenancy | — Pending |
| Production hardening (load test, OTel, TLS/mTLS, OpenAPI, Dockerfile.carrier) deferred to v2.1 | Features and hardening are separate commitments; cleaner to split milestones | — Pending |

---
*Last updated: 2026-04-16 after Phase 2*
