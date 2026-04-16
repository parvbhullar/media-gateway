# Code Map

SuperSip's source lives in `src/` with ~20 top-level modules.
See [Subsystems Reference](../04-subsystems/index.md) for detailed pages.

## Entry Points

- `src/bin/rustpbx.rs` — Main server binary
- `src/bin/sipflow.rs` — Standalone SipFlow recording service (requires `opus` feature)
- `src/app.rs` — Router construction + server startup (`create_router`, `run`)
- `src/lib.rs` — Crate root, all module exports

## Module Dependency Flow

```
bin/rustpbx.rs
  -> app.rs (CoreContext, AppState, create_router, run)
    -> handler/ (HTTP API: /ami/v1/*, /api/v1/*)
    -> console/ (Web UI, feature-gated on "console")
    -> proxy/server.rs (SIP server)
      -> proxy/call.rs -> call/ (application logic)
        -> media/ (RTP, codecs, recording)
      -> proxy/acl.rs (access control)
      -> proxy/auth.rs (SIP authentication)
      -> proxy/registrar.rs (SIP registration)
      -> proxy/presence.rs (presence/BLF)
    -> rwi/ (WebSocket control plane)
    -> addons/ (plugin system)
  -> models/ (SeaORM entities + migrations)
  -> callrecord/ (CDR pipeline)
  -> sipflow/ (packet capture)
  -> storage/ (S3/local abstraction)
  -> config.rs (TOML configuration)
  -> metrics.rs (Prometheus counters/gauges)
  -> observability.rs (OTel reload layer)
  -> tls_reloader.rs (hot-reload TLS certs after ACME renewal)
  -> services.rs (shared service utilities)
  -> preflight.rs (startup checks)
  -> license.rs (commercial addon licensing)
  -> fixtures.rs (seed data for demo/test)
  -> ip_detect.rs (public IP detection)
  -> config_merge.rs (multi-file config merging)
  -> upload_retry.rs (failed S3 upload retry scheduler)
  -> utils.rs (misc helpers)
  -> version.rs (build version + update checker)
```

## Key Architectural Patterns

- **AppState** — `Arc<AppStateInner>` shared across all handlers; holds `CoreContext` (config, DB, cancellation token, storage), `SipServer`, addon registry, and console state.
- **CoreContext** — Inner struct carrying config, `DatabaseConnection`, `CancellationToken`, callrecord sender, storage, and RWI references. Shared via `Arc`.
- **Addon trait** — Plugins register routes, sidebar items, locale directories, and initializers via `AddonRegistry`.
- **ProxyModule trait** — SIP proxy modules (`acl`, `auth`, `registrar`, `presence`, `call`) loaded from config and registered on `SipServerBuilder`.
- **UserBackend trait** — Pluggable authentication (memory, DB, HTTP, plain, extension).
- **View types** — Never serialize SeaORM `Model` directly; use view structs with `From<Model>` (e.g., `GatewayView`).
- **Adapter pattern** — Pure data functions with `pub(crate)` visibility, no `State<>` or `Response` in signatures. Both HTML console and JSON API handlers call the same data functions.
- **CallRecordManager** — Background task with hooks (`DatabaseHook`, `SipFlowUploadHook`) for post-call processing.
