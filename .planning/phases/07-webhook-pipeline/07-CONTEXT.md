# Phase 7: Webhook Pipeline — Context

**Gathered:** 2026-04-25
**Status:** Ready for planning
**Source:** Discussion (13 areas batched into 4 groups; all-recommended)

<domain>
## Phase Boundary

Phase 7 ships a CRUD webhook registry plus a background processor that delivers internal events (call lifecycle, recording, transcription) to operator-registered HTTP endpoints. Delivery is HMAC-signed (Stripe-style), retried with exponential backoff + jitter, and falls back to disk-per-failure on permanent fail. A dedicated `WebhookEventSender` broadcast channel decouples emit-sites from the processor.

**Routes shipped (5 endpoints):**

| Route | Purpose | Source module |
|---|---|---|
| `GET /api/v1/webhooks` | List webhooks | NEW `src/handler/api_v1/webhooks.rs` |
| `POST /api/v1/webhooks` | Create webhook (fires synchronous test event per WH-05) | same |
| `GET /api/v1/webhooks/{id}` | Get one webhook | same |
| `PUT /api/v1/webhooks/{id}` | Replace webhook (per WH-06) | same |
| `DELETE /api/v1/webhooks/{id}` | Remove webhook + cancel in-flight retries (per WH-06) | same |

**Schema changes:**

- NEW table `supersip_webhooks` (D-00 prefix override of REQUIREMENTS.md literal `rustpbx_webhooks` — see D-01) with columns:
  - `id` (UUID v4 primary key — string format like other supersip_ tables)
  - `name` (UNIQUE, lowercase + dashes)
  - `url` (validated http/https + denylist localhost)
  - `secret` (plaintext String — consistent with Phase 3 D-03)
  - `events: Json` (array of event names; empty = subscribe-all)
  - `description: Option<String>`
  - `is_active: bool` (default true)
  - `retry_count: i32` (default 3)
  - `timeout_ms: i32` (default 5000, max 30000)
  - `created_at, updated_at: DateTimeUtc`

**New runtime infrastructure:**

- NEW broadcast channel `WebhookEventSender` (`tokio::sync::broadcast::Sender<WebhookEvent>`) — created at server boot, cloneable. Plumbed into `AppState` so source modules emit events without coupling to webhook implementation.
- NEW `src/proxy/webhook_processor.rs` — background task: subscribes to `WebhookEventSender`, loads matching webhooks from DB per event, fires HMAC-signed POSTs, retries with backoff+jitter, disk-fallback on terminal fail.
- NEW `src/proxy/webhook_signer.rs` — HMAC-SHA256 signing helper (Stripe-style canonicalized `{timestamp}.{body}`).
- NEW `src/proxy/webhook_state.rs` — `WebhookCancelRegistry: DashMap<webhook_id, CancellationToken>` for cancel-on-delete (WH-06).
- Per-event source-module instrumentation (small): emit at call lifecycle hooks, record stop, transcribe marker drop. Phase 4 D-18 marker is now consumed: when `maybe_drop_transcribe_marker` succeeds, emit `transcribe.requested` event.

**Out of scope** — explicitly deferred:

- Persistent retry queue (DB-backed retries that survive restarts) — v2.1
- Background re-attempt sweep of disk-fallback files — v2.1 (Phase 7 disk fallback is manual replay)
- Secret encrypted-at-rest — v2.1 (plaintext per Phase 3 D-03 convention)
- Per-event-type rate limiting — v2.1
- Webhook delivery metrics dashboard — Phase 11 (CDR observability)
- Bulk webhook import/export — operator uses repeated POSTs in v2.0
- Sub-account isolation on webhooks — Phase 13
- Custom HTTP methods (PUT/PATCH targets) — POST only in v2.0
</domain>

<decisions>
## Implementation Decisions (LOCKED)

### Storage shape (WH-01)

- **D-01 (table prefix override):** Table is named `supersip_webhooks`. REQUIREMENTS.md literally says `rustpbx_webhooks` but Phase 3 D-00 locked all NEW tables to `supersip_` prefix. Override REQUIREMENTS.md literal in favor of project-wide convention. Document this override in 07-SUMMARY.md.
- **D-02 (columns):** `(id, name UNIQUE, url, secret, events: Json, description, is_active, retry_count, timeout_ms, created_at, updated_at)` per the Q2 full-shape recommendation. Secret is plaintext String (D-15). Defaults: `is_active=true`, `retry_count=3`, `timeout_ms=5000`.
- **D-03 (id format):** UUID v4 string — consistent with Phase 6 routing record IDs (D-02 from Phase 6).
- **D-04 (validation):** name is lowercase + dashes (URL-safe path segment); url scheme http/https only; url denylist localhost/127.0.0.0/8/::1/fe80::/10 (NOT RFC1918 — operators legitimately webhook to internal services per Q9); timeout_ms range [100, 30000]; retry_count range [0, 10].

### Event taxonomy (WH-02)

- **D-05 (locked event names):** 6 events:
  - `call.started` — fired from `src/proxy/proxy_call/sip_session.rs` on session establishment (200 OK)
  - `call.completed` — fired from callrecord finalize (existing path; sender already exists at `src/callrecord/mod.rs:446`)
  - `call.failed` — fired from session lifecycle on early termination (3xx/4xx/5xx response or timeout)
  - `recording.completed` — fired when Phase 4's `/record` stop completes (file written to disk)
  - `transcribe.requested` — fired when Phase 4's `maybe_drop_transcribe_marker` succeeds (consumes the marker hand-off from Phase 4 D-18)
  - `webhook.test` — synthetic event fired on POST `/api/v1/webhooks` (WH-05)
- **D-06:** All 6 events emit-and-deliver in Phase 7. `call.started`/`call.failed` instrumented in `sip_session.rs`; `recording.completed` in the record route stop handler; `transcribe.requested` in `maybe_drop_transcribe_marker`.
- **D-07 (envelope shape):** Stripe-style envelope:
  ```json
  {
    "event_id": "evt_<uuid>",
    "event": "call.completed",
    "timestamp": 1714060800,
    "data": { /* event-specific JSON */ }
  }
  ```
  - `call.completed.data` = existing `CallRecord` JSON (reuse Phase 1 storage shape)
  - `call.started.data` = `{session_id, caller_number, destination_number, started_at, direction}`
  - `call.failed.data` = `{session_id, caller_number, destination_number, failure_reason, sip_code}`
  - `recording.completed.data` = `{session_id, recording_path, format, duration_secs, size_bytes}`
  - `transcribe.requested.data` = `{session_id, recording_path, marker_path}`
  - `webhook.test.data` = `{webhook_id, message: "Test event from supersip"}`

### Per-webhook event filtering (WH-02)

- **D-08:** Webhook row has `events: Vec<String>` column. Empty list = subscribe-all (operator-friendly default). Non-empty = fire only for events in the list.
- **D-09:** Validation at PUT/POST: each entry must match the locked event-name set (D-05). Unknown events → 400 with `code: "bad_request"` and message listing valid events.
- **D-10:** Filter check at fire time: processor computes `webhook.events.is_empty() || webhook.events.contains(&event.event_name)` before queueing delivery.

### CallRecord → webhook adapter (WH-02)

- **D-11:** Dedicated `WebhookEventSender = tokio::sync::broadcast::Sender<WebhookEvent>` channel created at server boot in `src/proxy/server.rs`. Stored in `SipServer.inner` alongside `active_call_registry`. Cloneable — emit sites use `state.webhook_sender().send(event)`.
- **D-12:** Webhook processor task spawns at server boot. Subscribes once. On each received event:
  1. Compute envelope (D-07)
  2. Query `supersip_webhooks WHERE is_active=true` (fresh DB read per event — Phase 5 D-17 / Phase 6 D-29 pattern; cache is v2.1)
  3. Filter by event name (D-10)
  4. For each matching webhook, spawn delivery task (per-webhook isolation; one slow webhook doesn't block others)
- **D-13:** Decoupling rationale: source modules (`sip_session.rs`, callrecord, record handler) emit via the broadcast channel. They don't import webhook concepts. The webhook processor is the sole subscriber in v2.0; v2.1 may add metrics/audit subscribers without changing emit sites.

### HMAC signing + headers (WH-03, WH-04)

- **D-14 (HMAC):** HMAC-SHA256, hex-encoded. Signed payload format: `"{timestamp}.{body}"` (Stripe-style; replay-resistant).
- **D-15 (signature header):** `X-Webhook-Signature: t={timestamp},v1={hex_sha256}` — receiver verifies timestamp window (recommend ±5min) AND HMAC. NOT mutually exclusive with the literal WH-04 headers.
- **D-16 (literal WH-04 headers, kept for spec parity):**
  - `X-Webhook-Event: <event_name>` (e.g., `call.completed`)
  - `X-Webhook-Secret: <plaintext-secret>` — per literal WH-04. Note: sending plaintext secret is non-standard and a leak risk via HTTP logs. Documented as known weakness; v2.1 may deprecate in favor of signature-only.
  - `X-Webhook-Request-Id: <uuid>` — per-delivery UUID, also used for idempotency by receiver
- **D-17 (Content-Type):** `application/json; charset=utf-8`
- **D-18 (User-Agent):** `supersip/<version>` (read from cargo pkg version)

### Retry policy (WH-03)

- **D-19 (schedule):** `[1s, 5s, 30s]` for 3 retries. Total window ~36s. Jitter ±25% on each delay (e.g., 1s → uniform random in [0.75s, 1.25s]). Per-webhook `retry_count` overrides default 3 (range 0-10 per D-04).
- **D-20 (per-attempt timeout):** Per-webhook `timeout_ms` column (default 5000ms, max 30000ms per D-04).
- **D-21 (status code policy):**
  - 2xx → success (stop)
  - 3xx → follow up to 5 hops automatically (reqwest default), final 2xx → success
  - 4xx (except 408/429) → permanent fail (no retry; immediate disk fallback — operator config error like wrong URL)
  - 408 (timeout), 429 (rate limit), 5xx, network error → retry
- **D-22 (`Retry-After` honored):** On 429 response, respect `Retry-After` header (seconds) if present and ≤ next scheduled delay; otherwise use scheduled delay.

### Disk fallback (WH-03)

- **D-23 (path):** `{Config.generated_dir}/webhooks/failed/{timestamp}-{webhook_id}-{event_id}.json` — file-per-failure (Q8 option B). Easy to delete individually after manual replay.
- **D-24 (file content):** Full envelope (D-07) + delivery metadata:
  ```json
  {
    "envelope": { /* Stripe-style envelope */ },
    "webhook_id": "<uuid>",
    "webhook_url": "<url at fail time>",
    "attempts": [
      {"timestamp": ..., "status": 502, "error": null, "duration_ms": 1234},
      {"timestamp": ..., "status": null, "error": "connection refused", "duration_ms": 5000}
    ],
    "first_attempt_at": ...,
    "final_failure_at": ...
  }
  ```
- **D-25 (replay):** Manual operator action in Phase 7 — `cat <file> | jq .envelope | curl ...` style. Background re-attempt sweep is v2.1.

### SSRF defense (Q9)

- **D-26 (write-time validation only):** Scheme http/https check + localhost denylist (D-04). Operators are trusted (webhook URL is explicit setup, not call-time input). Runtime DNS-rebind check is NOT done here (Phase 6 HttpQuery had a per-call SSRF risk; webhook URLs are operator-config, lower-risk).
- **D-27 (denylist scope):** localhost, 127.0.0.0/8, ::1, fe80::/10 only. RFC1918 (10/8, 172.16/12, 192.168/16) ALLOWED — operators legitimately webhook to internal services (e.g., k8s service DNS).

### Test event on create (WH-05)

- **D-28 (synchronous fire on POST):** POST `/webhooks` creates the row, then synchronously fires `webhook.test` event with synthetic payload `{webhook_id, message: "Test event from supersip"}`. Blocks response up to webhook's `timeout_ms`.
- **D-29 (response on test failure):** Per WH-05 "non-fatal": webhook is created (row persists), POST returns **201 Created** with body containing the webhook + `{"test_delivery": "succeeded" | "failed", "test_error": "<msg if failed>"}`. Operator can debug via the response.
- **D-30 (test event respects HMAC + headers):** Identical signing/headers to real fires (D-14..D-18). Receivers can use the test to verify their HMAC implementation before going live.

### Cancel-on-delete (WH-06)

- **D-31 (in-memory cancel registry):** `WebhookCancelRegistry: DashMap<webhook_id (String), CancellationToken>`. Each delivery task acquires its webhook's CancellationToken on spawn; `select!` between delivery + token. DELETE triggers `token.cancel()`.
- **D-32 (pre-flight DB recheck):** Before each retry attempt (after a backoff sleep), processor re-fetches the webhook row by id. If missing or `is_active=false`, abort retry (no disk fallback either — operator explicitly killed it).
- **D-33 (in-memory only):** Pending retries are in-memory only in Phase 7. Server restart loses any in-flight retries (they're written to disk fallback only after all retries exhausted — which won't happen if killed mid-retry by restart). Persistent retry queue is v2.1.
- **D-34 (cleanup):** On webhook PUT or DELETE, the cancel registry entry for that webhook_id is replaced (PUT) or removed (DELETE). PUT-then-cancel-prior-retries: PUT triggers cancel of any in-flight retries (config changed; old payload doesn't match new url/secret). Documented behavior.

### Secret storage (Q12)

- **D-35:** Plaintext String — consistent with Phase 3 D-03 (credentials plaintext). HMAC verification requires plaintext access. Encrypted-at-rest is a v2.1 hardening concern (whole-DB encryption is the operator's deployment job).

### Wire types (skeleton — planner finalizes)

```rust
// webhooks.rs
#[derive(Serialize, Deserialize)]
pub struct WebhookView {
    pub id: String,
    pub name: String,
    pub url: String,
    pub secret: String,           // plaintext per D-35
    pub events: Vec<String>,
    pub description: Option<String>,
    pub is_active: bool,
    pub retry_count: i32,
    pub timeout_ms: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct CreateWebhookRequest {
    pub name: String,
    pub url: String,
    pub secret: String,
    pub events: Option<Vec<String>>,        // default: subscribe-all (empty)
    pub description: Option<String>,
    pub is_active: Option<bool>,            // default true
    pub retry_count: Option<i32>,           // default 3
    pub timeout_ms: Option<i32>,            // default 5000
}

#[derive(Serialize)]
pub struct CreateWebhookResponse {
    #[serde(flatten)]
    pub webhook: WebhookView,
    pub test_delivery: String,              // "succeeded" | "failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_error: Option<String>,
}

// webhook_processor.rs (internal types)
#[derive(Clone, Debug)]
pub struct WebhookEvent {
    pub event_id: String,                   // evt_<uuid>
    pub event: String,                      // "call.completed" etc.
    pub timestamp: i64,                     // unix seconds
    pub data: serde_json::Value,
}
```

### Router wiring

`src/handler/api_v1/mod.rs` (Wave 1 owns):

```rust
pub mod webhooks;  // NEW

let protected: Router<AppState> = Router::new()
    .merge(/* ...existing... */)
    .merge(webhooks::router());
```

### Migration registration order

`src/models/migration.rs::Migrator::migrations` appends:

```rust
Box::new(super::webhooks::Migration),  // create supersip_webhooks
```

### Test convention (IT-01)

- `tests/api_v1_webhooks.rs` — CRUD: 401, list, POST happy + test-event-success, POST happy + test-event-fail (returns 201 with `test_delivery: failed`), POST duplicate-name 409, POST invalid-url 400 (localhost), POST invalid-event 400, GET happy/missing-404, PUT happy/missing-404, DELETE happy/missing-404, DELETE cancels in-flight retries
- `tests/proxy_webhook_pipeline.rs` — IT-WH end-to-end: emit `call.completed` event → webhook fires with HMAC + headers; retry 3 times on 502 → disk fallback file written; retry on 429 honors Retry-After; permanent fail on 400 (no retry); cancel-on-delete aborts in-flight retry; PUT cancels prior retry; transcribe.requested event fires when Phase 4 marker drops

### Claude's Discretion

- Exact reqwest version reuse (likely already a dep via Phase 6 HttpQuery; verify in research)
- Hex encoding crate (`hex` is workspace dep already? probably — verify)
- HMAC crate (`hmac` + `sha2` — standard)
- Whether `WebhookCancelRegistry` is on `AppState` or on the processor task — recommend AppState so handlers can DELETE-trigger cancel
- Channel buffer size for `WebhookEventSender` broadcast — recommend 1024 (matches locator_webhook pattern); document tradeoff
- Whether to consolidate `webhook_processor.rs` + `webhook_signer.rs` + `webhook_state.rs` into a single `src/proxy/webhook/` module directory — recommend yes for organization
- Whether `webhook.test` payload includes the webhook's URL or just `webhook_id` — recommend `webhook_id` only (URL is operator's own config)
- Disk fallback directory creation: lazy on first failure or eager on startup — recommend lazy (no failure = no dir)
- Whether to log full request body in disk fallback file — yes (for replay); document size implications
- File mode for disk-fallback files — 0600 (owner read/write only) — secrets are in HMAC headers, not body, but defense in depth
</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project specs
- `.planning/REQUIREMENTS.md` §WH — WH-01..06 acceptance criteria (note: WH-01 says `rustpbx_webhooks`, overridden to `supersip_webhooks` per D-01)
- `.planning/ROADMAP.md` — Phase 7 success criteria (4 must-be-true items)

### Phase hand-offs
- `.planning/phases/04-active-calls-mid-call-control/04-CONTEXT.md` §D-18 — `transcribe.marker` sidecar contract; Phase 7 consumes via `transcribe.requested` event (D-05/D-06)
- `.planning/phases/03-trunk-sub-resources-l1-and-routing-resolve/03-CONTEXT.md` §D-00 — `supersip_` prefix lock-in; §D-03 — plaintext secret convention (Phase 7 D-35)
- `.planning/phases/06-routing-tables-records-distribution/06-CONTEXT.md` §SSRF defense pattern — Phase 7 reuses scheme + denylist approach (D-04, D-26)
- `.planning/phases/05-trunk-enforcement-capacity-acl-codec-filter/05-CONTEXT.md` §D-17 — fresh DB read per event (Phase 7 D-12 reuses)

### Existing code (read before designing)
- `src/proxy/locator_webhook.rs` — REFERENCE PATTERN: existing reqwest webhook with broadcast channel; Phase 7 emulates the architecture
- `src/callrecord/mod.rs:446` — `CallRecordSender = mpsc::UnboundedSender<...>`; Phase 7 does NOT subscribe here directly (uses dedicated WebhookEventSender per D-11)
- `src/callrecord/storage.rs` — call completion path; emit hook for `call.completed` event lives nearby
- `src/proxy/proxy_call/sip_session.rs` — session lifecycle hooks for `call.started`/`call.failed`
- `src/handler/api_v1/calls.rs` — Phase 4 `/record` stop handler — emit hook for `recording.completed`
- `src/handler/api_v1/calls.rs` `maybe_drop_transcribe_marker` — Phase 4 transcribe marker drop site; emit `transcribe.requested` here per D-06
- `src/config.rs:780` — `Config::generated_dir` (existing String field); Phase 7 disk fallback path uses this
- `src/handler/api_v1/trunks.rs` — CRUD pattern reference
- `src/handler/api_v1/trunk_capacity.rs` (Phase 5) and `routing_tables.rs` (Phase 6) — recent CRUD/stub-router pattern references
- `src/proxy/server.rs:564` — server boot; Phase 7 `WebhookEventSender` constructed here alongside `active_call_registry`

### External crates (research will fetch via context7)
- `reqwest` — HTTP client (already dep; reuse with timeout/retry)
- `hmac` + `sha2` — HMAC-SHA256 implementation
- `hex` — hex encoding
- `tokio::sync::broadcast` — event channel (already used in locator_webhook)
- `tokio_util::sync::CancellationToken` — cancel-on-delete (already used elsewhere in codebase)
- `uuid` — event_id generation
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- **`src/proxy/locator_webhook.rs`** — full reference pattern for HTTP webhook delivery via reqwest + broadcast channel. Phase 7 mirrors this architecture but adds HMAC + retries + disk fallback.
- **`Config::generated_dir`** — already exists as a String field at `src/config.rs:780`. Disk fallback at `{generated_dir}/webhooks/failed/`.
- **`tokio::sync::broadcast`** — already used in locator_webhook; Phase 7 emulates with `WebhookEventSender`.
- **`CallRecordSender`** at `src/callrecord/mod.rs:446` — Phase 7 does NOT subscribe directly. Instead, the callrecord finalize path emits a `call.completed` event via `WebhookEventSender` (decoupling per D-11/D-13).
- **Phase 6 SSRF write-time validation** — Phase 7 reuses scheme + denylist patterns from `src/handler/api_v1/routing_records.rs` (validate URL helper).

### Established Patterns
- **CRUD sub-resource pattern** — Phase 5/6 trunk_capacity, trunk_acl, routing_tables, routing_records all share the same handler/test/router-merge pattern
- **Wave 1 owns mod.rs registration** — Phase 5/6 lesson; Wave 1 (07-01) lands the migration + stub router; downstream waves don't touch mod.rs
- **Plaintext secrets** — Phase 3 D-03 + Phase 7 D-35
- **Stable UUID IDs for sub-resources** — Phase 6 D-02 (record_id) → Phase 7 webhook id
- **HMAC-SHA256 with hex encoding** — industry standard; Stripe pattern is the cleanest UX

### Integration Points
- `src/proxy/server.rs:564` — construct `WebhookEventSender` here, plumb into `AppState`
- `src/proxy/proxy_call/sip_session.rs` — add 2 emit sites (call started, call failed)
- `src/callrecord/storage.rs` — add 1 emit site (call completed)
- `src/handler/api_v1/calls.rs` — add 2 emit sites (recording stop, transcribe marker drop)
- `src/handler/api_v1/mod.rs` — Wave 1 router merge (single edit, then frozen)
- `src/models/migration.rs` — Wave 1 appends 1 migration
- `tests/common/mod.rs` — extend with `seed_webhook` and `start_test_webhook_server` (axum mock for IT)
</code_context>

<specifics>
## Specific Ideas

- **Webhook test event** is the FIRST production-shape webhook a receiver gets — they can use it to validate their HMAC verification before going live (D-30)
- **Stripe-style signature** is replay-resistant via timestamp window — receivers should reject signatures > 5min old (documented in operator deploy guide)
- **`X-Webhook-Secret` plaintext header is documented as known weakness** — kept for literal WH-04 spec parity, but receivers should ignore it in favor of `X-Webhook-Signature` (D-16). v2.1 deprecates.
- **Disk fallback file mode 0600** — owner read/write only; defense in depth (HMAC header is the secret leak surface, not the body)
- **`call.completed.data` reuses existing `CallRecord` JSON shape** — receivers parse the same structure as `callrecord/` directory (Phase 1 storage). No translation layer.
- **`transcribe.requested` is the contract Phase 4 D-18 promised** — Phase 4's `transcribe.marker` sidecar is now consumed; receivers can subscribe to this event to trigger their own transcription pipeline
- **Per-webhook `retry_count` and `timeout_ms` allow operator tuning** without redeploy — slow integrations can set `timeout_ms: 30000`; flaky integrations can set `retry_count: 10`
- **Empty events list = subscribe-all** is operator-friendly default (most ops want all events; opt-out is rarer than opt-in)
- **PUT cancels prior retries** (D-34) — webhook config changed (URL, secret, events filter), old payload no longer matches new contract
</specifics>

<deferred>
## Deferred Ideas

- **Persistent retry queue** (DB-backed retries surviving restarts) — v2.1
- **Background re-attempt sweep** of disk-fallback files — v2.1; Phase 7 is manual replay
- **Encrypted-at-rest secrets** — v2.1 (whole-DB encryption is operator deployment concern)
- **Per-event-type rate limiting** — v2.1 (e.g., burst-protect a webhook from a call-storm)
- **Webhook delivery metrics dashboard** — Phase 11 CDR observability
- **Bulk webhook import/export** — v2.1; v2.0 uses repeated POSTs
- **Sub-account isolation on webhooks** — Phase 13 (sub-accounts revisits all RBAC)
- **Custom HTTP methods** (PUT/PATCH instead of POST) — v2.1; v2.0 is POST only
- **Webhook templating** (e.g., per-webhook payload transformation) — v2.1
- **Webhook health checks** (periodic test fires; auto-disable on N consecutive failures) — v2.1
- **Streaming events** (WebSocket/SSE webhooks for real-time consumers) — out of v2.0
- **Hot-reload of webhook config** mid-flight retry — handled implicitly by D-32 pre-flight recheck
- **Custom HMAC algorithms** beyond SHA-256 — v2.1
- **Replay protection on receiver side** (idempotency keys via `X-Webhook-Request-Id`) — receiver concern; documented in operator guide but not enforced server-side
</deferred>

---

*Phase: 07-webhook-pipeline*
*Context gathered: 2026-04-25*
