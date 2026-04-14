# Carrier API Gap Closure — media-gateway

**Date:** 2026-04-14
**Status:** Design
**Scope:** Expose the 86-route carrier control plane from `docs/CARRIER-API.md` at `/api/v1/*` inside media-gateway, reusing existing `console/` and `proxy/` logic. No drastic changes to media-gateway's architecture — it stays Rust-only, SQL-backed, single-node.

---

## Context

Media-gateway already ships a carrier data plane (rsipstack-based proxy, SeaORM storage, console UI) but only 3 carrier API routes are wired: `GET /api/v1/gateways`, `GET /api/v1/gateways/{name}`, `POST /api/v1/diagnostics/trunk-test`. The spec in `docs/CARRIER-API.md` describes 86 routes across 13 groups. The console UI in `src/console/handlers/` already CRUDs most of the same entities via HTML routes — those handlers are the closest source of truth for the JSON contract.

This plan maps every spec route to what exists, then sequences the work so cheap wrappers ship first and greenfield rule engines ship last.

## Architectural divergences to accept

Three places where media-gateway deliberately departs from the spec shape. Each closes with a pragmatic workaround rather than a refactor.

1. **Single SIP stack, static transports.** `ProxyConfig` has one `udp_port/tcp_port/tls_port/ws_port` set ([config.rs:733-736](../../src/config.rs#L733-L736)). The 5 `/api/v1/endpoints` routes become a **read-only projection** of that config; write routes return `501 Not Implemented`.
2. **Trunk vs gateway conflation.** `sip_trunk` rows today serve both roles. The plan introduces a new `rustpbx_trunk_groups` + `rustpbx_trunk_group_members` pair so a "trunk" can be a named group of sip_trunk "gateways" without migrating existing rows.
3. **Single-node only.** `/api/v1/system/cluster` returns a hardcoded one-node response. Documented, not stubbed out.

## Executive gap matrix

| Group | Spec'd | Wired | Logic elsewhere | True missing |
|---|---|---|---|---|
| Endpoints | 5 | 0 | config.rs (static) | 5 (write routes become 501) |
| Gateways | 5 | 2 | console sip_trunk | 3 |
| Trunks + sub-resources | 19 | 0 | console sip_trunk, frequency_limit, acl | ~14 |
| DIDs | 5 | 0 | console did.rs | 0 |
| Routing | 9 | 0 | console routing.rs + proxy/routing/ | 2 |
| Translations | 5 | 0 | — | 5 |
| Manipulations | 5 | 0 | — | 5 |
| Active Calls | 6 | 0 | console call_control.rs | 2 |
| CDRs | 5 | 0 | console call_record.rs + callrecord/ | 0 |
| Webhooks | 4 | 0 | proxy/locator_webhook.rs (locator only) | 4 |
| Security | 6 | 0 | proxy/acl.rs (global static) | 6 |
| Diagnostics | 5 | 1 | console diagnostics.rs | 0 |
| System | 6 | 0 | ami.rs + metrics.rs + version.rs | 4 |

**Totals after citation review:** ~17 trivial wrappers, ~20 moderate adapters, ~49 new logic, 11 routes intentionally deferred or stubbed.

## Per-group detail

### 1. Endpoints (5 routes, all 🔴)
One proxy, static transports. GET list/GET one project `ProxyConfig` transports; POST/PUT/DELETE return `501`.

### 2. Gateways (5 routes, 2 ✅ / 3 🟡)
Wrappers over `console/handlers/sip_trunk.rs` create/update/delete.

### 3. Trunks + sub-resources (19 routes, 🟡/🔴)
New `trunk_groups` table. Sub-resources split by difficulty: credentials/origination_uris/media (schema-only) are cheap; acl/capacity/media-enforcement require proxy hot-path changes.

### 4. DIDs (5 routes, all 🟡)
Thin adapters over [console/handlers/did.rs:21-27](../../src/console/handlers/did.rs#L21-L27).

### 5. Routing (9 routes, 7 🟡 / 2 🔴)
Wrap `console/handlers/routing.rs` for tables. `POST /routing/resolve` is a shim over `proxy/routing/matcher.rs`. Records sub-routes need an adapter because console stores records as embedded documents, not rows.

### 6. Translations (5 routes, all 🔴)
Greenfield. New `rustpbx_translations` table + `proxy/translation/engine.rs` regex engine + pipeline hook before routing. Rewrites From/To numbers only. Direction filter (`inbound`/`outbound`/`both`).

### 7. Manipulations (5 routes, all 🔴)
Greenfield. Rule engine with conditions (`and`/`or` over caller_number / destination_number / trunk / header:* / var:*) and actions (`set_header`/`remove_header`/`set_var`/`log`/`hangup`/`sleep`). Pipeline hook after routing, before gateway dispatch.

### 8. Active Calls (6 routes, 4 🟡 / 2 🔴)
Wrap `console/handlers/call_control.rs`. `CallCommandPayload` enum needs new variants for mute/unmute/transfer if not already present; dispatched through `active_call_registry` → `proxy_call/session.rs`.

### 9. CDRs (5 routes, all 🟡)
Wrap `console/handlers/call_record.rs`. Pagination/filter parameters already close to spec.

### 10. Webhooks (4 routes, all 🔴)
Greenfield. New `rustpbx_webhooks` table + background processor consuming `callrecord/` events + HMAC-signed HTTP POST with 3 retries and disk fallback under `ProxyConfig.generated_dir`. Reuse retry pattern from `proxy/locator_webhook.rs`.

### 11. Security (6 routes, all 🔴)
Greenfield. Split into: firewall store (promote static `proxy/acl.rs` to DB-backed), flood tracker (in-memory sliding window), brute-force tracker (auth failure store), auto-blocks table, topology hiding (config flag over existing proxy_call logic).

### 12. Diagnostics (5 routes, 1 ✅ / 4 🟡)
Wrap `console/handlers/diagnostics.rs` (route_evaluate, trunk options, locator lookup). Add aggregated `/diagnostics/summary`.

### 13. System (6 routes, 1 🟡 / 5 🔴)
Collapse ami.rs `reload/*` endpoints into one `POST /system/reload`. `/system/info` shim over `version.rs`. `/system/config` returns non-sensitive subset of `ProxyConfig` + `system_config` rows. `/system/stats` JSON adapter over `metrics.rs` Prometheus registry. `/system/cluster` hardcoded.

## Phased sequence

Each phase is independently shippable. Rough estimates assume one engineer.

| Phase | Goal | Routes | Effort |
|---|---|---|---|
| 0 | Structural decisions (trunk model, endpoints shape, cluster) | 0 | ~1 day |
| 1 | API shell + cheap wrappers (Gateways writes, DIDs, CDRs, Diagnostics, System/health+reload) | ~17 | ~1 week |
| 2 | Trunk groups schema + core trunk CRUD | 6 | ~1 week |
| 3 | Trunk sub-resources layer 1 (credentials, origination_uris, media schema, routing resolve) | ~9 | ~1 week |
| 4 | Active calls + control commands | 6 | ~3 days |
| 5 | Per-trunk capacity + ACL + codec-filter enforcement | 5 | ~1 week |
| 6 | Routing records sub-routes + distribution modes | 3 | ~4 days |
| 7 | Webhook pipeline | 4 | ~1 week |
| 8 | Translations engine | 5 | ~1 week |
| 9 | Manipulations engine | 5 | ~1.5 weeks |
| 10 | Security suite | 6 | ~2 weeks |
| 11 | System polish (info, config, stats, cluster) | 4 | ~2 days |
| 12 | Endpoints read-only view + 501 writes | 5 | ~2 days |
| **Total** | | **~75 of 86** | **~10-12 weeks** |

## Routes intentionally deferred or stubbed

- **5 endpoint write routes** → `501 Not Implemented` (no multi-listener)
- **`/system/cluster`** → hardcoded single-node
- **`parallel` distribution mode** → flag-gated, distinct failure semantics need dedicated testing
- **Recording / sip-flow placeholders** (`/cdrs/{id}/recording`, `/cdrs/{id}/sip-flow`) → already `501` in the spec

That's 11 routes with deliberate partial coverage. The remaining 75 reach full parity.

## Risks

1. **ConsoleState ↔ AppState boundary.** Console handlers take `Arc<ConsoleState>`; api_v1 takes `AppState`. Mitigation: extract data-fetch fns keyed on `&DatabaseConnection` and call them from both layers. `AppStateInner.console` already carries an `Option<Arc<ConsoleState>>` ([app.rs:74-75](../../src/app.rs#L74-L75)) so handler-level reuse is also possible if a data-fetch extraction proves invasive.
2. **Proxy hot-path changes** (Phases 4, 5, 8, 9, 10). Each phase that touches `proxy_call/session.rs` or dispatch adds integration test requirements before merging.
3. **Spec contract drift.** Any JSON shape decision made in Phase 1 locks the contract. Keep phase-1 adapters strict — reject unknown fields with 400 rather than silently ignoring.

## Success criteria

- All 75 target routes pass contract tests matching CARRIER-API.md examples.
- No regression in existing console UI or proxy call path (confirmed via existing integration test suite).
- Deferred routes return the documented `501` or hardcoded response with clear body messaging.
- Each phase merges independently behind a feature-flagged router mount, so partial delivery is safe to ship.
