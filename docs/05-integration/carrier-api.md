# Carrier API

The `/api/v1/*` REST surface provides programmatic control over SuperSip.
This is the primary API for automation, billing integration, and carrier operations.

> For the console UI API, see [Console API](console-api.md).
> For outbound webhooks, see [HTTP Router](http-router.md) and [Webhooks](webhooks.md).

---

## Authentication

All endpoints require a Bearer token issued via the `api-key` CLI command. Tokens use the `rpbx_` prefix and are validated against SHA-256 hashes stored in the `rustpbx_api_keys` table.

```
Authorization: Bearer rpbx_<64-hex-chars>
```

Tokens are checked on every request with no caching layer -- revoking a key takes effect immediately. The middleware updates `last_used_at` asynchronously on each successful authentication.

Missing or invalid tokens return:

```json
{"error": "missing bearer token", "code": "unauthorized"}
```

---

## Pagination

List endpoints return a paginated envelope:

```json
{
  "items": [...],
  "page": 1,
  "page_size": 20,
  "total": 42
}
```

Query parameters: `?page=1&page_size=20`. Page size is clamped to a maximum of 200.

---

## Error Format

All errors use a consistent JSON envelope:

```json
{"error": "message describing the problem", "code": "error_code"}
```

| HTTP Status | Code | Meaning |
|-------------|------|---------|
| 400 | `bad_request` | Validation failure or malformed input |
| 401 | `unauthorized` | Missing or invalid Bearer token |
| 404 | `not_found` | Resource does not exist |
| 409 | `conflict` | Duplicate create or deletion blocked by references |
| 500 | `internal` | Unexpected server error |
| 501 | `not_implemented` | Endpoint exists but handler is stubbed |
| 503 | `unavailable` | Service temporarily unavailable |

---

## Endpoint Groups

| Group | Base Path | Status | Phase |
|-------|-----------|--------|-------|
| Gateways | `/api/v1/gateways` | Shipped | [1](../07-roadmap/phase-01-api-shell.md) |
| DIDs | `/api/v1/dids` | Shipped | [1](../07-roadmap/phase-01-api-shell.md) |
| CDRs | `/api/v1/cdrs` | Shipped | [1](../07-roadmap/phase-01-api-shell.md) |
| Diagnostics | `/api/v1/diagnostics` | Shipped | [1](../07-roadmap/phase-01-api-shell.md) |
| System | `/api/v1/system` | Shipped | [1](../07-roadmap/phase-01-api-shell.md) |
| Trunks | `/api/v1/trunks` | Shipped | [2](../07-roadmap/phase-02-trunk-groups.md) |
| Trunk Sub-Resources | `/api/v1/trunks/{name}/*` | Planned | [3](../07-roadmap/phase-03-trunk-sub-resources.md) |
| Active Calls | `/api/v1/calls` | Planned | [4](../07-roadmap/phase-04-active-calls.md) |
| Routing | `/api/v1/routing` | Planned | [6](../07-roadmap/phase-06-routing.md) |
| Webhooks | `/api/v1/webhooks` | Planned | [7](../07-roadmap/phase-07-webhooks.md) |
| Translations | `/api/v1/translations` | Planned | [8](../07-roadmap/phase-08-translations.md) |
| Manipulations | `/api/v1/manipulations` | Planned | [9](../07-roadmap/phase-09-manipulations.md) |
| Security | `/api/v1/security` | Planned | [10](../07-roadmap/phase-10-security.md) |
| Recordings | `/api/v1/recordings` | Planned | [12](../07-roadmap/phase-12-listeners-recordings.md) |
| Endpoints | `/api/v1/endpoints` | Planned | [13](../07-roadmap/phase-13-cpaas.md) |
| Applications | `/api/v1/applications` | Planned | [13](../07-roadmap/phase-13-cpaas.md) |
| Sub-Accounts | `/api/v1/sub-accounts` | Planned | [13](../07-roadmap/phase-13-cpaas.md) |

---

## Shipped Endpoints

### Gateways

CRUD for SIP gateways (upstream carriers). Gateways are the transport-level entries that trunk groups reference as members.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/gateways` | List all gateways |
| GET | `/api/v1/gateways/{name}` | Get gateway by name |
| POST | `/api/v1/gateways` | Create gateway (409 on duplicate) |
| PUT | `/api/v1/gateways/{name}` | Update gateway fields |
| DELETE | `/api/v1/gateways/{name}` | Delete gateway (409 if referenced by a DID) |

**Create request body:**

```json
{
  "name": "carrier-west",
  "display_name": "West Coast Carrier",
  "direction": "bidirectional",
  "sip_server": "sip.carrier-west.com",
  "transport": "udp",
  "auth_username": "user",
  "auth_password": "secret",
  "health_check_interval_secs": 30,
  "failure_threshold": 3,
  "recovery_threshold": 2,
  "is_active": true
}
```

**Response fields:** `name`, `display_name`, `direction`, `proxy_addr`, `transport`, `status`, `is_active`, `last_health_check_at`, `consecutive_failures`, `consecutive_successes`, `failure_threshold`, `recovery_threshold`, `health_check_interval_secs`.

### DIDs

CRUD for Direct Inward Dialing numbers. Numbers are normalized to E.164 format on create.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/dids` | List DIDs (paginated, filterable) |
| GET | `/api/v1/dids/{number}` | Get DID by number (URL-encoded `+`) |
| POST | `/api/v1/dids` | Create DID (409 on duplicate) |
| PUT | `/api/v1/dids/{number}` | Replace DID fields |
| DELETE | `/api/v1/dids/{number}` | Delete DID (204) |

**List filters:** `?trunk=<name>&q=<search>&unassigned=true&page=1&page_size=20`

**Create request body:**

```json
{
  "number": "+14155551234",
  "trunk_name": "carrier-west",
  "extension_number": "1001",
  "failover_trunk": "carrier-east",
  "label": "Main Office",
  "enabled": true
}
```

**Response fields:** `number`, `trunk_name`, `extension_number`, `failover_trunk`, `label`, `enabled`, `created_at`, `updated_at`.

### CDRs

Read and delete call detail records.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/cdrs` | List CDRs (paginated, filterable) |
| GET | `/api/v1/cdrs/{id}` | Get CDR by ID |
| DELETE | `/api/v1/cdrs/{id}` | Delete CDR (204, 404 if missing) |
| GET | `/api/v1/cdrs/{id}/recording` | Get recording (501 -- Phase 12) |
| GET | `/api/v1/cdrs/{id}/sip-flow` | Get SIP flow (501 -- Phase 12) |

**List filters:** `?direction=<inbound|outbound>&status=<status>&from_number=<num>&to_number=<num>&start_date=<iso>&end_date=<iso>&page=1&page_size=20`

**Response fields:** `id`, `call_id`, `direction`, `status`, `started_at`, `ended_at`, `duration_secs`, `from_number`, `to_number`, `sip_gateway`, `caller_uri`, `callee_uri`, `recording_url`, `created_at`.

### Diagnostics

Routing dry-run, SIP registration inspection, and system diagnostics.

| Method | Path | Description |
|--------|------|-------------|
| POST | `/api/v1/diagnostics/route-evaluate` | Dry-run routing match |
| POST | `/api/v1/diagnostics/trunk-test` | Probe gateway OPTIONS response |
| GET | `/api/v1/diagnostics/registrations` | List active SIP registrations |
| GET | `/api/v1/diagnostics/registrations/{user}` | Get single user registration |
| GET | `/api/v1/diagnostics/summary` | Aggregated diagnostics snapshot |

**Route evaluate request:**

```json
{
  "caller": "+14155551234",
  "destination": "+442079460123",
  "direction": "outbound"
}
```

**Trunk test request:**

```json
{"name": "carrier-west"}
```

**Trunk test response:**

```json
{"ok": true, "latency_ms": 42, "detail": "200 OK"}
```

### System

Health check and hot reload.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/system/health` | Uptime, DB status, active calls, version |
| POST | `/api/v1/system/reload` | Hot-reload trunks, routes, ACL (409 if already in progress) |

**Health response:**

```json
{
  "uptime_secs": 86400,
  "db_ok": true,
  "active_calls": 12,
  "version": "0.1.0"
}
```

**Reload response:**

```json
{
  "reloaded": ["trunks", "routes", "acl"],
  "steps": [
    {"step": "trunks", "elapsed_ms": 15, "changed_count": 3},
    {"step": "routes", "elapsed_ms": 8, "changed_count": 1},
    {"step": "acl", "elapsed_ms": 2, "changed_count": 0}
  ],
  "elapsed_ms": 25
}
```

### Trunks (Phase 2)

CRUD for trunk groups -- logical groupings of gateways with distribution modes.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/trunks` | List trunk groups (paginated, filterable) |
| GET | `/api/v1/trunks/{name}` | Get trunk group by name |
| POST | `/api/v1/trunks` | Create trunk group (400 on missing gateway, 409 on duplicate) |
| PUT | `/api/v1/trunks/{name}` | Update trunk group (full member replacement) |
| DELETE | `/api/v1/trunks/{name}` | Delete trunk group (409 if referenced by DID or route) |

**List filters:** `?direction=<dir>&q=<search>&page=1&page_size=20`

**Create request body:**

```json
{
  "name": "us-east-trunks",
  "display_name": "US East Carriers",
  "direction": "outbound",
  "distribution_mode": "round_robin",
  "members": [
    {"gateway_name": "carrier-west", "weight": 100, "priority": 0},
    {"gateway_name": "carrier-east", "weight": 50, "priority": 1}
  ],
  "is_active": true
}
```

**Distribution modes:** `round_robin`, `weight_based`, `hash_callid`, `hash_src_ip`, `hash_destination`, `parallel` (feature-gated).

**Response fields:** `name`, `display_name`, `direction`, `distribution_mode`, `members[]`, `credentials`, `acl`, `nofailover_sip_codes`, `is_active`, `created_at`, `updated_at`.

---

## Planned Endpoints

For planned endpoints, see the corresponding phase page in [Roadmap](../07-roadmap/index.md).

Key upcoming surfaces:

- **Phase 3** -- Trunk sub-resources: credentials, origination URIs, media config
- **Phase 4** -- Active calls: list, hangup, transfer, mute, play, speak, DTMF, record
- **Phase 6** -- Routing: tables CRUD, records CRUD, resolve dry-run
- **Phase 7** -- Webhooks: CRUD + HMAC-signed delivery pipeline
- **Phase 13** -- CPaaS: endpoints, applications, sub-accounts

---

## See Also

- [Console API](console-api.md) -- session-authenticated console UI API
- [Webhooks](webhooks.md) -- event delivery (CDR hooks + planned registry)
- [SDK Examples](sdk-examples.md) -- curl, Python, JavaScript quick-starts

---
**Status:** Partial (Phases 1-2 shipped, Phases 3-13 planned)
**Last reviewed:** 2026-04-16
