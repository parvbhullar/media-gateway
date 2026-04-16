# SuperSip — Developer & Integrator Wiki

SuperSip is a high-performance software-defined PBX built in Rust. It handles SIP
signalling, RTP media, real-time WebSocket integration (RWI), carrier management, and
AI-driven call control — all in a single binary with no external media servers.

## How to Read This Wiki

- **Integrators** — start at [02 Getting Started](02-getting-started/index.md), then
  move to [05 Integration](05-integration/index.md).
- **Developers** — start at [03 Concepts](03-concepts/index.md), then
  [04 Subsystems](04-subsystems/index.md) and [08 Contributing](08-contributing/index.md).

## Contents

| # | Section | Description |
|---|---------|-------------|
| 01 | [Overview](01-overview/index.md) | Architecture, editions, glossary |
| 02 | [Getting Started](02-getting-started/index.md) | Install, first call, first webhook |
| 03 | [Concepts](03-concepts/index.md) | Core ideas behind SuperSip |
| 04 | [Subsystems](04-subsystems/index.md) | Developer reference per module |
| 05 | [Integration](05-integration/index.md) | Connect your systems to SuperSip |
| 06 | [Operations](06-operations/index.md) | Deploy, configure, monitor, tune |
| 07 | [Roadmap](07-roadmap/index.md) | v2.0 Carrier Control Plane phases |
| 08 | [Contributing](08-contributing/index.md) | Setup, navigate, extend, test |

## Status Legend

Tags appear at three levels: page footer, section headings, and table rows.

| Tag | Meaning |
|-----|---------|
| ✅ Shipped | In `main`, tests passing, safe to depend on |
| 🟡 Partial | Exists but has known gaps (listed inline) |
| 🚧 In Progress | Active phase work underway |
| 📋 Planned | Spec'd in `.planning/phases/` but no code yet |
| 💭 Proposed | On the roadmap but not yet spec'd |
| ⚠️ Deprecated | Still works, scheduled for removal |

## Related Resources

- [Architecture diagram](architecture.svg)
- [Screenshots](screenshots/)
- [Configuration reference](config/)
