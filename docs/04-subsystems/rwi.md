# RWI (Real-time WebSocket Interface)

## What it does

The RWI module provides a real-time WebSocket interface for programmatic
call control. External applications connect via WebSocket, authenticate,
subscribe to call contexts, and issue commands (originate, answer, reject,
hangup, transfer, hold, mute, play audio, send DTMF). The module manages
session state, call ownership, smart routing, rule-based DTMF handling,
and supervised monitoring modes.

## Key types & entry points

- **`RwiAuth`** — token-based authentication with scoped permissions (originate, control, subscribe, supervise, admin). `src/rwi/auth.rs`
- **`RwiGateway`** — central gateway managing all WebSocket sessions, context subscriptions, call ownership, and event broadcasting. `src/rwi/gateway.rs`
- **`RwiSession`** — per-WebSocket-connection session state with ownership mode and supervisor mode. `src/rwi/session.rs`
- **`SmartRouter`** — routes incoming calls to RWI-connected applications based on context matching and priority rules. `src/rwi/routing.rs`
- **`TransferController`** — handles call transfers via SIP REFER, attended transfer, and 3PCC fallback. `src/rwi/transfer.rs`
- **`RuleExecutor`** — executes local DTMF rules and smart routing actions. `src/rwi/rule_engine.rs`
- **`RwiCommand`** (enum) — all WebSocket commands: `SessionSubscribe`, `CallOriginate`, `CallAnswer`, `CallHangup`, `CallTransfer`, `CallHold`, `CallMute`, `CallPlayAudio`, `CallSendDtmf`, etc. `src/rwi/proto.rs`
- **`RwiEvent`** (enum) — all WebSocket events pushed to clients: call state changes, DTMF, transfer updates, media events. `src/rwi/proto.rs`
- **`RwiEnvelope`** — versioned JSON envelope wrapping all RWI messages (`rwi: "1.0"`). `src/rwi/proto.rs`

## Sub-modules

- `app.rs` — RWI application entry point
- `auth.rs` — Token authentication and scope checking
- `gateway.rs` — Session registry, context subscriptions, event dispatch
- `handler.rs` — WebSocket upgrade and message handler
- `processor.rs` — Command processing pipeline
- `proto.rs` — Protocol types (RwiCommand, RwiEvent, envelope)
- `routing.rs` — SmartRouter and smart routing configuration
- `rule_engine.rs` — DTMF rule execution and local actions
- `session.rs` — Per-connection session state
- `transfer.rs` — Transfer controller (REFER, attended, 3PCC)

## Configuration

Config section `[rwi]` controls:

- Token-based authentication (`tokens` with scopes)
- Context definitions (`contexts` with no-answer timeout and actions)
- Transfer settings (REFER, attended, 3PCC fallback, timeouts)
- Smart routing rules

## Public API surface

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/rwi/ws` | WebSocket | Real-time call control channel |

## See also

- [../05-integration/rwi-protocol.md](../05-integration/rwi-protocol.md) — RWI protocol specification
- [call.md](call.md) — Call layer that RWI commands control
- [proxy.md](proxy.md) — SIP proxy that handles the underlying SIP transactions

---
**Status:** ✅ Shipped
**Source:** `src/rwi/`
**Related phases:** (core infrastructure)
**Last reviewed:** 2026-04-16

> TODO: deep-dive pending
