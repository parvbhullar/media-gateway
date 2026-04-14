# Milestones

History of completed milestones for media-gateway.

## v1.0 — Stable Baseline (pre-gsd, inferred from codebase)

**Status:** Shipped (ungated, prior to gsd tracking)
**Shipped capabilities** (inferred from current `sip_fix` branch):

- rsipstack-based SIP proxy with B2BUA (`proxy/proxy_call/`, `proxy/server.rs`)
- Media bridge with RTP relay and codec negotiation (`media/`, `proxy/proxy_call/media_bridge.rs`)
- SIP-to-WebRTC and SIP-to-WebSocket bridge modes
- Session timer RFC 4028 (`proxy/proxy_call/session_timer.rs`)
- Parallel dialer for concurrent gateway attempts
- Console UI (HTML + SeaORM) covering sip_trunks, DIDs, routing, call records, diagnostics, settings, users, roles
- SQL storage via SeaORM + migrations (`models/`)
- AMI management router (`handler/ami.rs`) with `/health`, `/reload/*`, `/dialogs`, `/shutdown`
- `/api/v1/*` shell with Bearer auth middleware (Plan 0 — 3 routes wired)
- Addons: archive, queue, voicemail, telemetry, observability, endpoint_manager, acme, ivr_editor, enterprise_auth
- Call recording + sipflow capture
- Locator with webhook support (`proxy/locator_webhook.rs`)
- Docker packaging (Dockerfile, Dockerfile.commerce, cross-compilation for aarch64 and x86_64)

**Phase count:** —
**Plan count:** —

---

*Subsequent milestones begin gsd-tracked from v2.0.*
