# Roadmap: Media Gateway

## Overview

v2.0 closes the gap between media-gateway's rich data plane (rsipstack proxy, SeaORM storage, console UI) and its intentionally-incomplete `/api/v1/*` control plane. Over 13 phases we ship ~75 of the 86 routes in `docs/CARRIER-API.md` plus a 29-route Vobiz-shaped CPaaS surface — wrapping existing console/proxy logic wherever possible and building new rule engines (Translations, Manipulations, Security suite, Webhooks, Applications) where the spec demands greenfield work. Each phase is independently shippable behind a feature-flagged sub-router mount so partial delivery never regresses the stable baseline.

## Milestones

- 📋 **v2.0 Carrier Control Plane — Feature Parity** - Phases 1-13 (planned)

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked INSERTED)

- [ ] **Phase 1: API Shell & Cheap Wrappers** - Adapter convention, Gateways writes, DIDs, CDRs, Diagnostics, System health/reload
- [ ] **Phase 2: Trunk Groups Schema & Core CRUD** - `rustpbx_trunk_groups` + `/api/v1/trunks` CRUD
- [ ] **Phase 3: Trunk Sub-Resources L1 & Routing Resolve** - Credentials, origination URIs, media schema, `/routing/resolve` dry-run
- [ ] **Phase 4: Active Calls & Mid-Call Control** - List/get/hangup/transfer/mute + play/speak/dtmf/record
- [ ] **Phase 5: Trunk Enforcement (Capacity, ACL, Codec Filter)** - Proxy hot-path enforcement for per-trunk limits
- [ ] **Phase 6: Routing Tables, Records & Distribution** - Full routing CRUD + record adapter + distribution modes
- [ ] **Phase 7: Webhook Pipeline** - CDR delivery with HMAC, retries, disk fallback
- [ ] **Phase 8: Translations Engine** - Number rewrite before routing
- [ ] **Phase 9: Manipulations Engine** - SIP header rewrite after routing
- [ ] **Phase 10: Security Suite** - Runtime firewall, flood, brute-force, auto-blocks, topology hiding
- [ ] **Phase 11: System Polish & CDR Export** - `/system/info|config|stats|cluster` + CDR search/recent/export
- [ ] **Phase 12: Listeners Projection & Recordings First-Class** - Read-only listeners, recordings CRUD/download/export
- [ ] **Phase 13: CPaaS Layer (Endpoints, Applications, Sub-Accounts)** - Vobiz-shaped user-agent endpoints, XML routing, multi-tenancy

## Phase Details

### Phase 1: API Shell & Cheap Wrappers
**Goal**: Establish the adapter convention for the entire milestone and ship ~17 routes that wrap existing console handlers with zero new business logic.
**Depends on**: Nothing (first phase)
**Requirements**: SHELL-01, SHELL-02, SHELL-03, SHELL-04, SHELL-05, GWY-01, GWY-02, GWY-03, GWY-04, DID-01, DID-02, DID-03, DID-04, CDR-01, CDR-02, CDR-03, CDR-04, DIAG-01, DIAG-02, DIAG-03, DIAG-04, DIAG-05, SYS-01, SYS-02, IT-01, MIG-03
**Success Criteria** (what must be TRUE):
  1. Operator can CRUD Gateways, DIDs, and CDRs via `/api/v1/*` with Bearer auth and get back the JSON shapes documented in `docs/CARRIER-API.md`
  2. Operator can run route-evaluate, probe a gateway, list registrations, query locator state, and fetch a diagnostics summary without touching the console UI
  3. `GET /api/v1/system/health` returns uptime/db_ok/active_calls/version and `POST /api/v1/system/reload` collapses all four AMI reload endpoints into one call with elapsed time
  4. Every existing console HTML route (sip_trunks, dids, call-records, routing, diagnostics, settings) renders identically after the data-fn extraction refactor
  5. Every sub-router ships with an integration test asserting 401-without-auth, happy-path, 404-missing, 400/409-bad-input
**Plans**: TBD

### Phase 2: Trunk Groups Schema & Core CRUD
**Goal**: Introduce the `trunk_groups` entity layer above existing `sip_trunk` rows and ship core `/api/v1/trunks` CRUD without breaking legacy data.
**Depends on**: Phase 1
**Requirements**: TRK-01, TRK-02, TRK-03, TRK-04, TRK-05, MIG-01
**Success Criteria** (what must be TRUE):
  1. Operator can create, list, retrieve, update, and delete trunk groups via `/api/v1/trunks` with gateway member lists and distribution mode
  2. Creating a trunk group that references a non-existent gateway returns 400; deleting a trunk group still referenced by a DID or routing record returns 409
  3. Dispatch honors `round_robin`, `weight_based`, `hash_callid`, `hash_src_ip`, and `hash_destination` distribution modes; `parallel` is off unless its feature flag is set
  4. Migration runs on an existing production database without modifying or losing any legacy `sip_trunk` rows

### Phase 3: Trunk Sub-Resources L1 & Routing Resolve
**Goal**: Ship schema-level trunk sub-resources (credentials, origination URIs, media config) and the routing dry-run endpoint, all without touching the proxy hot path.
**Depends on**: Phase 2
**Requirements**: TSUB-01, TSUB-02, TSUB-03, RTE-03
**Plans:** 5 plans
**Success Criteria** (what must be TRUE):
  1. Operator can CRUD per-trunk credentials and origination URIs via `/api/v1/trunks/{name}/credentials` and `/origination_uris`
  2. Operator can GET and PUT per-trunk media config (codec list, dtmf mode, srtp, media mode)
  3. `POST /api/v1/routing/resolve` dry-runs a caller/destination pair against the live routing engine and returns the chosen target(s) without placing a call

Plans:
- [ ] 03-01-PLAN.md — Schema migrations (4) + 4 stub sub-routers wired into mod.rs + Phase 2 test split
- [ ] 03-02-PLAN.md — TSUB-01: trunk credentials full implementation + IT-01 tests
- [ ] 03-03-PLAN.md — TSUB-02: trunk origination URIs full implementation + IT-01 tests
- [ ] 03-04-PLAN.md — TSUB-03: trunk media config GET/PUT + IT-01 tests
- [ ] 03-05-PLAN.md — RTE-03: /routing/resolve dry-run via match_invite_with_trace + IT-01 tests

### Phase 4: Active Calls & Mid-Call Control
**Goal**: Expose the active call registry and dispatch mid-call REST commands through the existing `proxy_call/session.rs` path.
**Depends on**: Phase 2
**Requirements**: CALL-01, CALL-02, CALL-03, CALL-04, CALL-05, CALL-06, CALL-07, CALL-08, CALL-09, CALL-10
**Plans:** 5 plans
**Success Criteria** (what must be TRUE):
  1. Operator can list active calls with pagination and retrieve a single call by id
  2. Operator can hangup, transfer (attended and blind), and mute/unmute an active call leg
  3. Operator can inject `play`, `speak`, `dtmf`, and `record` commands into a live call and observe them land via the active call registry
  4. All mid-call operations dispatch through the existing `active_call_registry` → `proxy_call/session.rs` path with no new proxy modules

Plans:
- [x] 04-01-PLAN.md — Payload relocation (CallCommandPayload → call/runtime) + CALL-01/02 list/get foundation
- [x] 04-02-PLAN.md — CALL-03/05 hangup + mute/unmute via leg→track_id constants
- [ ] 04-03-PLAN.md — CALL-04 transfer (blind + attended + complete + cancel) with CommandResult/SessionSnapshot extensions
- [ ] 04-04-PLAN.md — CALL-06/07/08 play + speak + dtmf with pre-dispatch probes and SendDtmf timing extension
- [ ] 04-05-PLAN.md — CALL-09 record with auto-path + transcribe marker + full-suite regression

### Phase 5: Trunk Enforcement (Capacity, ACL, Codec Filter)
**Goal**: Promote per-trunk capacity, ACL, and codec filtering from schema into proxy hot-path enforcement so the sub-resources become observable in call outcomes.
**Depends on**: Phase 3
**Requirements**: TSUB-04, TSUB-05, TSUB-06, TSUB-07, IT-03
**Success Criteria** (what must be TRUE):
  1. Operator can GET/PUT per-trunk `capacity` (max_calls, max_cps) and see live active counts reflected in the response
  2. A caller with no codec overlap against the trunk codec list is rejected with SIP 488 Not Acceptable Here
  3. Per-trunk ACL entries are enforced on ingress alongside the global firewall, blocking unauthorized sources
  4. Integration tests exercise capacity exhaustion, codec mismatch, and ACL block paths end-to-end through the dispatch flow

### Phase 6: Routing Tables, Records & Distribution
**Goal**: Ship full `/api/v1/routing/*` CRUD including the routing records sub-route adapter for console's embedded-document storage.
**Depends on**: Phase 3
**Requirements**: RTE-01, RTE-02, RTE-04, RTE-05
**Success Criteria** (what must be TRUE):
  1. Operator can CRUD routing tables via `/api/v1/routing/tables`
  2. Operator can CRUD individual routing records via `/api/v1/routing/tables/{name}/records` and `/records/{index}` even though console stores them as embedded documents
  3. All five match types (`Lpm`, `ExactMatch`, `Regex`, `Compare`, `HttpQuery`) resolve correctly against integration tests
  4. A routing table marked `is_default: true` returns its default record when no rule matches

### Phase 7: Webhook Pipeline
**Goal**: Ship a CRUD webhook registry plus a background processor that delivers CDR completion events with HMAC signing, retries, and disk fallback.
**Depends on**: Phase 1
**Requirements**: WH-01, WH-02, WH-03, WH-04, WH-05, WH-06
**Success Criteria** (what must be TRUE):
  1. Operator can CRUD webhooks via `/api/v1/webhooks` and each new webhook fires a synchronous test event whose failure is logged but non-fatal
  2. Completion of a call in `callrecord/` triggers webhook delivery with JSON payload, HMAC header, and the documented `X-Webhook-Event`/`X-Webhook-Secret`/request-id headers
  3. A failing webhook target is retried 3 times with exponential backoff and then written to a disk JSON fallback under `ProxyConfig.generated_dir`
  4. Deleting a webhook cancels any in-flight retries for that endpoint

### Phase 8: Translations Engine
**Goal**: Ship the Translations rule engine so inbound calls are normalized (e.g., `02079460123 → +442079460123`) before the router sees them.
**Depends on**: Phase 6
**Requirements**: TRN-01, TRN-02, TRN-03, TRN-04, TRN-05, TRN-06
**Success Criteria** (what must be TRUE):
  1. Operator can CRUD translation classes via `/api/v1/translations` with caller/destination regex patterns, replacements, and direction
  2. An inbound call hitting a matching translation rule arrives at the routing stage with rewritten caller and destination numbers
  3. An `inbound`-scoped rule does NOT fire on an outbound leg (direction filter observed)
  4. End-to-end integration test asserts `02079460123 → +442079460123` and `4155551234 → +14155551234` through the live dispatch path

### Phase 9: Manipulations Engine
**Goal**: Ship the Manipulations rule engine so operators can conditionally rewrite SIP headers, set variables, or hang up calls after routing resolves the trunk.
**Depends on**: Phase 8
**Requirements**: MAN-01, MAN-02, MAN-03, MAN-04, MAN-05, MAN-06, MAN-07, IT-02
**Success Criteria** (what must be TRUE):
  1. Operator can CRUD manipulation classes via `/api/v1/manipulations` with and/or conditions and actions (`set_header`, `remove_header`, `set_var`, `log`, `hangup`, `sleep`)
  2. Conditions evaluate over `caller_number`, `destination_number`, `trunk`, `header:<name>`, and `var:<name>` — including the chosen trunk from the prior routing step
  3. A `hangup` action short-circuits with a chosen SIP code and cleanly tears down the dialog via `proxy_call/session.rs`
  4. Anti-actions fire on the else branch when `condition_mode` evaluates false
  5. A pipeline integration test simulates a call through dispatch and asserts both rewritten numbers (Translations) and mutated headers (Manipulations)

### Phase 10: Security Suite
**Goal**: Promote security from static file-loaded CIDR to a DB-backed runtime store with firewall, flood tracker, brute-force tracker, auto-blocks, and topology hiding.
**Depends on**: Phase 1
**Requirements**: SEC-01, SEC-02, SEC-03, SEC-04, SEC-05, SEC-06
**Success Criteria** (what must be TRUE):
  1. Operator can GET/PATCH `/api/v1/security/firewall` and edits land in `rustpbx_security_rules` and take effect without a proxy restart
  2. A flooding IP is rejected with SIP 503 once its sliding window threshold is breached and the event is visible via `GET /api/v1/security/flood-tracker`
  3. Repeated auth failures from `(ip, realm)` write a row to `rustpbx_security_blocks` and return 403 thereafter; `GET /api/v1/security/blocks` lists them and `DELETE /api/v1/security/blocks/{ip}` unblocks
  4. Topology hiding (internal Via/Record-Route stripping) can be toggled at runtime via the new config flag over existing `proxy_call/session.rs` logic

### Phase 11: System Polish & CDR Export
**Goal**: Finish the `/api/v1/system/*` group and ship CDR search/recent/export surface at Vobiz parity.
**Depends on**: Phase 1
**Requirements**: SYS-03, SYS-04, SYS-05, SYS-06, CDR-05, CDR-06, CDR-07, MIG-04
**Success Criteria** (what must be TRUE):
  1. `GET /api/v1/system/info`, `/config`, `/stats`, and `/cluster` all return their documented payloads (including the hardcoded single-node cluster response)
  2. `GET /api/v1/cdrs` supports search-with-filter-summary, recent-without-date-range, and CSV export streaming with all documented columns
  3. `handler/ami.rs` endpoints continue responding but `/api/v1/system/*` is documented as the supported surface going forward

### Phase 12: Listeners Projection & Recordings First-Class
**Goal**: Expose SIP transports as a read-only projection and promote recordings from CDR placeholders to first-class `/api/v1/recordings` resource.
**Depends on**: Phase 1
**Requirements**: LSTN-01, LSTN-02, LSTN-03, LSTN-04, REC-01, REC-02, REC-03, REC-04, REC-05, REC-06, REC-07, MIG-02
**Success Criteria** (what must be TRUE):
  1. `GET /api/v1/listeners` returns a read-only projection of `ProxyConfig` transports; `POST/PUT/DELETE` return `501 Not Implemented` with a clear body explaining multi-listener is unsupported
  2. Operator can list, retrieve, download, and delete recordings via `/api/v1/recordings` — all routes wrap existing `callrecord/storage.rs` with no new storage layer
  3. Operator can export multiple recordings as an archive via `POST /api/v1/recordings/export` and bulk-delete via criteria (date range, trunk, status)
  4. Every new table shipped in this milestone has a documented rollback path (or is explicitly marked forward-only)

### Phase 13: CPaaS Layer (Endpoints, Applications, Sub-Accounts)
**Goal**: Ship the 29-route Vobiz-shaped CPaaS surface: SIP user-agent endpoints, XML Applications routing, and multi-tenant sub-accounts with per-request account scoping.
**Depends on**: Phase 12
**Requirements**: EPUA-01, EPUA-02, EPUA-03, EPUA-04, EPUA-05, APP-01, APP-02, APP-03, APP-04, APP-05, APP-06, TEN-01, TEN-02, TEN-03, TEN-04, TEN-05, TEN-06, IT-04, IT-05
**Success Criteria** (what must be TRUE):
  1. Tenant developer can CRUD SIP user-agent endpoints via `/api/v1/endpoints` and see live `sip_registered` status derived from the registrar — all wrapping existing `proxy/user_extension.rs` and `registrar.rs`
  2. Tenant developer can CRUD Applications with `answer_url`/`hangup_url`/`message_url`, attach phone numbers to them, and see an incoming call fetch XML from the answer_url and execute verbs (`Play`, `Speak`, `Dial`, `Hangup`, `GetDigits`, `Record`) through the existing IVR runtime
  3. Operator can CRUD sub-accounts via `/api/v1/sub-accounts`; every existing api_v1 row defaults to the `root` account and API keys resolve to an `account_id` scope on every request
  4. A sub-account Bearer token cannot read or mutate another sub-account's trunk, DID, webhook, or recording — verified by integration test
  5. The sub-accounts migration is additive and all pre-existing rows inherit the `root` account_id without data loss

## Progress

**Execution Order:**
Phases execute in numeric order: 1 → 2 → 3 → ... → 13

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. API Shell & Cheap Wrappers | 0/TBD | Not started | - |
| 2. Trunk Groups Schema & Core CRUD | 0/TBD | Not started | - |
| 3. Trunk Sub-Resources L1 & Routing Resolve | 0/5 | Planned | - |
| 4. Active Calls & Mid-Call Control | 0/5 | Planned | - |
| 5. Trunk Enforcement | 0/TBD | Not started | - |
| 6. Routing Tables, Records & Distribution | 0/TBD | Not started | - |
| 7. Webhook Pipeline | 0/TBD | Not started | - |
| 8. Translations Engine | 0/TBD | Not started | - |
| 9. Manipulations Engine | 0/TBD | Not started | - |
| 10. Security Suite | 0/TBD | Not started | - |
| 11. System Polish & CDR Export | 0/TBD | Not started | - |
| 12. Listeners Projection & Recordings First-Class | 0/TBD | Not started | - |
| 13. CPaaS Layer | 0/TBD | Not started | - |

---
*Roadmap created: 2026-04-15*
