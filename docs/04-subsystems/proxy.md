# Proxy

## What it does

The proxy module is the SIP core of SuperSip. It implements a full SIP proxy/B2BUA
server that handles incoming and outgoing SIP transactions, user registration,
authentication, NAT traversal, trunk registration, presence, ACL enforcement,
and WebSocket transport. Every SIP message entering the system passes through
this module before being dispatched to the call layer.

## Key types & entry points

- **`ProxyModule`** (trait) — lifecycle hooks (`on_start`, `on_stop`, `on_transaction_begin`, `on_transaction_end`) for pluggable proxy behaviours. `src/proxy/mod.rs`
- **`UserBackend`** (trait) — pluggable user lookup; implementations include plain-text, HTTP, database, and extension-based backends. `src/proxy/user.rs`
- **`SipServerInner`** — holds the SIP endpoint, transport layer, dialog layer, registrar state, locator, and active call registry. `src/proxy/server.rs`
- **`SipServerBuilder`** — builder for constructing a configured `SipServerInner` with modules, trunks, routes, and listeners. `src/proxy/server.rs`
- **`ProxyAction`** (enum) — `Continue` or `Abort`, returned by `ProxyModule::on_transaction_begin`. `src/proxy/mod.rs`
- **`FnCreateProxyModule`** / **`FnCreateRouteInvite`** — factory function types for creating proxy modules and route invite handlers. `src/proxy/mod.rs`

## Sub-modules

- `routing/` — Route matching engine (see [routing.md](routing.md))
- `proxy_call/` — Per-call SIP session management and B2BUA logic
- `auth` — SIP digest authentication backend
- `registrar` — SIP REGISTER handler and binding store
- `nat` — NAT traversal (rport, Via rewriting)
- `acl` — Access control lists for IP-based filtering
- `trunk_registrar` — Outbound SIP trunk registration
- `locator` / `locator_db` / `locator_webhook` — User location services (memory, DB, webhook)
- `presence` — SIP SUBSCRIBE/NOTIFY presence manager
- `ws` — WebSocket SIP transport handler
- `user` / `user_db` / `user_extension` / `user_http` / `user_plain` — User backend implementations
- `active_call_registry` — Tracks active proxy calls
- `gateway_health` — SIP trunk health monitoring
- `data` — Shared proxy data context
- `call` — Call routing and dialplan inspection

## Configuration

Config keys from `[proxy]` in config.toml control listener addresses,
SIP domains, transport (UDP/TCP/TLS/WS/WSS), authentication realm,
registration expiry, NAT settings, ACL rules, trunk definitions, and
route tables. See also `[proxy.http_router]` for HTTP-based routing.

## Public API surface

The proxy module does not directly expose HTTP routes. It serves as
the SIP transport and transaction layer that other modules (call, RWI,
handler) build upon.

## See also

- [../03-concepts/](../03-concepts/) — SIP concepts and B2BUA architecture
- [../05-integration/](../05-integration/) — Integration with external SIP carriers

---
**Status:** ✅ Shipped
**Source:** `src/proxy/`
**Related phases:** [Phase 5](../07-roadmap/phase-05-proxy.md), [Phase 8](../07-roadmap/phase-08-routing.md), [Phase 9](../07-roadmap/phase-09-security.md), [Phase 10](../07-roadmap/phase-10-nat.md)
**Last reviewed:** 2026-04-16

> TODO: deep-dive pending
