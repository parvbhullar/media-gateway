# Requirements: Media Gateway

**Defined:** 2026-04-14
**Milestone:** v2.0 — Carrier Control Plane (Feature Parity)
**Core Value:** Every SIP call is routed, controlled, observed, and billed through a single Rust binary with a first-class REST API.

Requirement IDs follow `[CATEGORY]-[NUMBER]`. All v2.0 requirements are user/operator-centric, testable, and atomic.

## v2.0 Requirements

### API Shell & Adapter Foundation (SHELL)

- [ ] **SHELL-01**: `/api/v1/*` sub-router loading pattern supports one file per group, merged into the existing Bearer-authenticated root router
- [ ] **SHELL-02**: A shared `Pagination` extractor (`page`, `page_size`) and `PaginatedResponse<T>` envelope are usable from every api_v1 handler
- [ ] **SHELL-03**: `ApiError` supports `bad_request`, `conflict`, `not_implemented` in addition to existing variants
- [ ] **SHELL-04**: Every api_v1 handler uses a `DidView`-style view type; no SeaORM `Model` is ever serialized directly
- [ ] **SHELL-05**: A console handler refactor convention exists where data-fetch fns become module-level `pub(crate)` functions keyed on `&DatabaseConnection`, and both HTML and JSON handlers call them

### Gateways (GWY)

- [ ] **GWY-01**: Operator can create a gateway via `POST /api/v1/gateways` with auth, health thresholds, and transport config
- [ ] **GWY-02**: Operator can update an existing gateway via `PUT /api/v1/gateways/{name}` without restarting health monitoring
- [ ] **GWY-03**: Operator can delete a gateway via `DELETE /api/v1/gateways/{name}`; deletion is blocked with 409 if any trunk-group or DID references it
- [ ] **GWY-04**: Gateway create hooks the existing `proxy/gateway_health.rs` monitor loop so health state is visible via existing GET routes immediately

### DIDs (DID)

- [ ] **DID-01**: Operator can list DIDs with pagination and filters (trunk, mode, active)
- [ ] **DID-02**: Operator can create a DID with routing mode (`ai_agent`, `sip_proxy`, `webrtc_bridge`, `ws_bridge`)
- [ ] **DID-03**: Operator can retrieve, update, and delete a DID by number via `/api/v1/dids/{number}` (URL-encoded `+`)
- [ ] **DID-04**: DID lifecycle uses the same underlying model the console UI uses; console rendering is unchanged after the refactor

### CDRs (CDR)

- [ ] **CDR-01**: Operator can list CDRs with filters (trunk, did, status, start_date, end_date, page, page_size)
- [ ] **CDR-02**: Operator can retrieve a single CDR by id
- [ ] **CDR-03**: Operator can delete a CDR by id
- [ ] **CDR-04**: Recording and sip-flow sub-resources return `501 Not Implemented` in Phase 1, promoted to real handlers in the Recordings phase
- [ ] **CDR-05**: CDR search returns a filter summary alongside results (Vobiz parity)
- [ ] **CDR-06**: CDR recent returns the N most recent CDRs without requiring a date range
- [ ] **CDR-07**: CDR export streams results as CSV with all documented columns

### Diagnostics (DIAG)

- [ ] **DIAG-01**: Operator can run route-evaluate as a dry-run matching a caller/destination pair against the live routing table
- [ ] **DIAG-02**: Operator can probe a gateway's OPTIONS response on demand without affecting health counters
- [ ] **DIAG-03**: Operator can list SIP registrations and query a single user's registration
- [ ] **DIAG-04**: Operator can query locator state (list and clear) for a given aor
- [ ] **DIAG-05**: Operator can fetch a combined diagnostics summary (registrations, health, recent flood events, recent auth failures)

### System (SYS)

- [ ] **SYS-01**: `GET /api/v1/system/health` returns uptime, db status, active call count, version
- [ ] **SYS-02**: `POST /api/v1/system/reload` collapses existing AMI reload endpoints (trunks, routes, acl, app) into one call and returns the elapsed time
- [ ] **SYS-03**: `GET /api/v1/system/info` returns version + build info from `version.rs`
- [ ] **SYS-04**: `GET /api/v1/system/config` returns a non-sensitive subset of effective `ProxyConfig` + `system_config` rows
- [ ] **SYS-05**: `GET /api/v1/system/stats` returns JSON stats derived from the existing `metrics.rs` Prometheus registry
- [ ] **SYS-06**: `GET /api/v1/system/cluster` returns a hardcoded single-node response documented as intentional

### Endpoints — SIP Listeners (LSTN)

- [ ] **LSTN-01**: `GET /api/v1/listeners` returns read-only projection of `ProxyConfig` transports (udp/tcp/tls/ws) with bind addr, port, enabled flag
- [ ] **LSTN-02**: `GET /api/v1/listeners/{name}` returns a single transport by name
- [ ] **LSTN-03**: Write attempts on listeners (`POST`/`PUT`/`DELETE`) return `501 Not Implemented` with a body explaining that multi-listener is not supported; transports are configured via settings
- [ ] **LSTN-04**: The `/api/v1/endpoints` path is reserved for SIP user-agents (Phase 13), NOT listeners — listeners use `/api/v1/listeners`

### Trunk Groups (TRK)

- [ ] **TRK-01**: A new `rustpbx_trunk_groups` table and `rustpbx_trunk_group_members` join table exist; legacy `sip_trunk` rows are untouched
- [ ] **TRK-02**: Operator can CRUD trunk groups via `/api/v1/trunks` with name, direction, distribution mode, gateway member list, credentials, acl, nofailover_sip_codes
- [ ] **TRK-03**: Creating or updating a trunk group validates that every referenced gateway exists; returns 400 on missing reference
- [ ] **TRK-04**: Deleting a trunk group is blocked with 409 if any DID or routing record references it
- [ ] **TRK-05**: Distribution modes `round_robin`, `weight_based`, `hash_callid`, `hash_src_ip`, `hash_destination` are honored in dispatch; `parallel` is feature-flagged and off by default

### Trunk Sub-Resources (TSUB)

- [ ] **TSUB-01**: Per-trunk credentials CRUD at `/api/v1/trunks/{name}/credentials` and `/api/v1/trunks/{name}/credentials/{realm}`
- [ ] **TSUB-02**: Per-trunk origination URIs CRUD at `/api/v1/trunks/{name}/origination_uris` and `/api/v1/trunks/{name}/origination_uris/{uri}`
- [ ] **TSUB-03**: Per-trunk media config (codec list, dtmf mode, srtp, media mode) GET/PUT at `/api/v1/trunks/{name}/media`
- [ ] **TSUB-04**: Per-trunk capacity (max_calls, max_cps) GET/PUT at `/api/v1/trunks/{name}/capacity`, enforced by proxy dispatch before gateway selection
- [ ] **TSUB-05**: Per-trunk ACL CRUD at `/api/v1/trunks/{name}/acl` and `/api/v1/trunks/{name}/acl/{entry}`; enforced in ingress check alongside global firewall
- [ ] **TSUB-06**: Media config filtering: if a caller SDP codec intersection with the trunk codec list is empty, the call is rejected with 488 Not Acceptable Here
- [ ] **TSUB-07**: Trunk capacity enforcement is observable via `GET /api/v1/trunks/{name}/capacity` showing current active count

### Routing (RTE)

- [ ] **RTE-01**: Operator can CRUD routing tables via `/api/v1/routing/tables`
- [ ] **RTE-02**: Operator can CRUD routing records within a table via `/api/v1/routing/tables/{name}/records` and `/records/{index}`, even though console stores records as an embedded document (adapter-only)
- [ ] **RTE-03**: `POST /api/v1/routing/resolve` dry-runs a caller/destination against the live routing engine and returns the chosen target(s) without placing a call
- [ ] **RTE-04**: Match types `Lpm`, `ExactMatch`, `Regex`, `Compare`, `HttpQuery` are all supported and covered by integration tests
- [ ] **RTE-05**: A routing table can designate a default record via `is_default: true`; resolve returns the default when no match

### Translations Engine (TRN)

- [ ] **TRN-01**: A new `rustpbx_translations` table + `models/translation.rs` entity exists
- [ ] **TRN-02**: Operator can CRUD translation classes via `/api/v1/translations` with caller/destination regex patterns, replacements, and direction (`inbound`/`outbound`/`both`)
- [ ] **TRN-03**: `proxy/translation/engine.rs` compiles and caches regex rules keyed on rule id
- [ ] **TRN-04**: Inbound call pipeline applies matching translation rules to caller and destination numbers BEFORE routing
- [ ] **TRN-05**: Translation engine honors direction filter — inbound-only rules do not fire on outbound legs
- [ ] **TRN-06**: An integration test exercises `02079460123 → +442079460123` and `4155551234 → +14155551234` end-to-end through the pipeline

### Manipulations Engine (MAN)

- [ ] **MAN-01**: A new `rustpbx_manipulations` table + `models/manipulation.rs` entity exists
- [ ] **MAN-02**: Operator can CRUD manipulation classes via `/api/v1/manipulations` with rules containing conditions (and/or), actions, and anti_actions
- [ ] **MAN-03**: Condition fields support `caller_number`, `destination_number`, `trunk`, `header:<name>`, `var:<name>`
- [ ] **MAN-04**: Action types `set_header`, `remove_header`, `set_var`, `log`, `hangup`, `sleep` are implemented
- [ ] **MAN-05**: Manipulation pipeline runs AFTER routing so rules can depend on the chosen trunk; runs before the outbound INVITE hits the wire
- [ ] **MAN-06**: `hangup` action short-circuits with a chosen SIP code and integrates cleanly with `proxy_call/session.rs` teardown
- [ ] **MAN-07**: Anti-actions fire on the else branch when condition_mode evaluates false

### Security Suite (SEC)

- [ ] **SEC-01**: Firewall store is promoted from static file-loaded CIDR to a DB-backed `rustpbx_security_rules` runtime store with `GET /api/v1/security/firewall` and `PATCH /api/v1/security/firewall`
- [ ] **SEC-02**: Flood tracker maintains a per-IP sliding window and returns 503 for incoming SIP when threshold is breached; stats queryable via `GET /api/v1/security/flood-tracker`
- [ ] **SEC-03**: Brute-force tracker records auth failures keyed on `(ip, realm)`, returns 403 after threshold, writes blocks to a new `rustpbx_security_blocks` table
- [ ] **SEC-04**: `GET /api/v1/security/blocks` lists auto-blocked IPs; `DELETE /api/v1/security/blocks/{ip}` unblocks
- [ ] **SEC-05**: `GET /api/v1/security/auth-failures` exposes recent auth failure stats
- [ ] **SEC-06**: Topology hiding (strip internal Via/Record-Route) is exposed as a config flag over existing `proxy_call/session.rs` logic, toggleable at runtime

### Active Calls & Mid-Call Control (CALL)

- [ ] **CALL-01**: Operator can list active calls via `GET /api/v1/calls` with pagination
- [ ] **CALL-02**: Operator can retrieve a single active call by id
- [ ] **CALL-03**: Operator can hangup an active call
- [ ] **CALL-04**: Operator can transfer an active call (attended and blind)
- [ ] **CALL-05**: Operator can mute and unmute a call leg
- [ ] **CALL-06**: `POST /api/v1/calls/{id}/play` plays an audio file to the call
- [ ] **CALL-07**: `POST /api/v1/calls/{id}/speak` synthesizes TTS and plays it to the call
- [ ] **CALL-08**: `POST /api/v1/calls/{id}/dtmf` transmits touch-tone digits
- [ ] **CALL-09**: `POST /api/v1/calls/{id}/record` starts recording with format (mp3/wav) + optional transcription
- [ ] **CALL-10**: Mid-call operations dispatch through the existing `active_call_registry` and `proxy_call/session.rs` path

### Webhooks (WH)

- [ ] **WH-01**: A new `rustpbx_webhooks` table + CRUD endpoints at `/api/v1/webhooks` exist
- [ ] **WH-02**: A background processor consumes `callrecord/` completion events and delivers them to registered webhooks
- [ ] **WH-03**: Webhook delivery posts JSON with HMAC header using the webhook's secret, uses 3 retries with exponential backoff, and falls back to a disk JSON file when all retries fail
- [ ] **WH-04**: Webhook events include `X-Webhook-Event`, `X-Webhook-Secret`, and a request id header
- [ ] **WH-05**: Creating a webhook fires a test event synchronously; failure to deliver the test is non-fatal and logged
- [ ] **WH-06**: `PUT /api/v1/webhooks/{id}` updates a webhook; `DELETE /api/v1/webhooks/{id}` removes it and cancels any in-flight retries

### Endpoints — SIP User Agents (EPUA)

- [ ] **EPUA-01**: Operator can create a SIP user-agent endpoint via `POST /api/v1/endpoints` with username, password, alias, and optional application reference
- [ ] **EPUA-02**: Operator can retrieve, update, delete a user-agent endpoint by id
- [ ] **EPUA-03**: Operator can list endpoints with pagination, scoped to the caller's account
- [ ] **EPUA-04**: Endpoint exposes `sip_registered` status derived from the live registrar state
- [ ] **EPUA-05**: Endpoint CRUD uses the existing `proxy/user_extension.rs` / `registrar.rs` infrastructure without requiring new proxy modules

### Applications / XML Routing (APP)

- [ ] **APP-01**: A new `rustpbx_applications` table + CRUD endpoints at `/api/v1/applications` exist
- [ ] **APP-02**: An application has `answer_url`, `hangup_url`, `message_url`, and optional auth headers
- [ ] **APP-03**: Operator can attach and detach phone numbers to an application via `POST/DELETE /api/v1/applications/{id}/numbers`
- [ ] **APP-04**: An incoming call whose routing target is an application fetches XML from the answer_url with a configurable timeout and executes the returned verbs through the existing `call/app/ivr*` runtime
- [ ] **APP-05**: Hangup events POST call completion data to the application's hangup_url
- [ ] **APP-06**: Application XML verb set includes at minimum `Play`, `Speak`, `Dial`, `Hangup`, `GetDigits`, `Record` — mapped to existing IVR runtime primitives

### Recordings First-Class (REC)

- [ ] **REC-01**: `GET /api/v1/recordings` lists recordings with filters and pagination
- [ ] **REC-02**: `GET /api/v1/recordings/{id}` returns recording metadata
- [ ] **REC-03**: `GET /api/v1/recordings/{id}/download` streams the recording file
- [ ] **REC-04**: `DELETE /api/v1/recordings/{id}` deletes a recording (file + DB row)
- [ ] **REC-05**: `POST /api/v1/recordings/export` exports multiple recordings as an archive
- [ ] **REC-06**: `DELETE /api/v1/recordings/bulk` deletes recordings matching criteria (date range, trunk, status)
- [ ] **REC-07**: Recording endpoints wrap existing `callrecord/storage.rs` and `callrecord/sipflow.rs` — no new storage layer

### Sub-Accounts & Multi-Tenancy (TEN)

- [ ] **TEN-01**: A new `rustpbx_sub_accounts` table is introduced; every existing api_v1 record defaults to a `root` account
- [ ] **TEN-02**: Operator can CRUD sub-accounts via `/api/v1/sub-accounts` with name, enabled flag, and auto-generated auth credentials
- [ ] **TEN-03**: API keys from `models/api_key.rs` gain an `account_id` column so every request resolves to an account scope
- [ ] **TEN-04**: Every api_v1 route that reads or writes account-scoped resources filters by the caller's account_id
- [ ] **TEN-05**: Master account sees all sub-accounts' resources via an explicit query parameter; sub-accounts cannot see sibling data
- [ ] **TEN-06**: The migration for sub-accounts is additive; all existing rows receive the root account_id

### Integration Tests (IT)

- [ ] **IT-01**: Every new api_v1 sub-router has a dedicated test file under `tests/` that asserts 401 without auth, happy path, 404 on missing resource, and 400/409 on bad input
- [ ] **IT-02**: Translations and Manipulations engines each have pipeline tests that place a simulated call through the dispatch path and assert rewritten numbers and mutated headers
- [ ] **IT-03**: Trunk capacity enforcement, codec filtering (488 on mismatch), and per-trunk ACL each have a proxy integration test
- [ ] **IT-04**: Applications XML answer-URL flow has an end-to-end test using a mock HTTP server returning canned XML
- [ ] **IT-05**: Sub-account isolation has a test asserting that a sub-account Bearer token cannot read or mutate another sub-account's trunk, DID, webhook, or recording

### Migration Safety (MIG)

- [ ] **MIG-01**: All new tables ship with backward-compatible migrations that run on existing databases without data loss
- [ ] **MIG-02**: Every migration has a documented rollback path (or is explicitly documented as forward-only)
- [ ] **MIG-03**: Console UI routes render identically on every page touched by a refactor (sip_trunks, dids, call_records, routing, settings, diagnostics) — verified by spot check before phase merge
- [ ] **MIG-04**: Existing `ami.rs` endpoints continue to respond until their `/api/v1/system/*` equivalents are documented as the supported surface

## v2.1 Requirements (Deferred — Production Hardening milestone)

Tracked but not in this milestone's roadmap:

### Observability (OBS)

- **OBS-01**: OpenTelemetry traces across the proxy call path
- **OBS-02**: Prometheus metrics expanded per api_v1 handler (latency histogram, error counter)
- **OBS-03**: Structured JSON logs with correlation ids
- **OBS-04**: Grafana dashboards for core SLIs

### Deployment (DEP)

- **DEP-01**: `Dockerfile.carrier` multi-stage build with minimal runtime image
- **DEP-02**: systemd unit file with health check and graceful shutdown
- **DEP-03**: Zero-downtime reload validated via test harness
- **DEP-04**: Migration runbook with go/no-go checks

### Load Testing (LOAD)

- **LOAD-01**: SIP-to-SIP relay load test sustaining 8k concurrent calls
- **LOAD-02**: SIP-to-AI agent load test sustaining 1k concurrent calls
- **LOAD-03**: API load test sustaining 1k req/s on api_v1 read paths
- **LOAD-04**: Regression baselines recorded in CI

### Hardening (HDN)

- **HDN-01**: TLS/mTLS for api_v1 with cert rotation
- **HDN-02**: Rate limiting on api_v1 itself
- **HDN-03**: Secrets management via env or Vault, not config files
- **HDN-04**: OWASP API Top 10 audit of every handler

### Documentation (DOC)

- **DOC-01**: OpenAPI 3.1 spec generated from api_v1 handlers
- **DOC-02**: Admin guide covering every endpoint group
- **DOC-03**: Tenant integration guide with code samples
- **DOC-04**: Incident response runbook

## Out of Scope

| Feature | Reason |
|---------|--------|
| Migrate storage from SQL to Redis | Breaks the "no drastic changes" constraint; SeaORM is the stable baseline |
| Multi-listener SIP with runtime endpoint CRUD | Requires refactoring `ProxyConfig` transports; write routes stay `501` |
| True multi-node clustering | Single-node deployment only; `/system/cluster` returns hardcoded single node |
| `parallel` distribution as GA | Stays feature-flagged — distinct failure semantics require dedicated soak |
| Replacing console UI | Console is stable and must keep rendering unchanged |
| Video / MCU / conferencing | Separate concerns; not in media-gateway scope |
| SMS/SMPP gateway | Separate concern |
| ENUM / number portability lookups | Add when carriers require it |
| Porting super-voice src/ Sofia/pjsip FFI | Media-gateway uses rsipstack only by design |
| Voicemail beyond existing addon | Scope creep; addons framework handles it |
| Production load testing / OpenAPI publish / Dockerfile.carrier / OTel / TLS-mTLS | Deferred to v2.1 Production Hardening milestone |

## Traceability

Populated during roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| (populated by roadmapper) | | |

**Coverage:**
- v2.0 requirements: 120 total
- Mapped to phases: 0
- Unmapped: 120 ⚠️ (roadmapper will fill)

---
*Requirements defined: 2026-04-14*
*Last updated: 2026-04-14 after initial definition*
