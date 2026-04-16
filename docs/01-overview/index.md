# Overview

SuperSip is a high-performance software-defined PBX built in Rust. Unlike traditional PBX systems that require C modules for customization, SuperSip externalizes all call control through three integration channels -- HTTP, WebSocket, and Webhook -- so that routing decisions, real-time media control, and event streams are programmable in any language. AI becomes a native participant in every call.

## Integration Channels

| Channel | Protocol | Purpose |
|---------|----------|---------|
| **Policy Decision** | HTTP Router | Real-time routing decisions: AI first, agent queue, IVR, or direct transfer |
| **Real-time Control** | RWI (WebSocket) | In-call control: listen, whisper, barge, transfer, hold, media injection |
| **Event Stream** | Webhook | Push CDR, queue status, and events to your CRM/ticketing system |

## Contents

- [Architecture](architecture.md) -- System architecture and component diagram
- [Editions](editions.md) -- SuperSip editions and feature matrix
- [Glossary](glossary.md) -- Terms and abbreviations used throughout the wiki

---
**Status:** Shipped
**Source:** `README.md`
**Last reviewed:** 2026-04-16
