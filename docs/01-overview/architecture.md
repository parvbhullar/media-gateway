# Architecture

![SuperSip Architecture](../architecture.svg)

SuperSip is an AI-native UCaaS platform built as a single Rust binary. The system is organized into three layers that separate external access, core telephony logic, and application-level integrations.

## Access Layer

The access layer terminates every inbound and outbound media path:

| Endpoint | Transport |
|----------|-----------|
| **PSTN** | SIP Trunk (UDP/TCP/TLS) |
| **WebRTC Browser** | WS/WSS with ICE/DTLS-SRTP |
| **SIP Client** | Standard SIP UA (UDP/TCP/TLS) |
| **Mobile App** | WebRTC or SIP over TLS |

## Core Layer

The core layer contains the telephony engine that processes every call:

| Component | Role |
|-----------|------|
| **B2BUA** | Dual-dialog call control with full SIP stack (UDP/TCP/WS/TLS/WebRTC), registration, and auth |
| **IVR** | Interactive voice response and menu trees |
| **Media Fabric** | RTP relay, NAT traversal, codec negotiation, WebRTC-to-SIP bridging |
| **Queue / ACD** | Sequential or parallel agent ringing, hold music, priority scheduling |
| **Recording** | SipFlow unified SIP+RTP capture with hourly rotation and on-demand playback |
| **CDR** | Call detail records with webhook delivery on hangup |
| **SIP Trunk** | Carrier connectivity, health monitoring, failover |

## App Service Layer

The app service layer is where external systems integrate with SuperSip:

| Component | Role |
|-----------|------|
| **AI Voice Agent** | Automated call handling via [Active Call](https://github.com/restsend/active-call) |
| **HTTP DialPlan** | Every INVITE hits your webhook; you return a routing decision in JSON |
| **RWI** | Real-time WebSocket interface for in-call control (listen, whisper, barge, transfer, hold, media injection) |
| **Webhook Consumer** | Push CDR, queue status, and events to your CRM or ticketing system |
| **CRM / Ticketing** | External system integration via webhooks and REST |

## Architectural Anchors

These constraints are load-bearing decisions that shape every subsystem:

- **One SIP stack.** SuperSip uses `rsipstack` exclusively. No Sofia-SIP or pjsip FFI in this binary.
- **One database.** SeaORM over SQL (SQLite or Postgres). No Redis dependency.
- **One binary.** A single `rustpbx` binary ships the proxy, media engine, console, and API surface.
- **Shared state bridge.** `AppStateInner.console: Option<Arc<ConsoleState>>` connects the `/api/v1/*` surface and the console UI — both share the same `&DatabaseConnection`.
- **View types, not ORM models.** Pure data-fetch functions live at module level; both HTML and JSON handlers call them. SeaORM entities are never serialized directly to API consumers.

## Further Reading

- [Concepts](../03-concepts/) -- B2BUA model, media bridging, routing pipeline
- [Subsystems](../04-subsystems/) -- deep dives into each core component

---
**Status:** Shipped
**Source:** `README.md`, `.planning/PROJECT.md`
**Last reviewed:** 2026-04-16
