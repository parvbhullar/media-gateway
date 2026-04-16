# Handler

## What it does

The handler module is the HTTP API layer for SuperSip. It hosts two
router trees: the AMI (Asterisk Manager Interface compatibility) router
at `/ami/v1/*` and the carrier/admin API router at `/api/v1/*`. Shared
middleware handles authentication, client address extraction, and
request logging.

## Key types & entry points

- **`ami_router()`** — builds the `/ami/v1/*` router for AMI-compatible endpoints. `src/handler/ami.rs`
- **`api_v1_router()`** — builds the `/api/v1/*` router with Bearer-token authentication and nested sub-routers for gateways, DIDs, CDRs, diagnostics, system, and trunks. `src/handler/api_v1/mod.rs`

## Sub-modules

- `ami.rs` — AMI-compatible HTTP API
- `api_v1/` — Carrier API v1:
  - `auth.rs` — Bearer-token authentication middleware (`api_v1_auth_middleware`)
  - `gateways.rs` — Gateway health and status endpoints
  - `dids.rs` — DID management endpoints
  - `cdrs.rs` — Call detail record query endpoints
  - `diagnostics.rs` — System diagnostics endpoints
  - `system.rs` — System configuration endpoints
  - `trunks.rs` — SIP trunk management endpoints
  - `reload_steps.rs` — Hot-reload step execution
  - `common.rs` — Shared types and helpers
  - `error.rs` — Error envelope types
- `middleware/` — Shared middleware:
  - `ami_auth.rs` — AMI authentication middleware
  - `clientaddr.rs` — Client IP address extraction
  - `request_log.rs` — Request/response logging

## Configuration

No dedicated config section. API authentication uses API keys stored in
the database (`api_key` model). AMI authentication uses the `[ami]`
section. All phases add routes to these routers.

## Public API surface

| Path prefix | Auth | Description |
|-------------|------|-------------|
| `/ami/v1/*` | AMI token | AMI-compatible management API |
| `/api/v1/gateways` | Bearer token | Gateway health and status |
| `/api/v1/dids` | Bearer token | DID management |
| `/api/v1/cdrs` | Bearer token | Call detail records |
| `/api/v1/diagnostics` | Bearer token | System diagnostics |
| `/api/v1/system` | Bearer token | System configuration |
| `/api/v1/trunks` | Bearer token | SIP trunk management |

## See also

- [console.md](console.md) — Web UI (separate from API)
- [rwi.md](rwi.md) — WebSocket API for real-time call control

---
**Status:** 🟡 Partial
**Source:** `src/handler/`
**Related phases:** All phases add routes here
**Last reviewed:** 2026-04-16
