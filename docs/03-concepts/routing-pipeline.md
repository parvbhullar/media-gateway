# Routing Pipeline

When an INVITE arrives at SuperSip, it passes through a multi-stage
pipeline before the call is dispatched. This page describes each stage
and how they compose.

## Pipeline overview

```
INVITE arrives
    |
    v
1. ACL check  ───── IP allow/deny, User-Agent filter
    |
    v
2. Authentication ── Digest auth via user backends
    |
    v
3. Translations ──── Number rewrite (📋 Planned Phase 8)
    |
    v
4. Route evaluation ── Static rules, HTTP router, DID index
    |
    v
5. Trunk resolution ── Select target trunk(s), distribution mode
    |                   (🚧 In Progress Phase 2)
    v
6. Manipulations ──── SIP header rewrite (📋 Planned Phase 9)
    |
    v
7. Call dispatch ──── Create Leg B and bridge media
```

## Stage 1: ACL check

The `AclModule` (from `proxy/acl.rs`) runs first as a `ProxyModule`.
It evaluates:

- **IP ACL rules** — ordered allow/deny rules with CIDR support.
  Default policy: `allow all` then `deny all` (open by default).
- **User-Agent filtering** — whitelist and blacklist by UA string.
- **Trunk recognition** — if the source IP matches a configured trunk's
  `dest` or `inbound_hosts`, the request bypasses ACL and is tagged
  with a `TrunkContext` for downstream stages.

Denied requests are marked as spam and aborted before reaching
authentication.

## Stage 2: Authentication

The `AuthModule` (`proxy/auth.rs`) challenges non-trunk requests with
SIP Digest authentication (RFC 2617). Credentials are verified against
pluggable `AuthBackend` implementations backed by:

- TOML-configured users (`user_plain`)
- Database users (`user_db`)
- External HTTP webhooks (`user_http`)

Trunk-originated calls skip authentication since they were already
validated by IP in the ACL stage.

## Stage 3: Translations

> 📋 **Planned Phase 8** — Number translation rules (caller/callee
> rewrite before routing) are not yet implemented.

When available, this stage will apply regex-based number transformations
to normalize E.164 formats, strip/add prefixes, and rewrite caller ID
before the request enters route evaluation.

## Stage 4: Route evaluation

The route matcher (`proxy/routing/matcher.rs`) evaluates configured
`RouteRule` entries in priority order. Each rule specifies:

### Match conditions

Rules match on any combination of SIP fields:

| Field             | Example                |
|-------------------|------------------------|
| `from.user`       | `^1800.*`              |
| `to.user`         | `^\\+1415`             |
| `from.host`       | `trunk.carrier.com`    |
| `to.host`         | `sip.example.com`      |
| `request_uri.user`| `100`                  |
| `header.*`        | Any SIP header value    |

Source trunk filtering is also supported — rules can restrict to specific
trunk names or IDs, and filter by direction (inbound/outbound/any).

### Route actions

When a rule matches, one of these actions executes:

| Action        | Behaviour                                     |
|---------------|-----------------------------------------------|
| **Forward**   | Route to destination trunk(s)                  |
| **Reject**    | Return error response (configurable code)      |
| **Busy**      | Return 486 Busy Here                           |
| **Queue**     | Enqueue call with hold music and dial strategy  |
| **Application** | Hand off to a call application (IVR, voicemail) |

### Destination selection

When forwarding to multiple trunks, the `select` field controls
distribution:

| Mode   | Behaviour                                       |
|--------|-------------------------------------------------|
| `rr`   | Round-robin (default)                            |
| `hash` | Consistent hash on a configurable key            |
| `wrr`  | Weighted round-robin using trunk `weight` values |

### Rewrite rules

Each route can include a `rewrite` block that modifies From, To,
Request-URI fields, and arbitrary headers before dispatch.

### Additional route sources

Beyond static TOML rules, routes can come from:

- **HTTP router** (`proxy/routing/http.rs`) — external routing decisions
  via webhook.
- **DID index** (`proxy/routing/did_index.rs`) — fast prefix-based DID
  number lookup.
- **Database routes** — loaded via `ProxyDataContext` and hot-reloaded.

## Stage 5: Trunk resolution

Trunk configuration (`TrunkConfig`) defines the outbound gateway:

- `dest` / `backup_dest` — primary and failover SIP URIs.
- `username` / `password` — outbound registration credentials.
- `codec` — preferred codec list for this trunk.
- `max_calls` / `max_cps` — capacity limits.
- `direction` — inbound, outbound, or bidirectional.
- `recording` — per-trunk recording policy.

> 🚧 **In Progress Phase 2** — Trunk group management with distribution
> modes across multiple carriers is being built out.

## Stage 6: Manipulations

> 📋 **Planned Phase 9** — SIP header manipulation rules (add, remove,
> modify headers) will run after trunk selection to customize signalling
> per carrier.

## Stage 7: Call dispatch

Once routing is resolved, the `CallSession` creates Leg B:

1. Build `InviteOption` with resolved destination, credentials, and codecs.
2. Send INVITE on Leg B.
3. Bridge media streams between Leg A and Leg B via `MediaBridge`.
4. Enter the session event loop to manage the established call.

For queue actions, the call enters a `QueuePlan` with hold music, dial
strategy (sequential or parallel), and fallback handling before the
final bridge.

## Further reading

- [SIP & B2BUA](sip-and-b2bua.md) — dialog lifecycle
- [Media Fabric](media-fabric.md) — how media is bridged
- [Routing subsystem](../04-subsystems/routing.md) — implementation details
- [HTTP Router integration](../05-integration/http-router.md) — webhook routing
