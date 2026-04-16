# Addons

## What it does

The addons module implements SuperSip's plugin system. It defines the `Addon`
trait that plugins implement to register routes, sidebar items, call record
hooks, proxy server hooks, template directories, locale files, and injected
scripts. An `AddonRegistry` discovers and initializes all feature-gated addons
at startup. Addons are categorized as Community (free) or Commercial (licensed).

## Key types & entry points

- **`Addon`** (trait) — plugin interface with lifecycle methods: `initialize()`, `router()`, `sidebar_items()`, `call_record_hook()`, `proxy_server_hook()`, `seed_fixtures()`, `authenticate()`, `inject_scripts()`, `locales_dir()`. `src/addons/mod.rs`
- **`AddonRegistry`** — collects all registered addons, initializes them, merges their routers, and provides sidebar/template/script lookups. `src/addons/registry.rs`
- **`AddonInfo`** — metadata for marketplace display: id, name, description, enabled, category, bundle, developer, website, cost, screenshots. `src/addons/mod.rs`
- **`SidebarItem`** — sidebar menu entry: name, i18n key, icon (SVG), URL, permission. `src/addons/mod.rs`
- **`AddonCategory`** (enum) — `Community` or `Commercial`. `src/addons/mod.rs`
- **`ScriptInjection`** — URL pattern and script URL for page-specific JS injection. `src/addons/mod.rs`

## Sub-modules (bundled addons)

Each addon is feature-gated:

- `acme/` — ACME (Let's Encrypt) TLS certificate management (`addon-acme`)
- `archive/` — CDR archival and compression (`addon-archive`)
- `queue/` — Call queue management (always enabled)
- `transcript/` — Call transcription (`addon-transcript`)
- `voicemail/` — Voicemail system (`addon-voicemail`)
- `observability/` — OpenTelemetry integration
- `telemetry/` — Telemetry collection (`addon-telemetry`)
- `enterprise_auth/` — LDAP/SAML/OIDC authentication (`addon-enterprise-auth`)
- `ivr_editor/` — Visual IVR flow editor (`addon-ivr-editor`)
- `endpoint_manager/` — Endpoint provisioning (`addon-endpoint-manager`)

## Configuration

Addons are enabled/disabled via Cargo feature flags (e.g. `addon-acme`,
`addon-voicemail`). Individual addons may have their own config sections
(e.g. `[addon.queue]`, `[addon.transcript]`).

## Public API surface

Each addon may register its own routes via the `router()` method. These
are merged into the main application router at startup. Addon routes
typically appear under `/addons/<addon-id>/` or as nested console pages.

## See also

- [console.md](console.md) — Console UI that displays addon sidebar items
- [callrecord.md](callrecord.md) — CDR hooks provided by addons (transcript, archive)

---
**Status:** ✅ Shipped
**Source:** `src/addons/`
**Related phases:** (addons span multiple phases)
**Last reviewed:** 2026-04-16
