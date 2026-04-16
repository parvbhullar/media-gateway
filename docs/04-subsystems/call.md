# Call

## What it does

The call module implements the call application layer — the high-level logic
that decides what happens when a SIP INVITE arrives. It manages dialplans,
dial strategies (sequential/parallel), queue plans, call forwarding, failure
handling, recording configuration, and application routing (voicemail, IVR).
This is the bridge between the raw SIP proxy and the business logic.

## Key types & entry points

- **`CallAppFactory`** (trait) — creates call applications by name (e.g. voicemail, IVR) from route context and parameters. `src/call/mod.rs`
- **`CallFailureHandler`** (trait) — produces a fallback call application when a call fails (no answer, busy, declined, offline, timeout). `src/call/mod.rs`
- **`RouteInvite`** (trait) — routes an INVITE to a `RouteResult` containing trunk selection, rewrite rules, and headers. `src/call/mod.rs`
- **`Dialplan`** (struct) — complete call routing plan: direction, flow, recording, ringback, media config, failure action, call forwarding, max duration. `src/call/mod.rs`
- **`DialplanFlow`** (enum) — `Targets(DialStrategy)`, `Queue { plan, next }`, or `Application { app_name, params, auto_answer }`. `src/call/mod.rs`
- **`RoutingState`** — stateful round-robin counters and policy guard for load-balanced trunk selection. `src/call/mod.rs`
- **`Location`** — a SIP registration binding: AOR, destination, WebRTC flag, credentials, GRUU, path, transport. `src/call/mod.rs`
- **`QueuePlan`** — queue-specific settings: hold music, fallback action, dial strategy, ring timeout, retry codes. `src/call/mod.rs`
- **`DialStrategy`** (enum) — `Sequential(Vec<Location>)` or `Parallel(Vec<Location>)`. `src/call/mod.rs`
- **`TransactionCookie`** / **`TrunkContext`** / **`TenantId`** — per-transaction metadata. `src/call/cookie.rs`
- **`SipUser`** — SIP user model shared across backends. `src/call/user.rs`

## Sub-modules

- `domain/` — Call domain models (call commands, leg IDs, session state)
- `runtime/` — Call runtime and session management
- `app/` — Call application trait and implementations
- `adapters/` — Adapter layer between call logic and SIP/media
- `policy.rs` — Rate limiting and frequency policy guard
- `queue_config.rs` — Queue configuration and plan building
- `sip.rs` — SIP-specific call helpers
- `cookie.rs` — Transaction cookie types

## Configuration

Config keys from `[proxy]` affect routing rules and trunk selection.
Queue configuration is driven by route-level `queue` blocks.
Recording is controlled by per-route `recording` settings.

## Public API surface

The call module does not expose HTTP routes directly. It is consumed by
the proxy layer (for INVITE handling) and by the RWI module (for
programmatic call control).

## See also

- [proxy.md](proxy.md) — SIP proxy that dispatches to the call layer
- [rwi.md](rwi.md) — Real-time call control via WebSocket
- [../03-concepts/](../03-concepts/) — Call flow and dialplan concepts

---
**Status:** ✅ Shipped
**Source:** `src/call/`
**Related phases:** [Phase 4](../07-roadmap/phase-04-call.md)
**Last reviewed:** 2026-04-16

> TODO: deep-dive pending
