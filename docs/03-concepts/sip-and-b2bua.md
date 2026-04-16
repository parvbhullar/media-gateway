# SIP & B2BUA

SuperSip is a **Back-to-Back User Agent (B2BUA)**, not a simple SIP proxy.
This page explains what that means and why it matters.

## What is a B2BUA?

A B2BUA terminates the incoming SIP dialog on one side and originates a
completely independent dialog on the other. From each endpoint's
perspective, it is talking directly to the B2BUA — not to the remote party.

This gives SuperSip full control over:

- **Signalling** — headers, URIs, and SDP can be rewritten between legs.
- **Media** — RTP streams can be relayed, transcoded, recorded, or mixed
  without either endpoint's awareness.
- **Call state** — each leg has its own dialog state machine, so features
  like transfer, hold, and conference are implemented locally rather than
  relying on endpoint cooperation.

In contrast, a stateless SIP proxy simply forwards messages and has no
control over media or dialog state.

## SIP dialog lifecycle

A typical call through SuperSip follows the standard SIP INVITE flow:

```
Caller              SuperSip (Leg A)     SuperSip (Leg B)           Callee
  |--- INVITE -------->|                      |                       |
  |<-- 100 Trying -----|                      |                       |
  |                     |--- INVITE --------->|--- INVITE ----------->|
  |                     |<-- 100 Trying ------|<-- 180 Ringing -------|
  |<-- 180 Ringing -----|                      |                       |
  |                     |                      |<-- 200 OK ------------|
  |<-- 200 OK ----------|                      |--- ACK -------------->|
  |--- ACK ------------>|                      |                       |
  |                   (media flows via RTP relay)                      |
  |--- BYE ------------>|                      |--- BYE -------------->|
  |<-- 200 OK ----------|                      |<-- 200 OK ------------|
```

Key SIP methods handled:

| Method     | Purpose                              |
|------------|--------------------------------------|
| INVITE     | Initiate or re-negotiate a call       |
| ACK        | Confirm 200 OK for INVITE             |
| BYE        | Terminate an established dialog        |
| CANCEL     | Abort a pending INVITE                 |
| REGISTER   | Bind an AoR to a contact address       |
| OPTIONS    | Keepalive / capability query            |
| REFER      | Initiate call transfer                 |
| INFO       | Mid-dialog signalling (e.g. DTMF)      |
| NOTIFY     | Subscription-based event delivery       |

## Session management

The `proxy_call/session.rs` module manages both call legs as a single
logical session (`CallSession`). Each session holds:

- **Leg A** — the inbound dialog (caller-side `ServerInviteDialog`).
- **Leg B** — the outbound dialog (callee-side client invite).
- **MediaBridge** — connects the two legs' RTP streams and handles
  codec negotiation via `MediaNegotiator`.
- **SessionTimer** — RFC 4028 session timer with automatic re-INVITE
  refresh.
- **CallReporter** — emits CDR and call-record events on state changes.

The session processes events from both legs in a unified select loop,
translating far-end signals into near-end actions (e.g. a BYE on Leg B
triggers a BYE on Leg A).

## Transport options

SuperSip binds listeners for multiple SIP transports simultaneously:

| Transport  | Use case                                    |
|------------|---------------------------------------------|
| **UDP**    | Traditional SIP trunks and LAN endpoints     |
| **TCP**    | Large messages, NAT traversal                |
| **TLS**    | Encrypted SIP signalling (SIPS)              |
| **WebSocket** | Browser-based SIP via SIP.js / JsSIP     |
| **WebRTC** | Browser RTC with ICE, DTLS-SRTP              |

Transport binding is configured via `proxy.udp`, `proxy.tcp`, `proxy.tls`,
and `proxy.ws` in the TOML config. TLS certificates can be provisioned
automatically via the ACME addon.

## Registration

SuperSip includes a built-in **registrar** (`proxy/registrar.rs`) that
accepts REGISTER requests and maintains a location service. User
credentials are resolved through pluggable **user backends**:

| Backend        | Source                   |
|----------------|--------------------------|
| `user_plain`   | TOML config `[users]`    |
| `user_db`      | Database (`rustpbx_sip_users`) |
| `user_http`    | External HTTP webhook    |
| `user_extension` | Extension number lookup |

The registrar also supports **trunk registration** (`trunk_registrar.rs`)
for outbound trunk keepalive with upstream carriers.

## NAT handling

The `proxy/nat.rs` module detects and compensates for NAT by comparing
Via received-from addresses with Contact URIs. When NAT is detected,
SuperSip rewrites Contact headers and enables RTP latching so media
flows correctly through NAT gateways.

## Further reading

- [Routing Pipeline](routing-pipeline.md) — how INVITEs are routed
- [Media Fabric](media-fabric.md) — RTP relay and codec negotiation
- [Proxy subsystem](../04-subsystems/proxy.md) — implementation details
