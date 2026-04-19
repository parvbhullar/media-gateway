# Phase 4: Active Calls & Mid-Call Control — Context

**Gathered:** 2026-04-19
**Status:** Ready for planning
**Source:** Discussion + Phase 3 hand-off + CARRIER-API spec + existing dispatch infra audit

<domain>
## Phase Boundary

Phase 4 exposes the live `ActiveProxyCallRegistry` as a read surface and ships **12 mid-call REST endpoints** on top of the already-built unified command dispatch path (`dispatch_console_command` → `console_to_call_command` → `CallCommand` → `SipSessionHandle::send_command`). Zero new proxy modules. Zero new tables.

**Routes shipped (12 total):**

| Route | Purpose | Underlying CallCommand | Requirement |
|---|---|---|---|
| `GET /api/v1/calls` | List active calls (paginated + filtered) | — | CALL-01 |
| `GET /api/v1/calls/{id}` | Call detail (meta + SessionSnapshot) | — | CALL-02 |
| `POST /api/v1/calls/{id}/hangup` | Hang up the call | `Hangup(HangupCommand)` | CALL-03 |
| `POST /api/v1/calls/{id}/transfer` | Blind or attended transfer (start) | `Transfer{attended}` | CALL-04 |
| `POST /api/v1/calls/{id}/transfer/complete` | Commit attended transfer | `TransferComplete{consult_leg}` | CALL-04 |
| `POST /api/v1/calls/{id}/transfer/cancel` | Roll back attended transfer | `TransferCancel{consult_leg}` | CALL-04 |
| `POST /api/v1/calls/{id}/mute` | Mute a call leg | `MuteTrack{track_id}` | CALL-05 |
| `POST /api/v1/calls/{id}/unmute` | Unmute a call leg | `UnmuteTrack{track_id}` | CALL-05 |
| `POST /api/v1/calls/{id}/play` | Play audio file or URL | `Play{source,options}` | CALL-06 |
| `POST /api/v1/calls/{id}/speak` | Synthesize and play TTS | `Play{source:Tts{text,voice}}` | CALL-07 |
| `POST /api/v1/calls/{id}/dtmf` | Send touch-tone digits | `SendDtmf{leg_id,digits}` | CALL-08 |
| `POST /api/v1/calls/{id}/record` | Start recording (wav/mp3 + optional transcribe flag) | `StartRecording{config}` | CALL-09 |

**Schema changes:** none. Phase 4 is stateless REST over the in-memory registry.

**Proxy changes:** one small additive extension to `CallCommand::SendDtmf` (see D-13), plus a relocation of `CallCommandPayload` out of `src/console/handlers/call_control.rs` into a neutral module (see D-11).

**Out of scope for Phase 4** — explicitly deferred:

- Transcription post-processing (Phase 7 webhooks consume the `transcribe:true` flag) — D-18
- Recording download / metadata API (Phase 12 Recordings — REC-01..07)
- Hold / unhold routes (not in CALL-01..10; add in Phase 7 or later if carrier asks)
- Conference routes (ConferenceCreate/Add/etc. exist in `CallCommand` but are out of scope — Phase 13)
- Supervisor routes (SupervisorListen/Whisper/Barge — out of scope for v2.0)
- Transfer state polling endpoint (GET /transfer) — deferred; clients track via hangup webhooks
</domain>

<decisions>
## Implementation Decisions (LOCKED)

### Naming & placement

- **D-00:** New sub-router lives at `src/handler/api_v1/calls.rs` (NEW). Wired into `src/handler/api_v1/mod.rs::protected` router via `.merge(calls::router())` per Phase 1 SHELL-01.
- **D-00b:** `tests/api_v1_calls.rs` (NEW) follows the IT-01 matrix: auth, happy path, 404, 400, 409 (plus command-specific CommandResult failure paths).

### List / get shape & pagination (CALL-01, CALL-02)

- **D-01:** **Pagination uses the Phase 1 `Pagination` extractor** (`page`, `page_size`, defaults 1 / 20). Handler reads `registry.list_recent(usize::MAX)` once, sorts by `started_at desc`, applies filters, then slices `[offset..offset+page_size]` and wraps in `PaginatedResponse<ActiveCallView>`. No changes to `ActiveProxyCallRegistry`. `total_count` equals the filtered length pre-slice.
- **D-02:** URL path parameter `{id}` is the registry's native **`session_id` string verbatim** (e.g., `sess_abc123`). Session IDs are UUID-ish — no special encoding required. This matches how `dispatch_console_command` is keyed. SIP Call-ID is NOT exposed in the URL.
- **D-03:** `ActiveCallView` is **rich on both list and get-by-id** — carries `ActiveProxyCallEntry` fields (`session_id`, `caller`, `callee`, `direction`, `started_at`, `answered_at`, `status`) plus `SessionSnapshot` (negotiated codec, media mode, recording state, mute state) resolved via `handle.snapshot()`. Wire shape:
  ```json
  {
    "session_id": "sess_abc123",
    "caller": "+14155551234",
    "callee": "+442079460123",
    "direction": "outbound",
    "started_at": "2026-04-19T12:34:56Z",
    "answered_at": "2026-04-19T12:34:58Z",
    "status": "talking",
    "snapshot": { /* SessionSnapshot JSON */ }
  }
  ```
  If `registry.get_handle(session_id)` returns `None` when resolving snapshot for a row (TOCTOU race), the view drops `snapshot` rather than 500ing — the row is simply projected without it.
- **D-04:** **Filters supported on GET /api/v1/calls:** `status=ringing|talking`, `direction=inbound|outbound`, `caller=<substring>`, `callee=<substring>`, `since=<RFC-3339 timestamp>`. All query params are optional. `caller` and `callee` use case-insensitive substring match. `since` filters to rows where `started_at >= since`. Invalid `status`, `direction`, or unparseable `since` → 400 with reason.
- **D-05:** Default sort is `started_at desc`. No client sort knob in Phase 4.

### Dispatch integration (CALL-10)

- **D-06:** **api_v1 reuses `dispatch_console_command` verbatim.** Handler converts the parsed request body into the shared payload enum, calls `dispatch_console_command(&registry, session_id, payload)`, then maps `CommandResult` → HTTP status per D-08.
- **D-07:** `CommandResult` → HTTP mapping:

  | CommandResult | HTTP | Body |
  |---|---|---|
  | `success` | 200 | `{"message":"dispatched"}` |
  | `!success` with `"not found"` in message | 404 | `{"error":"call not found","code":"not_found"}` |
  | `!success` where command was not-supported (MediaCapabilityCheck::Denied) | 400 | `{"error":<reason>,"code":"bad_request"}` |
  | `!success` with `"failed to dispatch"` (mpsc send error) | 409 | `{"error":"command dispatch failed: <underlying>","code":"conflict"}` |
  | anyhow error from dispatch | 500 | `{"error":<message>,"code":"internal"}` |

- **D-08:** Registry lookup precedes dispatch: handler calls `registry.get_handle(session_id)` first and returns 404 before handing off, so "call not found" is always a clean 404 and not a dispatch-level failure.

### Mute / unmute (CALL-05)

- **D-09:** API accepts `{leg: "caller" | "callee"}`. Handler resolves `leg` → `track_id` via `handle.snapshot()` (which exposes per-leg track identifiers). Resolved `track_id` feeds into `CallCommand::MuteTrack{track_id}` / `CallCommand::UnmuteTrack{track_id}`. If the snapshot is unavailable (e.g., call still ringing, media not negotiated) → 409 `conflict` with `{"error":"media tracks not yet established"}`.
- **D-09b:** Body is required (`{leg}` mandatory). Omitting `leg` → 400. No "mute both" shortcut in Phase 4 — explicit calls per leg.

### CallCommandPayload relocation (CALL-10, SHELL-04)

- **D-10:** `CallCommandPayload` (currently in `src/console/handlers/call_control.rs`) **moves to `src/call/runtime/command_payload.rs`** (NEW module, neutral location). Both console and api_v1 import from there. `console_to_call_command` adapter stays in `src/call/adapters/console_adapter.rs` — its input type alias updates to the new path.
- **D-11:** Payload variants extend from the console's existing 5 (`Hangup`, `Accept`, `Transfer`, `Mute`, `Unmute`) to add:
  - `BlindTransfer { target: String, leg: Option<Leg> }`
  - `AttendedTransferStart { target: String, leg: Option<Leg> }` → returns `consult_leg_id`
  - `AttendedTransferComplete { consult_leg: String }`
  - `AttendedTransferCancel { consult_leg: String }`
  - `Play { source: PlaySource, leg: Option<Leg>, options: Option<ApiPlayOptions> }`
  - `Speak { text: String, voice: Option<String>, leg: Option<Leg> }`
  - `Dtmf { digits: String, duration_ms: Option<u32>, inter_digit_ms: Option<u32>, leg: Option<Leg> }`
  - `Record { path: Option<String>, format: Option<String>, beep: Option<bool>, max_duration_secs: Option<u32>, transcribe: Option<bool> }`

  Existing console-only variants (`Accept`, console's single `Transfer`, raw `Mute{track_id}`, raw `Unmute{track_id}`) stay untouched so console callers continue to work. The console adapter picks up the new API variants but doesn't have to consume them — it only needs to not break.
- **D-11b:** `Leg` enum: `{Caller, Callee}` serialized as lowercase strings. Defined in `command_payload.rs`.

### Play (CALL-06)

- **D-12:** Wire shape:
  ```json
  {
    "source": {"file": "/var/lib/supersip/audio/hold.wav"}  // or {"url": "https://..."}
    "leg": "callee",            // optional, default both (None)
    "loop": false,              // optional
    "interrupt_on_dtmf": true   // optional
  }
  ```
  `source` is a tagged enum `{file: String}` or `{url: String}` — rejects other variants from `MediaSource` (no `Silence`, `Tone`, `Tts` here — `Tts` goes through `/speak`). Maps to `CallCommand::Play{leg_id, source: MediaSource::File|Url, options: PlayOptions{loop_playback, interrupt_on_dtmf, await_completion:false, track_id:None, send_progress:false}}`.
- **D-12b:** URL fetching uses whatever the existing `FileTrack`/`RtpTrackBuilder` chain does for `MediaSource::Url`. If URL playback isn't wired, `MediaCapabilityCheck::Denied` surfaces → 400.

### Speak (CALL-07) — TTS passthrough

- **D-13:** Wire shape: `{text: String, voice?: String, leg?: "caller"|"callee"}`. Handler builds `CallCommand::Play{leg_id, source: MediaSource::Tts{text, voice}}`. If the TTS engine isn't wired in the live session pipeline, the existing `MediaCapabilityCheck::Denied` path returns `not_supported` → 400 with the reason. **No runtime probe before dispatch** — let the dispatch layer's capability check do it.

### DTMF (CALL-08)

- **D-14:** Wire shape: `{digits: String, leg?: "caller"|"callee", duration_ms?: u32, inter_digit_ms?: u32}`. Default target leg is **callee** for outbound calls, **caller** for inbound — handler resolves from `entry.direction`. Digits accept `0-9A-D*#`; other chars → 400.
- **D-14b:** `CallCommand::SendDtmf` currently has only `{leg_id, digits}`. **Additive change:** extend `SendDtmf` with `duration_ms: Option<u32>` and `inter_digit_ms: Option<u32>` fields. Console adapter passes `None` for both. `session.rs`/`sip_session.rs` consumer reads them if present, falls back to existing defaults otherwise. **Scope note:** only extend the CallCommand struct in Phase 4; actually honoring the timing overrides in the SIP layer can follow in a later hardening pass if the default DTMF timing is sufficient — flag this in the plan.

### Record (CALL-09)

- **D-15:** Wire shape:
  ```json
  {
    "path": null,                 // optional — server auto-generates if null
    "format": "wav",              // optional — wav|mp3, default wav
    "beep": false,                // optional
    "max_duration_secs": 3600,    // optional
    "transcribe": false           // optional — flag only, see D-18
  }
  ```
  Handler builds `CallCommand::StartRecording{config: RecordConfig{path, max_duration_secs, beep, format: Some(fmt)}}`.
- **D-16:** **Auto-generated path:** when `path` is null, handler generates `<RECORDINGS_DIR>/<session_id>-<unix_ts>.<ext>`. `RECORDINGS_DIR` resolves from `ProxyConfig::recording_dir` (or equivalent — **Claude's Discretion to locate the right config field during planning**). Response body returns `{"message":"dispatched","path":"<resolved-path>"}` so the client knows where the file will land. Auto-generate is the recommended path; explicit paths are accepted but validated to be inside the recording dir tree (no `..` traversal, must be absolute, parent dir must exist).
- **D-17:** Format validation in Phase 4: `format` must be `wav` or `mp3` if present — reject other values with 400. The underlying `RecordConfig.format` is `Option<String>` and the session layer already handles the extension mapping.
- **D-18:** `transcribe: true` is accepted and **stored on a metadata sidecar** (`<RECORDINGS_DIR>/<session_id>-<unix_ts>.transcribe.marker` — simple empty file next to the recording path). **No transcription happens in Phase 4.** Phase 7 (Webhooks) consumes this marker when it processes `callrecord/` completion events and triggers transcription then. If no transcription infra exists in Phase 7 either, the marker sits until whichever phase wires it. Document the marker contract in `src/handler/api_v1/calls.rs` module doc comment.

### Transfer (CALL-04)

- **D-19:** Single endpoint `POST /api/v1/calls/{id}/transfer` with tagged body:
  ```json
  {"type": "blind", "target": "sip:...", "leg": "callee"}
  {"type": "attended", "target": "+14155551234", "leg": "callee"}
  ```
  Handler:
  - `type:"blind"` → `CallCommand::Transfer{leg_id, target, attended:false}` → 200 on dispatch
  - `type:"attended"` → `CallCommand::Transfer{leg_id, target, attended:true}` → 200 with `{"consult_leg_id":"<id>"}` in the body
- **D-20:** Attended transfer **full flow ships in Phase 4**: `POST /api/v1/calls/{id}/transfer/complete` with `{consult_leg_id}` body, `POST /api/v1/calls/{id}/transfer/cancel` with `{consult_leg_id}` body. Each maps to `CallCommand::TransferComplete{consult_leg}` / `CallCommand::TransferCancel{consult_leg}`. Unknown `consult_leg_id` → 404.
- **D-21:** **Default leg = callee.** Optional `{leg: "caller"|"callee"}` overrides. Resolved to `LegId` by the handler before dispatch.
- **D-22:** **Target normalization:** accept either a SIP URI (`sip:...` or `sips:...`) or a bare E.164 number (`+14155551234`). Handler normalizes bare numbers to `sip:<number>@<configured_transfer_host>` before dispatch. Configured host is `ProxyConfig::external_ip` or equivalent — **Claude's Discretion to pick the right field** during planning; fall back to the first UDP transport bind IP if no dedicated config exists. Anything that doesn't parse as either → 400. Match the existing `transfer_to_uri` behavior in [src/proxy/proxy_call/session.rs:2410](src/proxy/proxy_call/session.rs#L2410).

### View types (SHELL-04)

```rust
// src/handler/api_v1/calls.rs
#[derive(Serialize)]
pub struct ActiveCallView {
    pub session_id: String,
    pub caller: Option<String>,
    pub callee: Option<String>,
    pub direction: String,
    pub started_at: DateTime<Utc>,
    pub answered_at: Option<DateTime<Utc>>,
    pub status: String,            // "ringing" | "talking"
    pub snapshot: Option<SessionSnapshot>,  // None if handle gone (TOCTOU) or serialization fails
}

#[derive(Deserialize)]
pub struct CallListQuery {
    pub status: Option<String>,
    pub direction: Option<String>,
    pub caller: Option<String>,
    pub callee: Option<String>,
    pub since: Option<DateTime<Utc>>,
}
```

### Integration test convention (IT-01)

- `tests/api_v1_calls.rs` — dedicated test file. Matrix covers:
  - 401 without Bearer on every route
  - Happy path list + paginated list + filtered list
  - GET /calls/{id} with rich snapshot; GET /calls/{unknown} → 404
  - Each command route: happy dispatch + 404 on unknown session + 400 on malformed body + 409 when dispatch mpsc fails (simulate by dropping the receiver)
  - Mute/unmute with `leg` param; 409 when snapshot has no tracks yet
  - Transfer blind happy; transfer attended returns consult_leg_id; complete/cancel happy + 404 on unknown consult_leg
- **Fixture strategy:** tests seed `ActiveProxyCallRegistry` directly via `register_handle(session_id, SipSessionHandle::with_handle(id).0)` exactly like `src/proxy/active_call_registry.rs::tests::make_handle`. The command-receiving end stays in `_cmd_rx` so dispatches succeed but no real SIP flow runs. For "dispatch mpsc fails" assertions, drop `_cmd_rx` explicitly before dispatching.

### Claude's Discretion

- Exact field name for `RECORDINGS_DIR` in `ProxyConfig` — find it during planning (likely `proxy.recording_dir` or `callrecord.storage_path`).
- Exact field name for `configured_transfer_host` in `ProxyConfig` — likely `proxy.external_ip`, else first UDP transport bind.
- Whether `ActiveCallView.status` serializes as lowercase string (`"ringing"`, `"talking"`) or enum — recommend lowercase string for wire stability.
- Whether to wrap the rich `SessionSnapshot` in a nested `"snapshot"` field or inline its fields at the top level — recommend nested for cleaner SHELL-04 typing.
- Transcribe marker filename convention (`<path>.transcribe.marker` vs separate `<recordings>/transcribe-queue/<session_id>.marker`) — recommend sibling marker for locality.
- Whether dispatch path needs any lock held while reading `handle.snapshot()` to avoid races — recommend not; snapshot is already internally locked.
- When `SendDtmf` extension fields (D-14b) are plumbed end-to-end vs wire-accepted-but-ignored — plan can defer the deep SIP-layer honoring but MUST land the struct extension so the payload doesn't lose data silently.
</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Spec / docs
- `../docs/CARRIER-API.md` §Active Calls (lines 331–342) — authoritative route list for 6 CARRIER-API endpoints. Phase 4 adds 6 more (transfer/complete, transfer/cancel, play, speak, dtmf, record) beyond the spec to satisfy CALL-06..09 requirements.
- `../docs/CARRIER-ARCHITECTURE.md` — reference only

### Prior phase context
- `.planning/phases/01-api-shell-cheap-wrappers/01-CONTEXT.md` — SHELL-01..05 conventions (sub-router mount pattern, view types, ApiError variants, Pagination extractor + PaginatedResponse envelope)
- `.planning/phases/02-trunk-groups-schema-core-crud/02-CONTEXT.md` — entity + test fixture conventions
- `.planning/phases/03-trunk-sub-resources-l1-routing-resolve/03-CONTEXT.md` — IT-01 test matrix convention (auth/happy/404/400/409), wire type conventions (SHELL-04)

### Dispatch infrastructure (CALL-10 reuse target)
- `src/proxy/active_call_registry.rs` — `ActiveProxyCallRegistry::{list_recent, get, get_handle, count, session_ids}`, `ActiveProxyCallEntry`, `ActiveProxyCallStatus`
- `src/call/runtime/command_dispatch.rs:74-99` — `dispatch_console_command(registry, session_id, payload)` — unified entry point; **api_v1 reuses this verbatim** per D-06
- `src/call/runtime/command_dispatch.rs:128-162` — `dispatch_command` internal helper; how `send_command` dispatches to `SipSessionHandle`
- `src/call/adapters/console_adapter.rs:33` — `console_to_call_command` adapter (signature unchanged; input type alias updates when `CallCommandPayload` relocates per D-10)
- `src/call/domain/command.rs:22-306` — `CallCommand` enum (all 37 variants; Phase 4 uses `Hangup`, `Transfer`, `TransferComplete`, `TransferCancel`, `Play`, `SendDtmf`, `StartRecording`, `MuteTrack`, `UnmuteTrack`)
- `src/call/domain/command.rs:324-335` — `PlayOptions` struct
- `src/call/domain/command.rs:351-360` — `RecordConfig` struct
- `src/call/domain/policy.rs:169-180` — `MediaSource::{File, Url, Tts, Silence, Tone}`
- `src/call/runtime/mod.rs` (re-exports) — `CommandResult`, `ExecutionContext`, `MediaCapabilityCheck`, `CommandSource`

### Session layer (underlying handlers)
- `src/proxy/proxy_call/sip_session.rs:2237` — `SipSession::play_audio_file`
- `src/proxy/proxy_call/sip_session.rs:2278` — `SipSession::start_recording`
- `src/proxy/proxy_call/sip_session.rs:2318` — `SipSession::stop_recording`
- `src/proxy/proxy_call/session.rs:2369` — `transfer_to_endpoint` (attended path)
- `src/proxy/proxy_call/session.rs:2410` — `transfer_to_uri` (blind path; target normalization reference per D-22)
- `src/proxy/proxy_call/media_peer.rs:20-73` — `mute_track` / `unmute_track` trait + impl
- `src/proxy/proxy_call/sip_session.rs` — `SessionSnapshot` type (referenced from D-03 rich view)

### Console analog (pattern template — do NOT import from console)
- `src/console/handlers/call_control.rs:16-24` — existing router pattern for `/calls/active`, `/calls/active/{id}`, `/calls/active/{id}/commands`
- `src/console/handlers/call_control.rs:33-54` — **existing `CallCommandPayload` enum (Hangup/Accept/Transfer/Mute/Unmute)** — moves to `src/call/runtime/command_payload.rs` in Phase 4 per D-10, console updates import

### Shell / view conventions
- `src/handler/api_v1/common.rs` — `Pagination` extractor, `PaginatedResponse<T>`
- `src/handler/api_v1/error.rs` — `ApiError::{bad_request, not_found, conflict, internal, not_implemented}`
- `src/handler/api_v1/mod.rs` — router merge + Bearer auth setup
- `src/handler/api_v1/gateways.rs` — view type + validation pattern template
- `src/handler/api_v1/trunks.rs` — CRUD + transaction template (not needed here, but stylistic reference)

### Existing system health consumer
- `src/handler/api_v1/system.rs:105` — `registry.count()` usage for `active_calls` in `/system/health` — unchanged; Phase 4 just adds new readers of the same registry

### Test fixture template
- `tests/common/mod.rs` — `test_state_with_api_key` + `test_state_empty` helpers
- `src/proxy/active_call_registry.rs:203-331` — `tests` module shows how to construct a `SipSessionHandle` via `SipSession::with_handle(id)` and drop the `_cmd_rx` for registry-only tests
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets (Phase 4 consumes these unchanged)
- **`ActiveProxyCallRegistry`** (`src/proxy/active_call_registry.rs`) — already wired into `AppState.sip_server().active_call_registry`. All read methods we need exist (`list_recent`, `get`, `get_handle`, `count`).
- **`dispatch_console_command`** — the CALL-10 integration target. api_v1 calls it as-is after moving `CallCommandPayload` to a neutral module.
- **`CallCommand` enum** — already has every variant we need. One additive tweak to `SendDtmf` (D-14b).
- **`SessionSnapshot`** — populated by `SipSessionHandle::snapshot()`. Used by D-03 for rich views; by D-09 to resolve `leg`→`track_id` for mute/unmute.
- **`Pagination` + `PaginatedResponse<T>`** (Phase 1) — list endpoint plumbing.
- **`ApiError`** (Phase 1) — all 4 error paths used (`bad_request` for invalid filters/leg values/malformed body, `not_found` for unknown session_id or consult_leg, `conflict` for dispatch-mpsc failures or missing tracks, `internal` for unexpected anyhow).

### Established Patterns
- One sub-router file per resource group (`src/handler/api_v1/calls.rs`); merged into `protected` router in `mod.rs` (SHELL-01).
- View types named `{Entity}View`; never serialize underlying types directly (SHELL-04).
- Validation helpers live in the same file as handlers (e.g., `validate_leg`, `parse_target` colocated in `calls.rs`).
- Integration tests construct `SipSessionHandle` via `SipSession::with_handle(id)` and hold the `_cmd_rx` to receive dispatched commands, or drop it to simulate dispatch failure.
- POST endpoints that accept a body but dispatch async → return 200 with `{"message":"dispatched"}` (matches console's existing pattern), not 202.

### Integration Points
- `src/handler/api_v1/mod.rs::protected` — one new `.merge(calls::router())` call.
- `src/handler/api_v1/mod.rs` — one new `pub mod calls;` declaration.
- `src/call/runtime/command_payload.rs` (NEW) — relocated home for `CallCommandPayload` plus 8 new variants per D-11.
- `src/call/runtime/mod.rs` — re-export `CallCommandPayload`, `Leg`.
- `src/console/handlers/call_control.rs:3` — update `use crate::console::handlers::call_control::CallCommandPayload;` import consumers throughout the tree (RWI processor, console dispatch, tests) to the new path. Same for `src/call/adapters/console_adapter.rs`.
- `src/call/domain/command.rs:121-126` — additive field extension on `SendDtmf` per D-14b.
- `src/call/adapters/console_adapter.rs` — maps new API-only variants (BlindTransfer, AttendedTransfer*, Play, Speak, Dtmf, Record) to `CallCommand` — this is the adapter that earns its keep in Phase 4.

### Anti-patterns to avoid
- Do **not** add per-handler direct calls to `SipSessionHandle::send_command` or build `CallCommand` inline in the api_v1 handlers — everything routes through `dispatch_console_command` so CALL-10's "existing dispatch path" property holds.
- Do **not** hold `registry.inner` (the Mutex) across `.await` points in handlers — snapshot, drop guard, then await. The registry's methods already return cloned data.
- Do **not** require the client to know `track_id` — mute/unmute MUST translate `leg` → `track_id` inside the handler (D-09).
</code_context>

<specifics>
## Specific Ideas

- **CARRIER-API spec** lists only 6 Active Calls routes (list/get/hangup/transfer/mute/unmute). Phase 4 ships **12 total** — the 6 spec routes plus 6 CPaaS-style routes (transfer/complete, transfer/cancel, play, speak, dtmf, record) to satisfy CALL-06..09. Note this as an explicit spec-exceeding scope decision during planning so OpenAPI docs in v2.1 carry both groupings.
- **`dispatch_console_command` is the only dispatch entry point** api_v1 uses. If anyone reaches past it to call `dispatch_call_command` or `SipSessionHandle::send_command` directly in a Phase 4 handler, that's a CALL-10 violation — call it out in PR review.
- **Transcribe marker contract (D-18)** needs a paragraph in the `src/handler/api_v1/calls.rs` module docstring so Phase 7 webhook implementers know the marker's filename and location.
- **DTMF field extension (D-14b)** is the one cross-cutting proxy change. It's strictly additive — all existing callers that don't care about timing overrides pass `None`. Do NOT change the SIP-layer consumer in Phase 4 beyond accepting the new fields — leave them unused if behavior is already acceptable, flag as TODO for the next hardening pass.
- **Attended transfer response** returning `consult_leg_id` means the handler needs to synchronously learn the consult leg id post-dispatch. `CommandResult::success()` today returns no payload. **Claude's Discretion during planning:** either extend `CommandResult` to carry an optional payload OR dispatch + then poll the registry for the new consult leg. Recommend the former (one-line enum extension).
- **`list_recent` lock duration** — handler pulls the full snapshot with `usize::MAX`; that briefly holds the `Mutex` while cloning all entries. For Phase 4's expected scale (hundreds of active calls) this is fine. Flag as a revisit if the count ever hits tens of thousands.
</specifics>

<deferred>
## Deferred Ideas (out of Phase 4 — tracked for future phases)

- **Transcription post-processing** (CALL-09 `transcribe:true` flag consumer) → Phase 7 Webhooks, or a future dedicated STT phase. The marker file is dropped in Phase 4; actual STT happens later.
- **Recording download / metadata / delete endpoints** → Phase 12 (REC-01..07)
- **Hold / unhold REST endpoints** → not in CALL-01..10; add in Phase 7+ if carrier asks
- **Conference routes** (ConferenceCreate, Add, Remove, Mute, Unmute, Destroy) → Phase 13 or later
- **Supervisor routes** (Listen, Whisper, Barge, Stop) → v2.1 hardening or later (out of v2.0)
- **Transfer state polling endpoint** (GET /api/v1/calls/{id}/transfer) → deferred; clients track via hangup webhooks
- **Deep DTMF timing override** (honoring `duration_ms`/`inter_digit_ms` end-to-end in the SIP layer) → follow-up once we have a carrier asking for it. Phase 4 ships the wire contract and struct extension only.
- **Cursor-based pagination** for /calls → not needed at current scale; revisit if registry holds > 10k entries
- **OpenAPI documentation** of all 12 routes → v2.1 (OpenAPI publication is milestone-deferred)
- **URL fetch / cache infrastructure** for `MediaSource::Url` in /play → Phase 4 accepts the wire shape; actual URL playback works only if the underlying `FileTrack`/`RtpTrackBuilder` chain already supports it. If `MediaCapabilityCheck::Denied` fires, the 400 response documents the gap.
</deferred>

## Validation Architecture

**Unit-level (optional):**
- `validate_leg("caller"|"callee")` helper in `calls.rs`
- `validate_target` helper (accepts SIP URI or bare E.164, normalizes)
- `validate_dtmf_digits` helper (regex or per-char check)
- `resolve_recording_path` helper (auto-generation + path-traversal rejection)

**Integration-level (required — IT-01):**
- `tests/api_v1_calls.rs` — full 12-route matrix:
  - **List/get (2 tests per route):** 401, happy list paginated+filtered, happy get-by-id, 404 on unknown id
  - **Each command route (4 tests × 10 command routes = 40 tests max, likely ~25 after consolidation):** 401, happy dispatch, 404 on unknown session, 400 on malformed body, 409 on dispatch mpsc failure
  - **Mute-specific:** 409 when no media tracks in snapshot
  - **Transfer-specific:** 200 on blind with dispatched target; 200 on attended with consult_leg_id in body; 200 on transfer/complete; 200 on transfer/cancel; 404 on unknown consult_leg
  - **Record-specific:** 200 with auto-generated path in body; 200 with explicit in-dir path; 400 on path-traversal; marker file dropped when transcribe:true

**Regression:**
- Full Phase 1 + Phase 2 + Phase 3 baseline (currently 69 + 114 = ~183 tests across all suites) must stay green.
- Console's `tests/console_call_control*` (if any) must continue to pass after the `CallCommandPayload` relocation per D-10 — this is a compile-level change; tests should auto-pick-up the new import path.
- `src/rwi/*` consumers of `CallCommandPayload` (see `src/rwi/handler.rs`, `src/rwi/processor.rs`) must continue to compile — scan for imports during planning.

**Manual:**
- Smoke test: place a real SIP call via the carrier path, call `POST /api/v1/calls/{id}/play`, hear the file. Not a CI test but worth a one-time validation before merge.
- MIG-03: no console templates touched — N/A for Phase 4.

---

*Phase: 04-active-calls-mid-call-control*
*Context gathered: 2026-04-19*
*Source: discuss-phase + Phase 3 hand-off + CARRIER-API spec + dispatch infra audit*
