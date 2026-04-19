# Phase 4 Research — Active Calls & Mid-Call Control

**Researched:** 2026-04-19
**Domain:** Rust/Axum REST surface + Tokio-based active-call registry + `CallCommand` dispatch
**Confidence:** HIGH (codebase-verified; no external API dependence)
**Crate:** `rustpbx` (workspace member at `/media-gateway`)

---

## Summary

- Phase 4 is a **pure adapter + wiring phase**: 12 new REST routes under `src/handler/api_v1/calls.rs`, one additive struct tweak on `CallCommand::SendDtmf`, one relocation of `CallCommandPayload`, and ~25 integration tests. **Zero new proxy modules**, zero new DB tables, zero migrations.
- The unified dispatch path (`dispatch_console_command` → `console_to_call_command` → `SipSessionHandle::send_command`) is already live and production-exercised by the console handlers at `src/console/handlers/call_control.rs`. api_v1 reuses it **verbatim** after the payload relocation. CALL-10 holds by construction.
- **Critical finding (invalidates one CONTEXT claim):** `CallCommandPayload` is used **only** by `console/handlers/call_control.rs`, `call/adapters/console_adapter.rs`, `call/adapters/mod.rs` (doc comment only), `call/runtime/command_dispatch.rs`, and `call/runtime/mod.rs` (doc comment). **RWI does NOT consume it** — CONTEXT.md's "RWI processor" callout (`04-CONTEXT.md:286, 341`) is incorrect. Relocation blast radius is 5 files, all inside our tree.
- **Critical finding (affects D-09 leg→track_id resolution):** `SessionSnapshot` (at `src/proxy/proxy_call/sip_session.rs:14-24`) does **NOT** expose per-leg `track_id` fields. The canonical track IDs are compile-time constants: `SipSession::CALLER_TRACK_ID = "caller-track"` and `CALLEE_TRACK_ID = "callee-track"` (`sip_session.rs:163-164`). D-09 must resolve `leg → track_id` **without** looking at `SessionSnapshot` — it's a pure pattern match on the lowercase leg string. SessionSnapshot is still useful for the D-03 rich view (state, leg_count, bridge_active, media_path, answer_sdp) and is already `#[derive(Serialize)]`.
- **Critical finding (affects D-13 TTS + D-12b URL handling):** `SipSession::handle_play` at `sip_session.rs:3594-3614` hard-rejects anything other than `MediaSource::File` with `anyhow!("Only file playback supported")`. This error surfaces as `CommandResult::failure("Only file playback supported")` — which under CONTEXT's D-07 status-mapping would fall through to the 500 branch (message contains neither "not found" nor "failed to dispatch"). **Plan MUST either (a) extend the D-07 mapping with a "not supported" detection pattern, or (b) have the api_v1 handler probe `MediaSource` variant up-front and return 400 before dispatch.** Option (b) is cleaner — it matches the D-13 "return 400 on not_supported" intent. Same applies to `Tts` and `Url` source variants.

**Primary recommendation:** slice into 5 plans (payload relocation, basic commands, transfer, media, record+tests). Extend `D-07` mapping to detect "Only file playback supported" → 400 with reason "url/tts playback not wired yet" (a pre-dispatch variant probe in the handler is simpler than a message-string sniff). Land the `SendDtmf` struct extension in plan 04-04 alongside the DTMF route so the scope is localized.

---

## Locked Decisions Confirmation

Every D-00..D-22 is validated below against a code citation. Codebase **confirms all decisions as executable**; flags are raised only where the CONTEXT text is subtly ambiguous or incorrect.

| ID | Decision | Validating Citation | Status |
|----|----------|---------------------|--------|
| D-00 | Sub-router at `src/handler/api_v1/calls.rs`, merged in `mod.rs::protected` | `src/handler/api_v1/mod.rs:33-44` (merge point exists; pattern = `.merge(trunks::router())` etc.) | ✓ executable |
| D-00b | `tests/api_v1_calls.rs` following IT-01 matrix | `tests/api_v1_routing_resolve.rs` is the template (78 lines of fixture helpers + matrix) | ✓ executable |
| D-01 | Reuse `Pagination` extractor + `PaginatedResponse<T>` | `src/handler/api_v1/common.rs:25-66` — both types in scope, defaults page=1/size=20 | ✓ executable |
| D-02 | Use registry's native `session_id` verbatim | `src/proxy/active_call_registry.rs:24` (`session_id: String`) + `get_handle(&str)` at :133 | ✓ executable |
| D-03 | Rich view (entry + snapshot) on list AND get | `ActiveProxyCallEntry` is `#[derive(Clone, Debug, Serialize)]` at `active_call_registry.rs:22`; `SessionSnapshot` is `#[derive(Debug, Clone, serde::Serialize)]` at `sip_session.rs:13` | ✓ executable — but see "Implementation Unknowns" #2 for snapshot shape |
| D-04 | Filters: status/direction/caller/callee/since | ActiveProxyCallEntry has all fields. `status: ActiveProxyCallStatus` (`Ringing`/`Talking`), `direction: String`, caller/callee `Option<String>`, `started_at: DateTime<Utc>` | ✓ executable |
| D-05 | Default sort `started_at desc` | Already done by `list_recent` — `entries.sort_by(\|a, b\| b.started_at.cmp(&a.started_at))` at `active_call_registry.rs:122` | ✓ executable (handler re-sorts after filter) |
| D-06 | Reuse `dispatch_console_command` verbatim | Exists at `src/call/runtime/command_dispatch.rs:74-99`, signature `(registry, session_id, payload) -> anyhow::Result<CommandResult>` | ✓ executable |
| D-07 | CommandResult → HTTP mapping table | `CommandResult { success: bool, message: Option<String>, … }` at `src/call/runtime/command_executor.rs:10-21`. Mapping is string-sniff on `message`. | ⚠ FLAG — see "Play/Tts failure mapping" in Implementation Unknowns #6. Recommend pre-dispatch variant probe instead of string sniff for `MediaSource::Url` / `MediaSource::Tts`. |
| D-08 | 404 before dispatch via `registry.get_handle` | Existing console handler does the same — `src/console/handlers/call_control.rs:121-128` | ✓ executable |
| D-09 | Resolve `leg` → `track_id` via snapshot | ⚠ FLAG — **`SessionSnapshot` has no track_id field** (`sip_session.rs:14-24`). Use constants `SipSession::CALLER_TRACK_ID = "caller-track"` / `CALLEE_TRACK_ID = "callee-track"` from `sip_session.rs:163-164` instead. See Implementation Unknowns #2. |
| D-09b | `{leg}` mandatory, no "both" shortcut | Handler decision; trivial validation | ✓ executable |
| D-10 | Relocate `CallCommandPayload` to `src/call/runtime/command_payload.rs` | ⚠ FLAG — blast radius is 5 files, NOT including RWI. See Implementation Unknowns #5. |
| D-11 | Extend with 8 new variants | `CallCommand` at `src/call/domain/command.rs:22-302` already has `Transfer { attended: bool }`, `TransferComplete`, `TransferCancel`, `Play`, `SendDtmf`, `StartRecording`, `MuteTrack`, `UnmuteTrack` — every target variant exists | ✓ executable |
| D-11b | `Leg` enum `{Caller, Callee}` lowercase | New type in `command_payload.rs`; serde attrs | ✓ executable |
| D-12 | Play wire shape `{source: {file\|url}, leg, loop, interrupt_on_dtmf}` | `MediaSource::{File, Url, Tts, Silence, Tone}` at `src/call/domain/policy.rs:169-180`. `PlayOptions` at `command.rs:323-335` has exactly `loop_playback`, `interrupt_on_dtmf`, `await_completion`, `track_id`, `send_progress`. | ✓ executable |
| D-12b | URL fallback via existing FileTrack/RtpTrackBuilder | ⚠ FLAG — `handle_play` at `sip_session.rs:3594-3614` rejects non-`File` with `anyhow!("Only file playback supported")`. Not wired today. See Implementation Unknowns #6. |
| D-13 | Speak → `CallCommand::Play{source: Tts{…}}` | `MediaSource::Tts { text, voice: Option<String> }` exists at `policy.rs:175`. But see D-12b flag — TTS is NOT currently routed in `handle_play`. |
| D-14 | DTMF wire shape + default-leg-from-direction | `entry.direction: String` at `active_call_registry.rs:26`; valid strings "inbound" / "outbound" per `register_handle` default at :154 | ✓ executable |
| D-14b | Extend `CallCommand::SendDtmf` with timing fields | Current shape `SendDtmf { leg_id: LegId, digits: String }` at `command.rs:120-126`. Consumers: (1) `sip_session.rs:2966-2971` match arm, (2) `sip_session.rs:3825-3860` `handle_send_dtmf`, (3) `session_action_bridge.rs:170-173` returns `NotSupported`, (4) `sip_session.rs:4278-4285` unit test, (5) one `Some(leg_id)` read at `command.rs:430`. Extension is strictly additive — 5 call sites to touch, none require behavioral changes. | ✓ executable — see Implementation Unknowns #4 |
| D-15 | Record wire shape → `RecordConfig` | `RecordConfig { path: String, max_duration_secs, beep, format: Option<String> }` at `command.rs:349-360` — every field lines up except that `RecordConfig.path` is non-optional `String`, so auto-gen must happen **in the handler** before building the struct (can't pass `None` through). | ✓ executable |
| D-16 | RECORDINGS_DIR from `ProxyConfig` (Claude's discretion) | ⚠ Not a `ProxyConfig` field — actual field is `Config::recording: Option<RecordingPolicy>` with `RecordingPolicy::path: Option<String>` (`config.rs:102-127`). Accessor: `Config::recorder_path() -> String` at `config.rs:1098-1103` (always returns a path via default fallback). See Implementation Unknowns #1. |
| D-17 | Format validation wav/mp3 in Phase 4 | Handler validates before building `RecordConfig`; underlying layer already takes `Option<String>` | ✓ executable |
| D-18 | `transcribe:true` drops a sidecar marker | File-system write in the handler; no infra dependency. Path convention: sibling `<path>.transcribe.marker`. | ✓ executable |
| D-19 | Single `/transfer` endpoint with tagged body | `CallCommand::Transfer { leg_id, target, attended: bool }` at `command.rs:69-76` — both modes use the same variant, toggled by `attended` | ✓ executable |
| D-19 (response) | Attended returns `consult_leg_id` | ⚠ FLAG — `CommandResult` carries NO payload field (`command_executor.rs:10-21`). Extension options covered in Implementation Unknowns #3. |
| D-20 | Transfer complete/cancel routes | `CallCommand::TransferComplete { consult_leg: LegId }` at `command.rs:79-82`; `TransferCancel` at :84-88. Both exist. | ✓ executable |
| D-21 | Default leg = callee | Handler decision | ✓ executable |
| D-22 | Target normalization with configured host (Claude's discretion) | ⚠ Not a `ProxyConfig` field — actual field is `Config::external_ip: Option<String>` at `config.rs:238`. `transfer_to_uri` at `src/proxy/proxy_call/session.rs:2410-2423` uses `rsip::Uri::try_from(uri)` directly, does no normalization. Handler must normalize up-front. See Implementation Unknowns #1. |

**Confirmation summary:** 23/23 decisions are structurally executable. 6 decisions (D-07, D-09, D-10, D-12b, D-13, D-16, D-19, D-22) need tactical tweaks documented in the next section.

---

## Implementation Unknowns Resolved

### 1. Exact config field names (D-16 RECORDINGS_DIR + D-22 configured_transfer_host)

**RECORDINGS_DIR (D-16)** — there is no `ProxyConfig::recording_dir`. The canonical accessor is `Config::recorder_path() -> String` at `src/config.rs:1098-1103`:

```rust
pub fn recorder_path(&self) -> String {
    self.recording
        .as_ref()
        .map(|policy| policy.recorder_path())
        .unwrap_or_else(default_config_recorder_path)
}
```

`RecordingPolicy.path: Option<String>` at `config.rs:126`; fallback `default_config_recorder_path() = "./config/recorders"` at `config.rs:22-27`. **Plan action:** handler calls `state.config().recorder_path()`. It always returns a path (no panic possible) — `Config::recording` being `None` falls through cleanly.

**configured_transfer_host (D-22)** — the right field is `Config::external_ip: Option<String>` at `src/config.rs:238` (NOT `ProxyConfig` — it's on the outer `Config`). Fallback if `None`: use a synthesized string. `transfer_to_uri` at `src/proxy/proxy_call/session.rs:2410-2423` only does `Uri::try_from(uri)` — the host has to be baked into the URI before dispatch.

```rust
// src/proxy/proxy_call/session.rs:2410-2412
async fn transfer_to_uri(&mut self, uri: &str) -> Result<()> {
    let parsed = Uri::try_from(uri)
        .map_err(|err| anyhow!("invalid forwarding uri '{}': {}", uri, err))?;
```

**Plan action:** handler has a `parse_target(raw: &str, external_ip: Option<&str>) -> ApiResult<String>` helper:
- If `raw.starts_with("sip:") || raw.starts_with("sips:")` → validate with `rsip::Uri::try_from` → pass through
- Else if `raw.starts_with('+')` and rest is digits → format as `sip:{raw}@{host}` where `host = external_ip.unwrap_or("127.0.0.1")` (127.0.0.1 chosen to match test-fixture convention — flag loudly in plan that production deployments MUST set `external_ip`)
- Else → 400 with reason

### 2. SessionSnapshot shape (for D-03 rich view + D-09 leg→track_id)

**Shape** (verified at `src/proxy/proxy_call/sip_session.rs:13-24`):

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSnapshot {
    pub id: SessionId,
    pub state: SessionState,
    pub leg_count: usize,
    pub bridge_active: bool,
    pub media_path: MediaPathMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_sdp: Option<String>,
    #[serde(skip)]
    pub callee_dialogs: Vec<DialogId>,
}
```

Snapshot is `Serialize`-derived. `callee_dialogs` is `#[serde(skip)]` so the wire shape stays stable. `answer_sdp` hidden when `None`. Safe to serialize directly inside `ActiveCallView`.

**What it does NOT contain:** per-leg `track_id`, mute state, recording state. D-03's claim that snapshot exposes "negotiated codec, media mode, recording state, mute state" is partial — `media_path` and `bridge_active` are there, but codec/recording/mute are not projected. Recommendation: document this in the plan as "snapshot fields exposed: `state`, `leg_count`, `bridge_active`, `media_path`, `answer_sdp`; codec/recording/mute are NOT in v1 of `ActiveCallView` — revisit if carriers ask."

**For D-09 leg→track_id:** use the compile-time constants:

```rust
// src/proxy/proxy_call/sip_session.rs:163-164
pub const CALLER_TRACK_ID: &'static str = "caller-track";
pub const CALLEE_TRACK_ID: &'static str = "callee-track";
```

Handler resolves `Leg::Caller → "caller-track"`, `Leg::Callee → "callee-track"` with no snapshot dependency. The D-09 "409 when snapshot has no tracks yet" guard becomes: lookup `registry.get_handle(session_id)`; if `handle.snapshot().is_none()` OR `snapshot.leg_count < 2`, return 409 "media tracks not yet established". This matches the intent (don't dispatch mute before media is negotiated) without requiring fields the struct doesn't have.

### 3. CommandResult extension for attended-transfer consult_leg_id (D-19)

**Current shape** (`src/call/runtime/command_executor.rs:9-21`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub message: Option<String>,
    pub affected_leg: Option<LegId>,
    pub media_degraded: bool,
    pub degradation_reason: Option<String>,
}
```

**Option A (recommended — one-line extension):** add `pub payload: Option<serde_json::Value>` after `degradation_reason`. Default to `None` in all existing constructors. New helper:

```rust
pub fn success_with_payload(payload: serde_json::Value) -> Self {
    Self { success: true, payload: Some(payload), ..Self::default_fields() }
}
```

`SipSession::transfer_to_endpoint` at `session.rs:2369` (attended path) post-dispatch returns the consult_leg — the handler either retrieves it from a `oneshot` response channel or polls the registry for the newly created consult session. The cleanest hook: the `Transfer { attended: true }` path at `sip_session.rs` creates a new consult session and we can stash its id in the handle's snapshot cache BEFORE returning. Then the api_v1 handler reads back `handle.snapshot()` and pulls it from a new `consult_leg_id: Option<String>` field added to `SessionSnapshot`.

**Option B (fallback — poll the registry):** after dispatching, handler sleeps for ~50ms and scans `registry.list_recent(10)` for a new entry with this session as its parent (would require adding a `parent_session_id` field to `ActiveProxyCallEntry`, which is more invasive than Option A).

**Recommendation:** go with a hybrid — extend `CommandResult` with `payload: Option<serde_json::Value>`, AND extend `SessionSnapshot` with `pending_consult_leg_id: Option<String>` (mutated by the `Transfer{attended:true}` handler right before sending the SIP REFER). Handler reads the snapshot post-dispatch. One-line additions in both structs; zero proxy-layer refactor. Plan this change inside plan 04-03 (Transfer).

### 4. `SendDtmf` extension feasibility (D-14b)

**All match arms touching `CallCommand::SendDtmf`** (verified via grep):

| File | Line | What it does | Change needed |
|------|------|--------------|---------------|
| `src/call/domain/command.rs` | 120-126 | Struct definition | Add `duration_ms: Option<u32>`, `inter_digit_ms: Option<u32>` |
| `src/call/domain/command.rs` | 430 | `Some(leg_id)` projection in `leg_id()` helper | No change — destructures `leg_id` only |
| `src/call/adapters/session_action_bridge.rs` | 170-173 | Returns `AdapterError::NotSupported` | No change — ignores fields |
| `src/proxy/proxy_call/sip_session.rs` | 2966-2971 | Main dispatch arm | Update destructure to bind new fields; pass to `handle_send_dtmf` |
| `src/proxy/proxy_call/sip_session.rs` | 3825-3863 | `handle_send_dtmf` impl | Accept new args; current code uses hardcoded `Duration=160` in the body — new fields optionally override (deferred per D-14b scope note) |
| `src/proxy/proxy_call/sip_session.rs` | 4278-4285 | Unit test constructs `SendDtmf` | Add `duration_ms: None, inter_digit_ms: None` |

**Blast radius:** 3 files, 5 call sites. Strictly additive. For Phase 4, the `handle_send_dtmf` body can ignore the new fields (leave `Duration=160` hardcoded) per D-14b's deferral note — just accept them so the payload isn't silently dropped. Flag as `TODO(phase-hardening)` in the impl.

### 5. `CallCommandPayload` relocation blast radius (D-10)

**Every file importing `CallCommandPayload`:**

```
src/console/handlers/call_control.rs   (pub enum definition + POST handler + Json<CallCommandPayload>)
src/call/adapters/mod.rs               (doc comment only — `//! - ConsoleAdapter: Converts CallCommandPayload…`)
src/call/adapters/console_adapter.rs   (use statement + input type + 3 unit tests)
src/call/runtime/command_dispatch.rs   (use statement + dispatch_console_command signature)
src/call/runtime/mod.rs                (doc comment only — ASCII diagram)
```

**Total: 5 files, 3 code-bearing.**

**NOT in the blast radius** (CONTEXT.md incorrectly lists these):
- `src/rwi/handler.rs` — uses `RwiCommandPayload`, NOT `CallCommandPayload` (grep confirmed: "No matches found")
- `src/rwi/processor.rs` — same, only `RwiCommandPayload`

**Plan action:** create `src/call/runtime/command_payload.rs` with the enum (extended per D-11/D-11b), update the 3 code imports, leave the 2 doc comments as-is (paths in doc comments don't break compile). Re-export from `call/runtime/mod.rs::pub use command_payload::*`.

### 6. `MediaCapabilityCheck` behavior for `MediaSource::Tts` / `MediaSource::Url`

**What `check_media_capability` does for `Play`** (`src/call/runtime/command_executor.rs:129-138`):

```rust
CallCommand::Play { .. } => {
    if self.media_profile.can_play() {
        MediaCapabilityCheck::Allowed
    } else {
        MediaCapabilityCheck::Degraded {
            reason: "playback not supported in bypass mode".to_string(),
        }
    }
}
```

**It does NOT inspect the `source` variant.** `MediaSource::File`, `Url`, `Tts` all return `Allowed` if media is anchored. No `Denied` path for url/tts at the capability-check layer.

**Where url/tts actually fails:** `SipSession::handle_play` at `src/proxy/proxy_call/sip_session.rs:3594-3614`:

```rust
let file_path = match source {
    crate::call::domain::MediaSource::File { path } => path,
    _ => return Err(anyhow!("Only file playback supported")),
};
```

This bubbles up as `CommandResult::failure("Only file playback supported")`. Under D-07 mapping this message matches NEITHER "not found" NOR "failed to dispatch" — it falls through to the `anyhow error from dispatch → 500` row. Not what CONTEXT D-12b/D-13 intend.

**Two fix options:**

1. **Pre-dispatch probe in the handler (RECOMMENDED):** before calling `dispatch_console_command`, the `/play` handler inspects the request's `source` variant. If it's `Url` or `Tts`, it returns 400 with `{"error": "url/tts playback not wired; see CALL-06/07 deferred item", "code": "not_supported"}`. No proxy changes. `/speak` handler always returns 400 with `"tts engine not wired"` in Phase 4 (per D-13, until the engine lands). Alternative: allow the dispatch and catch the specific failure message pattern at the response-mapping step.

2. **Extend D-07 mapping with a "not supported" pattern:** treat any `CommandResult::failure` where `message` contains `"not supported"` OR `"Only file playback supported"` as 400 instead of 500. More brittle; couples the HTTP layer to internal error messages.

**Recommended:** Option 1 + add one extra status-mapping row to D-07 as a safety net: "message matches substring `not supported` → 400 with code `not_supported`". The handler short-circuits are the primary defense; the mapping change catches the future cases where new CallCommand variants fail with similar wording.

### 7. Test harness for dispatch assertions

Pattern already exists in `src/proxy/active_call_registry.rs:208-214`:

```rust
fn make_handle(session_id: &str) -> SipSessionHandle {
    use crate::call::runtime::SessionId;
    let id = SessionId::from(session_id);
    let (handle, _cmd_rx) = SipSession::with_handle(id);
    handle
}
```

`SipSession::with_handle(id)` at `src/proxy/proxy_call/sip_session.rs:166-177`:

```rust
pub fn with_handle(id: SessionId) -> (SipSessionHandle, mpsc::UnboundedReceiver<CallCommand>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let snapshot_cache: Arc<RwLock<Option<SessionSnapshot>>> = Arc::new(RwLock::new(None));
    let handle = SipSessionHandle { session_id: id, cmd_tx, snapshot_cache };
    (handle, cmd_rx)
}
```

**Integration tests should:**
1. Build `test_state_with_api_key(name)` via `tests/common/mod.rs:70-84`
2. Access `state.sip_server().inner.active_call_registry` (field is `pub` per `src/proxy/server.rs:73`)
3. Use `SipSession::with_handle(SessionId::from("test-sess-1"))` to get `(handle, mut cmd_rx)`
4. Call `registry.upsert(entry, handle)` and `registry.register_handle(id, handle)` as the console tests do
5. **To assert a command landed:** hold `cmd_rx` alive and call `cmd_rx.try_recv()` after the HTTP response — you get the `CallCommand` enum variant directly
6. **To simulate dispatch mpsc failure (D-07 409 path):** drop `cmd_rx` before the HTTP request. `SipSessionHandle::send_command` will return `anyhow::Error("channel closed: …")`, which `dispatch_console_command` wraps into `CommandResult::failure("failed to dispatch: channel closed: …")` → 409.

Snapshot mutation for D-09 tests: the handle exposes `SipSessionHandle::update_snapshot(SessionSnapshot)` at `sip_session.rs:153-155` — use it to stamp a snapshot with `leg_count = 2` to pass the mute/unmute precondition, or leave it `None` to trigger the 409.

---

## Task Breakdown Suggestion

5 plans. Average ~150-250 LOC each (handler plus tests). Every plan ends compile-green and test-green against Phase 1 + 2 + 3 baseline. Plan 04-01 unblocks 04-02..05.

### Plan 04-01 — Payload relocation + foundation (list/get)

**Scope:**
- Create `src/call/runtime/command_payload.rs` — move `CallCommandPayload` out of `console/handlers/call_control.rs`. Extend with 8 new variants per D-11 + `Leg` enum per D-11b.
- Update 3 imports (`console/handlers/call_control.rs`, `call/adapters/console_adapter.rs`, `call/runtime/command_dispatch.rs`). Re-export from `runtime/mod.rs`.
- Create `src/handler/api_v1/calls.rs` with router scaffold + wire types (`ActiveCallView`, `CallListQuery`) + `GET /calls` (paginated, filtered) + `GET /calls/{id}` (rich).
- Merge into `src/handler/api_v1/mod.rs::protected`.
- **Tests:** `tests/api_v1_calls.rs` — auth 401 on both routes, happy list (empty + seeded), pagination math, filters (status/direction/caller/callee/since), 400 on unparseable `since`, 404 on unknown id, rich get with seeded snapshot.

**Files touched:** 5 (4 edits + 2 new). **Net new LOC:** ~300.

### Plan 04-02 — Basic commands (hangup/mute/unmute)

**Scope:**
- Extend `console_to_call_command` adapter to consume new `CallCommandPayload::Mute`/`Unmute` variants (existing Mute/Unmute take raw `track_id`; new ones take `Leg` and handler pre-resolves). Hangup passthrough unchanged.
- Add `POST /calls/{id}/hangup`, `/mute`, `/unmute` routes.
- `validate_leg` helper in `calls.rs` (string → `Leg::Caller`/`Callee`).
- Leg→track_id resolution using `SipSession::{CALLER,CALLEE}_TRACK_ID` constants (NOT snapshot fields).
- 409 guard when `handle.snapshot().is_none() || snapshot.leg_count < 2`.
- **Tests:** per route — 401, happy dispatch (assert via `cmd_rx.try_recv()`), 404 on unknown session, 400 on bad body (missing leg / bad leg value), 409 on dropped cmd_rx, 409 on missing snapshot for mute.

**Files touched:** 2 edits + 1 test file append. **Net new LOC:** ~250.

### Plan 04-03 — Transfer (blind + attended + complete + cancel)

**Scope:**
- Extend adapter with `BlindTransfer`, `AttendedTransferStart`, `AttendedTransferComplete`, `AttendedTransferCancel` variants.
- `parse_target` helper — SIP URI passthrough or E.164 → `sip:<num>@{external_ip_or_127.0.0.1}`.
- Add `POST /calls/{id}/transfer`, `/transfer/complete`, `/transfer/cancel`.
- Extend `CommandResult` with `payload: Option<serde_json::Value>` (one line + backfill constructors).
- Extend `SessionSnapshot` with `pending_consult_leg_id: Option<String>` (one line + skip_serializing_if).
- Instrument `SipSession`'s attended-transfer path to set `pending_consult_leg_id` via `handle.update_snapshot(...)` BEFORE the REFER lands (Implementation Unknowns #3).
- Handler reads snapshot post-dispatch for the `consult_leg_id` in the response body.
- **Tests:** 401; blind happy with dispatched target (assert `CallCommand::Transfer{attended:false}`); attended happy with `consult_leg_id` in body; blind with bare E.164 target normalized; invalid target 400; complete/cancel happy; complete/cancel 404 on unknown consult_leg.

**Files touched:** 4 edits + test append. **Net new LOC:** ~280.

### Plan 04-04 — Media commands (play/speak/dtmf)

**Scope:**
- Extend adapter with `Play`, `Speak`, `Dtmf` variants.
- Extend `CallCommand::SendDtmf` with `duration_ms: Option<u32>`, `inter_digit_ms: Option<u32>` (5 call sites per Implementation Unknowns #4) — all pass `None` except the new api_v1 path.
- Add `POST /calls/{id}/play`, `/speak`, `/dtmf`.
- **Pre-dispatch probes:** `/play` rejects `{url}` and `{tts}` sources with 400 `not_supported` (url/tts not wired in `handle_play`); `/speak` always returns 400 `"tts engine not wired; see CALL-07 deferred"` for v1; `/dtmf` validates digit charset `[0-9A-D*#]` up-front.
- Add a safety-net status-mapping row in `dispatch_console_command → HTTP` table: `message contains "not supported"` → 400.
- `validate_dtmf_digits` helper.
- **Tests:** per route — 401, happy dispatch with file play, `/play` 400 on url/tts, `/speak` 400 always, `/dtmf` 400 on bad chars, 404 on unknown session, 409 on dropped cmd_rx. Tests also assert `duration_ms`/`inter_digit_ms` survive into the dispatched `CallCommand::SendDtmf`.

**Files touched:** 6 edits + test append. **Net new LOC:** ~300.

### Plan 04-05 — Record + transcribe marker + regression sweep

**Scope:**
- Extend adapter with `Record` variant.
- `resolve_recording_path` helper: when `path` is None → `{state.config().recorder_path()}/{session_id}-{unix_ts}.{ext}`; when explicit, reject `..`, require absolute, require parent dir exists.
- Format validation (`wav`/`mp3` only; default `wav`).
- Add `POST /calls/{id}/record` route. Response body: `{"message":"dispatched","path":"<resolved-path>"}`.
- Transcribe marker: on `transcribe:true`, drop empty file `<resolved-path>.transcribe.marker` via `std::fs::File::create`. Document contract in `calls.rs` module docstring (D-18).
- **Tests:** 401, happy auto-path, happy explicit path, 400 on format, 400 on path-traversal (`..`), 400 on relative path, marker file created when transcribe:true, no marker when false, 404 on unknown session, 409 on dropped cmd_rx.
- **Regression sweep:** full `cargo test -p rustpbx` passes — ~183 existing tests stay green + ~25 new = ~208 total.

**Files touched:** 2 edits + test append. **Net new LOC:** ~250.

---

## Wave Analysis

**Parallelizable (if ever splitting across agents):**
- 04-02, 04-03, 04-04, 04-05 all touch `src/handler/api_v1/calls.rs` — serializing to avoid merge conflicts is the safer bet.
- 04-02 and 04-04 touch different files in `call/adapters/` and `proxy/proxy_call/` — could run in parallel IF 04-01 lands first, BUT both edit `console_adapter.rs` match arms, so again serialization is safer.

**Sequential is the recommended order** (dependency: every plan imports the relocated `CallCommandPayload` from 04-01):

```
04-01 ──► 04-02 ──► 04-03 ──► 04-04 ──► 04-05
```

**Conflict surfaces to monitor:**
- `src/handler/api_v1/calls.rs` — grows in every plan. Keep handlers clustered by verb group for clean diffs.
- `src/call/runtime/command_payload.rs` — grows in 04-03, 04-04, 04-05.
- `src/call/adapters/console_adapter.rs` — match arm grows in 04-02 → 04-05.
- `tests/api_v1_calls.rs` — grows in every plan. Subdivide by module comments (`// ── mute/unmute ──`) for readability.

**Risk of parallelism:** tiny. Benefit of parallelism: ~30% wall-clock savings on a 5-agent pool. **Recommendation: sequential.** The cross-plan state (relocated enum, extended CommandResult, extended SessionSnapshot, extended SendDtmf) compounds in a way where parallel agents would thrash on rebase.

---

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | `cargo test` (built-in) + `tokio::test` async harness |
| Config file | `Cargo.toml` (`[[test]]` entries auto-discovered via `tests/*.rs`) |
| Quick run command | `cargo test -p rustpbx --test api_v1_calls` (target only Phase 4) |
| Full suite command | `cargo test -p rustpbx` (includes all Phase 1+2+3 baselines) |

### Unit-level test candidates
Plain `#[test]` fns colocated in `src/handler/api_v1/calls.rs`:

- `validate_leg("caller") == Leg::Caller`, `validate_leg("CALLEE") → 400`, `validate_leg("both") → 400`
- `parse_target("sip:1001@example.com", None)` → unchanged
- `parse_target("+14155551234", Some("1.2.3.4"))` → `"sip:+14155551234@1.2.3.4"`
- `parse_target("+14155551234", None)` → `"sip:+14155551234@127.0.0.1"`
- `parse_target("not-a-uri", None)` → 400
- `validate_dtmf_digits("12AB")` → ok; `validate_dtmf_digits("1e")` → 400; `validate_dtmf_digits("")` → 400
- `resolve_recording_path(None, "sess-1", "wav", "/var/rec")` → starts with `/var/rec/sess-1-` and ends with `.wav`
- `resolve_recording_path(Some("/var/rec/../../etc/passwd"), …)` → 400
- `resolve_recording_path(Some("relative.wav"), …)` → 400 (must be absolute)

Target: ~8-10 unit tests, sub-second total runtime.

### Integration test matrix per IT-01
`tests/api_v1_calls.rs` — 12 routes × auth/happy/404/400/409 matrix. Consolidate per D-00b into ~25 tests:

| Route | Tests (≥ count) |
|-------|-----------------|
| `GET /calls` | 401, happy list paginated, happy filtered by status/direction/caller/since, 400 on bad since (~5) |
| `GET /calls/{id}` | 401, happy rich, 404 unknown (~3) |
| `POST /calls/{id}/hangup` | 401, happy dispatch assert, 404, 409 dropped rx (~4) |
| `POST /calls/{id}/mute` + `/unmute` | 401, happy each, 404, 400 no-leg, 409 no-snapshot (~6 — share fixture) |
| `POST /calls/{id}/transfer` | 401, blind happy, blind e164-normalized, attended happy with consult_leg_id, 400 bad target (~5) |
| `POST /calls/{id}/transfer/complete` + `/cancel` | 401, happy each, 404 unknown consult_leg (~4) |
| `POST /calls/{id}/play` | 401, happy file, 400 url, 400 tts, 404, 409 (~5) |
| `POST /calls/{id}/speak` | 401, 400 (always, in v1), 404 (~3) |
| `POST /calls/{id}/dtmf` | 401, happy with duration/inter_digit, 400 bad digits, 404 (~4) |
| `POST /calls/{id}/record` | 401, happy auto-path, happy explicit, 400 format, 400 traversal, marker created when transcribe (~6) |

Per CONTEXT §Validation Architecture, total ≈ 25 after consolidation.

### Regression baseline
- **Phase 1:** 78 tests
- **Phase 2:** 114 total (36 new)
- **Phase 3:** 183 total (baseline per STATE.md §Phase 2 Verification); +~30 new tests in Phase 3's 4 test files → ~183 total claimed; this will stay as the baseline Phase 4 must preserve green

**Phase 4 delta:** +25 integration tests + ~8-10 unit tests ≈ +33 tests. Final target: ~215 green.

**Cross-cutting regression risks:**
- `console/handlers/call_control.rs` existing dispatch tests (if any — `tests/sip_session_command_test.rs` is adjacent) — must continue green after `CallCommandPayload` relocation. Compile-level change; path update in imports is all that's needed.
- `CallCommand::SendDtmf` consumers (5 call sites per Implementation Unknowns #4) must all compile after the struct extension.
- `CommandResult` constructors throughout `sip_session.rs` (~15 call sites) must keep working after adding `payload: Option<serde_json::Value>` — if we use `Default` + `..Default::default()`, zero call sites change. Implement `Default` for `CommandResult`.
- `SessionSnapshot` constructor at `sip_session.rs:529-537` — one-line addition for `pending_consult_leg_id: None`.

### Sampling rate
- **Per task commit:** `cargo test -p rustpbx --test api_v1_calls` (< 10s)
- **Per plan merge:** `cargo test -p rustpbx` (< 2 min on the existing baseline; Phase 4 adds negligible time)
- **Phase gate:** `cargo test -p rustpbx` fully green + `cargo build -p rustpbx --release` succeeds before `/gsd-verify-work`

### Wave 0 Gaps
None. All infrastructure already exists:
- Test harness: `tests/common/mod.rs` ships `test_state_with_api_key` and `test_state_empty`
- Registry seeding: `SipSession::with_handle` + `registry.upsert` / `register_handle` verified in `active_call_registry.rs::tests`
- `tower::ServiceExt::oneshot` pattern reused from every `tests/api_v1_*.rs` file
- `serde_json::json!()` + axum request builder conventions established across 10+ test files

---

## Risks & Mitigations

1. **D-12b / D-13 silently producing 500s when url/tts are submitted** — the capability-check layer doesn't cover `MediaSource::Url`/`Tts` at all, and the proxy's `handle_play` fails with a message that doesn't match any D-07 mapping row. **Mitigation:** pre-dispatch variant probe in the handler (option 1 in Implementation Unknowns #6). Ship as part of Plan 04-04. Add safety-net mapping row for `not supported` → 400.

2. **`SessionSnapshot` missing per-leg track fields for D-09** — CONTEXT assumes snapshot exposes track IDs. **Mitigation:** use the compile-time constants instead (`SipSession::{CALLER,CALLEE}_TRACK_ID`). Document this decision in the plan so future contributors don't re-try the snapshot-based approach. Use `leg_count < 2` as the 409 precondition instead of "no tracks".

3. **Attended-transfer consult_leg_id retrieval** — `CommandResult` has no payload today. Extending it AND `SessionSnapshot` to carry `pending_consult_leg_id` crosses the api_v1/proxy boundary twice. **Mitigation:** land both one-line extensions in Plan 04-03, co-located with the first route that needs them. Alternative (if review pushback): defer attended-transfer's richer response to a follow-up plan and ship `/transfer/complete` / `/cancel` with `{consult_leg_id}` required in the REQUEST body (client tracks it themselves from the SIP-layer hangup webhooks). Flag in the plan as a fallback if extension review is contentious.

4. **`transfer_to_uri` normalization** — handler formats E.164 as `sip:{num}@{host}` but doesn't validate the resulting URI with `rsip::Uri::try_from` before dispatch. If `external_ip = Some("bad value with spaces")`, the SIP layer will reject it at REFER time with an opaque 500. **Mitigation:** `parse_target` helper does a post-normalization `rsip::Uri::try_from` validation — if it fails, return 400 "invalid external_ip configuration". Surfaces the misconfiguration at request-time instead of REFER-time.

5. **Recording-path traversal bypass via symlinks** — `resolve_recording_path` rejects `..` but doesn't `canonicalize` the path, so a symlinked path inside the recording dir could still escape. **Mitigation:** use `std::fs::canonicalize` on the parent dir and confirm it's `starts_with(recorder_path)`. Plan notes this as a security consideration. Rejection of relative paths + `..` handles 99% of cases; canonicalize is defense-in-depth.

---

## Open Questions

None. Every scope guardrail from CONTEXT is resolved via codebase-verified citations above.

---

## Metadata

**Confidence breakdown:**
- Standard stack (Axum, SeaORM, rsipstack, Tokio, serde): **HIGH** — in-crate patterns already shipped across Phases 1-3
- Dispatch path (CALL-10 reuse): **HIGH** — `dispatch_console_command` is exercised daily by console
- Payload relocation blast radius: **HIGH** — exhaustive grep done; CONTEXT had an incorrect RWI callout
- SessionSnapshot fields: **HIGH** — struct definition read at `sip_session.rs:14-24`
- CommandResult extension: **HIGH** — struct definition read; extension is one line
- URL/TTS playback state: **HIGH** — `handle_play` hard-codes file-only
- Config field names: **HIGH** — `Config::recorder_path()` and `Config::external_ip` verified
- Test harness: **HIGH** — pattern exercised 10+ times in existing `tests/api_v1_*.rs`

**Research date:** 2026-04-19
**Valid until:** 2026-05-19 (30 days; no external API changes expected)

---

## RESEARCH COMPLETE

All scope guardrails resolved. No blockers. Plan can proceed with the 5-plan breakdown above, starting with 04-01 (payload relocation + list/get foundation).
