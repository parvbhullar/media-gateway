# Getting Started

Get SuperSip running and make your first call in under 10 minutes.

## Prerequisites

| Requirement | Notes |
|-------------|-------|
| **Docker** (recommended) or Rust toolchain | Docker is the fastest path; build from source if you prefer |
| **SIP client** | [Ooh! SIP](https://ooh.sip), [Linphone](https://www.linphone.org/), or the built-in WebRTC phone |
| **Ports 5060 + 8080** available | SIP proxy (UDP 5060) and web console / RWI (TCP 8080) |

## Contents

| Guide | Description |
|-------|-------------|
| [Install](install.md) | Docker quick-start, build from source, first admin account |
| [First Call](first-call.md) | Register two SIP users and place a call between them |
| [First Webhook](first-webhook.md) | Wire up the HTTP Router to route calls via your own API |
| [First RWI Session](first-rwi-session.md) | Open a WebSocket and control calls in real-time |
