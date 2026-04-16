# SuperSip Configuration Guide

SuperSip configuration is split into several logical sections. The application loads its main configuration from `rustpbx.toml` by default, or from a path specified by the `--conf` argument. The format is **TOML**.

## Configuration Sections

| # | Section | File | Description |
|---|---------|------|-------------|
| 0 | [Overview & Concepts](../config/00-overview.md) | `00-overview.md` | File structure, reload behavior, generated configs |
| 1 | [Platform & Networking](../config/01-platform.md) | `01-platform.md` | HTTP, Logging, Database, RTP, NAT, ICE |
| 2 | [Proxy Core](../config/02-proxy-core.md) | `02-proxy-core.md` | Binding ports, Transport (UDP/TCP/TLS/WS), Concurrency, Modules |
| 3 | [Authentication & Users](../config/03-auth-users.md) | `03-auth-users.md` | User Backends (Memory, DB, HTTP), Locators, Realms |
| 4 | [Routing](../config/04-routing.md) | `04-routing.md` | Static Routes, Regex Matching, Rewrites, HTTP Dynamic Router |
| 5 | [Trunks & Queues](../config/05-trunks-queues.md) | `05-trunks-queues.md` | SIP Gateways, Load Balancing, Queue Strategies, Agent Management |
| 6 | [Media, Recording & CDR](../config/06-media-recording.md) | `06-media-recording.md` | Media Proxy, Recording Policies, Storage Backends (Local/S3) |
| 7 | [Addons, Console & Admin](../config/07-addons-admin-storage.md) | `07-addons-admin-storage.md` | Web Console, AMI, Archiving, Wholesale & Custom Addons |

## Minimal Configuration

The smallest usable `config.toml` to get SuperSip running with one extension:

```toml
http_addr = "0.0.0.0:8080"
database_url = "sqlite://rustpbx.sqlite3"

[console]
base_path = "/console"
allow_registration = false

[proxy]
addr = "0.0.0.0"
udp_port = 5060
modules = ["auth", "registrar", "call"]

[[proxy.user_backends]]
type = "memory"
users = [{ username = "1001", password = "password" }]

[sipflow]
type = "local"
root = "./config/cdr"
subdirs = "hourly"
```

This gives you:
- A SIP proxy on UDP port 5060
- A web console at `http://localhost:8080/console`
- One SIP extension (`1001`) with password `password`
- Local CDR storage in `./config/cdr/`

For production deployments, see the individual configuration sections linked above.

---
**Status:** ✅ Shipped
**Source:** `docs/configuration.md`
**Last reviewed:** 2026-04-16
