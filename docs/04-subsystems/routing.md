# Routing

## What it does

The routing module is the route matching engine within the proxy subsystem.
It evaluates incoming INVITE requests against configured routing rules,
applies number rewriting, selects target trunks with load balancing
(round-robin, priority, weight), and supports DID-based indexing and
HTTP-based external routing lookups.

## Key types & entry points

- **`match_invite()`** — main routing function: matches rules by priority, applies rewrites, selects trunk, returns `RouteResult`. `src/proxy/routing/matcher.rs`
- **`RouteRule`** — a single routing rule with match patterns, priority, action (route/reject/queue/application), rewrite rules, and trunk references. `src/proxy/routing/mod.rs`
- **`TrunkConfig`** — SIP trunk configuration: destination, credentials, transport, codecs, headers. `src/proxy/routing/mod.rs`
- **`RouteTrace`** — diagnostic trace of the routing decision: matched rule, selected trunk, rewrite operations. `src/proxy/routing/matcher.rs`
- **`RouteResourceLookup`** (trait) — async lookup for queue configs referenced by route rules. `src/proxy/routing/matcher.rs`
- **`ConfigOrigin`** (enum) — tracks whether routing config came from embedded TOML or an external file. `src/proxy/routing/mod.rs`

## Sub-modules

- `did_index.rs` — DID number index for fast DID-to-route lookups
- `http.rs` — HTTP-based external route lookups (`[proxy.http_router]`)
- `matcher.rs` — Core route matching logic, rewrite engine, trunk selection

**Match capabilities:** Routing rules support prefix matching, regex patterns,
exact match, numeric comparison, and HTTP query-based external routing. The
matcher processes rules in priority order and stops at the first match.

## Configuration

Config keys from `[proxy]`:

- `routes` — array of routing rules with match patterns, actions, and trunk references
- `trunks` — map of named trunk configurations
- `[proxy.http_router]` — HTTP-based external routing endpoint

## Public API surface

The routing module does not expose HTTP routes directly. It is invoked
by the proxy call layer during INVITE processing.

## See also

- [proxy.md](proxy.md) — Parent SIP proxy module
- [call.md](call.md) — Call layer that consumes routing results
- [../03-concepts/](../03-concepts/) — Routing concepts and rule syntax

---
**Status:** ✅ Shipped
**Source:** `src/proxy/routing/`
**Related phases:** [Phase 6](../07-roadmap/phase-06-routing.md), [Phase 8](../07-roadmap/phase-08-routing.md), [Phase 9](../07-roadmap/phase-09-security.md)
**Last reviewed:** 2026-04-16

> New in Phase 2: `trunk_group_resolver` for trunk group based routing.
