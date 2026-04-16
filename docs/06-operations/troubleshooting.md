# Troubleshooting

## SIP 401 Behind NAT / Docker

When SuperSip runs behind NAT or inside a Docker container, the realm in SIP authentication challenges may not match what the client expects. Set the realm explicitly to match your public IP:

```toml
[proxy]
realms = ["your-public-ip:5060"]
```

Also set `external_ip` so SIP/SDP advertises the correct address:

```toml
external_ip = "your-public-ip"
```

## Common SIP Error Codes

| Code | Meaning in SuperSip |
|------|---------------------|
| 401 | Unauthorized — check credentials or realm config |
| 403 | Forbidden — ACL blocked the request |
| 404 | Not Found — user not registered or route not found |
| 408 | Request Timeout — target didn't respond |
| 480 | Temporarily Unavailable — user offline |
| 486 | Busy Here — user on another call |
| 488 | Not Acceptable — codec mismatch |
| 503 | Service Unavailable — overloaded or flood-blocked |

## Debug Logging

Set verbose logging in config:

```toml
log_level = "debug"
```

This produces high-volume output and is not recommended for production. Use temporarily to diagnose specific issues, then revert to `"info"`.

## AMI Diagnostics

Use the AMI endpoints for runtime inspection:

- `GET /ami/v1/health` — server health status
- `GET /ami/v1/dialogs` — list active SIP dialogs
- `GET /ami/v1/transactions` — list active SIP transactions

## API Diagnostics

- `POST /api/v1/diagnostics/route-evaluate` — dry-run a routing decision
- `GET /api/v1/diagnostics/registrations` — list registered endpoints
- `GET /api/v1/diagnostics/trunk-test/{name}` — probe a gateway

## SipFlow Analysis

Captured SIP/RTP flows can be viewed in the web console under SipFlow, or queried via the standalone `sipflow` binary's HTTP API.

Configure SipFlow capture:

```toml
[sipflow]
type = "local"
root = "./config/sipflow"
subdirs = "hourly"
```

## Codec Mismatch (488 Not Acceptable)

If calls fail with 488, ensure both endpoints share at least one common codec. SuperSip defaults to G.711 PCMU. Check trunk and endpoint codec configurations.

## Registration Failures

If endpoints fail to register:

1. Verify credentials in user backends (memory, extension, or HTTP).
2. Check that `modules` includes `"registrar"` and `"auth"`.
3. Confirm `registrar_expires` is not set too low (default: 60 seconds).
4. Check ACL rules are not blocking the source IP.

---
**Status:** ✅ Shipped
**Last reviewed:** 2026-04-16
