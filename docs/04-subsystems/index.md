# Subsystems Reference

Developer reference for SuperSip's core modules. Each page covers purpose,
key types, entry points, configuration, and roadmap status.

| Module | Source | Description |
|--------|--------|-------------|
| [Proxy](proxy.md) | `src/proxy/` | SIP stack, B2BUA, registration, NAT |
| [Call](call.md) | `src/call/` | Call application logic, state machine, domain models |
| [Media](media.md) | `src/media/` | RTP, codecs, WebRTC bridging, conferencing |
| [SipFlow](sipflow.md) | `src/sipflow/` | Unified SIP+RTP packet capture |
| [Call Record](callrecord.md) | `src/callrecord/` | CDR generation, hooks, storage |
| [RWI](rwi.md) | `src/rwi/` | Real-time WebSocket call control |
| [Console](console.md) | `src/console/` | Web management UI |
| [Handler](handler.md) | `src/handler/` | HTTP API layer (AMI + v1) |
| [Services](services.md) | `src/services/` | Reserved for future abstractions |
| [Storage](storage.md) | `src/storage/` | Object storage (local + S3) |
| [Routing](routing.md) | `src/proxy/routing/` | Route matching engine |
| [Addons](addons.md) | `src/addons/` | Plugin system |
| [Models](models.md) | `src/models/` | Database entities + migrations |
| [Upload Retry](upload-retry.md) | `src/upload_retry/` | Background retry scheduler |
