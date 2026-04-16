# SuperSip Console & Admin API

This page covers the inbound REST APIs that your systems use to control SuperSip: active call management, CRUD operations on extensions/trunks/routes, and the low-level AMI interface.

> For outbound webhooks (HTTP Router, User Backend, Locator, CDR Push), see [http-router.md](http-router.md).

---

**Base URL**: `http://<supersip-ip>:8080/console`  
**Authentication**: Session cookie (login via `/console/login`) or API Token (future).

## Active Call Control

Manage calls that are currently in progress.

**List Active Calls**:
`GET /console/calls/active`

**Control a Call**:
`POST /console/calls/active/{call_id}/commands`

**Payloads**:
1. **Hangup**:
   ```json
   { "action": "hangup", "reason": "admin_kick" }
   ```
2. **Blind Transfer**:
   ```json
   { "action": "transfer", "target": "sip:1002@pbx.com" }
   ```
3. **Mute/Unmute**:
   ```json
   { "action": "mute", "track_id": "audio-0" } // use 'unmute' to reverse
   ```
4. **Force Answer** (for ringing channels):
   ```json
   { 
     "action": "accept", 
     "sdp": "v=0..." // Server-generated SDP answer
   }
   ```

## System Management (CRUD)

| Resource | Endpoint | Methods | Description |
| :--- | :--- | :--- | :--- |
| **Extensions** | `/console/extensions` | `GET`, `POST`, `PUT`, `DELETE` | Manage SIP users |
| **Trunks** | `/console/sip-trunk` | `GET`, `POST`, `PUT`, `DELETE` | Manage upstream carriers |
| **Routes** | `/console/routing` | `GET`, `POST`, `PUT`, `DELETE` | Manage dial plan rules |
| **CDRs** | `/console/call-records` | `GET`, `POST` (Search) | Query history |
| **Recording** | `/console/call-records/{id}/recording` | `GET` | Stream audio file |
| **SIP Flow** | `/console/call-records/{id}/sip-flow` | `GET` | Get PCAP-like ladder diagram JSON |

## AMI (Admin Interface)

Low-level system operations. Protected by IP whitelist (`[ami].allows` in config).

**Base URL**: `http://<supersip-ip>:8080/ami/v1`

- **Health**: `GET /health` - System vital stats (uptime, active calls, load).
- **Reload**: `POST /reload/trunks`, `/reload/routes`, `/reload/acl` - Hot reload config without restart.
- **Shutdown**: `POST /shutdown` - Graceful shutdown (stops accepting new calls, waits for active ones).
- **Dialogs**: `GET /dialogs` - Raw dump of internal SIP dialog states (for debugging).

---
**Status:** ✅ Shipped
**Source:** `docs/api_integration_guide.md`
**Last reviewed:** 2026-04-16
