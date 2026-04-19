# Phase 4: Active Calls & Mid-Call Control — Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-19
**Phase:** 04-active-calls-mid-call-control
**Areas discussed:** List/get shape & pagination, Mute/unmute & dispatch coupling (CALL-10), Mid-call commands wire types (play/speak/dtmf/record), Transfer: attended vs blind + wire shape

---

## List/get shape & pagination

### Q1: Pagination approach — registry currently has list_recent(limit) only

| Option | Description | Selected |
|--------|-------------|----------|
| Snapshot + Phase 1 Pagination | Pull full list, sort by started_at desc, apply page/page_size in handler. No registry changes. | ✓ |
| Extend registry with offset | Add list_page(offset, limit). Mutates shared struct. | |
| Cursor (session_id-based) | ?after=<id>. Diverges from rest of /api/v1. | |

### Q2: URL path parameter {id}

| Option | Description | Selected |
|--------|-------------|----------|
| session_id verbatim | Use registry's native key. Matches dispatch_console_command. | ✓ |
| SIP Call-ID | More meaningful to ops but requires secondary lookup. | |

### Q3: ActiveCallView shape

| Option | Description | Selected |
|--------|-------------|----------|
| Minimal (registry fields only) | Thin wire. 1:1 mapping. | |
| Rich (+ SessionSnapshot everywhere) | Codec, media mode, recording state, mute state. | ✓ |
| Minimal on list, rich on get-by-id | Hybrid. | |

### Q4: Default sort + filters

| Option | Description | Selected |
|--------|-------------|----------|
| Sort started_at desc; no filters | Simplest. | |
| + ?status & ?direction | Small addition. | |
| + full filter set (status, direction, caller, callee, since) | Carrier parity. | ✓ |

---

## Mute/unmute & dispatch coupling (CALL-10)

### Q1: Mute/unmute target

| Option | Description | Selected |
|--------|-------------|----------|
| Abstract to {leg: 'caller'\|'callee'} | Carrier-friendly; handler resolves track_id. | ✓ |
| Pass-through track_id | Simpler; exposes internal concept. | |
| Mute both legs | Simplest; no per-leg selectivity. | |

### Q2: api_v1 ↔ dispatch integration

| Option | Description | Selected |
|--------|-------------|----------|
| Reuse dispatch_console_command verbatim | Move CallCommandPayload to neutral module; extend variants. | ✓ |
| New api_v1 adapter function | Parallel ApiCallCommand + api_v1_to_call_command. | |
| api_v1 calls dispatch_call_command directly | Skip adapter layer. | |

### Q3: Where CallCommandPayload lives post-refactor

| Option | Description | Selected |
|--------|-------------|----------|
| src/call/runtime/command_payload.rs (neutral) | Both console and api_v1 import. | ✓ |
| Leave in console::handlers::call_control | Inverts layering. | |
| Duplicate with separate enums | Divergence risk. | |

### Q4: Error mapping for CommandResult

| Option | Description | Selected |
|--------|-------------|----------|
| CommandResult mapping (success→200, not-found→404, not-supported→400, dispatch-err→409) | REST-idiomatic. | ✓ |
| All 200 with body status | REST anti-pattern. | |
| Strict 2xx/4xx/5xx on success only | Loses distinction. | |

---

## Mid-call commands wire types (play/speak/dtmf/record)

### Q1: /play request body

| Option | Description | Selected |
|--------|-------------|----------|
| {source: {file}\|{url}} + leg/loop/interrupt_on_dtmf | Tagged enum matches MediaSource. | ✓ |
| Only file path | Simpler; rejects URL. | |
| Accept both but reject URL with 400 until resolver wired | Forward-compat. | |

### Q2: /speak TTS scope

| Option | Description | Selected |
|--------|-------------|----------|
| {text, voice?, leg?} passthrough to MediaSource::Tts | Let MediaCapabilityCheck handle not-wired. | ✓ |
| Return 501 Not Implemented | Don't ship half-working. | |
| Wire + runtime probe | Explicit capability check before dispatch. | |

### Q3: /dtmf + /record bodies

| Option | Description | Selected |
|--------|-------------|----------|
| Recommended: dtmf {digits, leg?}; record {path?, format?, beep?, max_duration_secs?, transcribe?} | Balanced. | |
| Full knobs: dtmf {digits, duration_ms, inter_digit_ms, leg}; record full RecordConfig | Max configurability. Requires SendDtmf struct extension. | ✓ |
| Strict minimums | Thinnest. | |

**Notes:** User chose full knobs. Captured in D-14/D-14b as requiring an additive extension to `CallCommand::SendDtmf` with optional `duration_ms`/`inter_digit_ms` fields. Plan must land the struct extension; deep SIP-layer honoring can follow in a later hardening pass.

### Q4: Record path + transcription behavior

| Option | Description | Selected |
|--------|-------------|----------|
| Auto-generate in recordings dir; transcribe:true is a Phase 7 marker | Separates Phase 4 wire from Phase 7 post-processing. | ✓ |
| Require client absolute path | Path-traversal risk. | |
| Auto-generate; reject transcribe:true with 501 | Explicit scoping out. | |

---

## Transfer: attended vs blind + wire shape

### Q1: Transfer URL shape

| Option | Description | Selected |
|--------|-------------|----------|
| Single endpoint with {type, target, leg?} | One router, tagged body. | ✓ |
| /transfer/blind + /transfer/attended | Self-documenting, duplicated. | |

### Q2: Attended flow scope

| Option | Description | Selected |
|--------|-------------|----------|
| Start-only in Phase 4 (defer complete/cancel) | Smaller scope. | |
| Full flow (start + complete + cancel) | CALL-04 literal parity. | ✓ |
| Full flow + state polling (GET /transfer) | Largest scope. | |

**Notes:** User chose full flow. /transfer/complete and /transfer/cancel ship in Phase 4.

### Q3: Default leg for transfer

| Option | Description | Selected |
|--------|-------------|----------|
| Default callee; optional {leg} override | Typical carrier flow. | ✓ |
| Require explicit leg | No default; more friction. | |
| Transfer both legs | Loses control. | |

### Q4: Target format validation

| Option | Description | Selected |
|--------|-------------|----------|
| SIP URI or E.164; handler normalizes | Matches transfer_to_uri in session.rs. | ✓ |
| SIP URI only | Strict; client builds URI. | |
| Freeform; downstream validates | Error surface moves. | |

---

## Claude's Discretion

Areas where the user deferred to Claude during planning:
- Exact `RECORDINGS_DIR` config field name (likely `ProxyConfig::recording_dir` or `callrecord.storage_path`)
- Exact `configured_transfer_host` config field name (likely `proxy.external_ip`, else first UDP transport bind)
- `ActiveCallView.status` wire format (recommend lowercase string)
- Snapshot nesting (recommend nested `"snapshot": {...}` field)
- Transcribe marker filename convention (recommend sibling `<path>.transcribe.marker`)
- Snapshot lock semantics (rely on existing internal locking; no handler-level Mutex juggling)
- `CommandResult` extension for attended transfer consult_leg_id payload (recommend one-line optional payload field)
- When DTMF timing extensions (D-14b) are plumbed end-to-end vs wire-accepted-but-ignored (struct extension MUST land; deep SIP layer honoring can be a follow-up)

## Deferred Ideas

Captured in CONTEXT.md `<deferred>` section. Summary:
- Transcription post-processing → Phase 7 Webhooks
- Recordings CRUD → Phase 12 (REC-01..07)
- Hold/unhold routes → not in CALL-01..10
- Conference routes → Phase 13+
- Supervisor routes → out of v2.0
- Transfer state polling → webhook-tracked
- Deep DTMF timing override → follow-up after carrier requests
- Cursor pagination → revisit if >10k active calls
- OpenAPI docs → v2.1
- URL fetch / caching for /play URL source → available if underlying FileTrack already supports it

---

*Log preserves 4 areas × 4 questions = 16 decision points captured during discuss-phase on 2026-04-19.*
