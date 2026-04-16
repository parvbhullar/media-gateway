# Console

## What it does

The console module provides a web-based management UI for SuperSip.
It is feature-gated and delivers a server-rendered HTML interface for
managing extensions, SIP trunks, routing rules, DIDs, call records,
presence, system settings, diagnostics, and addon configuration.
The template system uses Minijinja with Alpine.js for client-side
interactivity and supports i18n with multiple locales.

## Key types & entry points

- **`ConsoleState`** — shared state for all console handlers: database connection, config, session key, SIP server reference, app state, i18n, RBAC permission cache. `src/console/mod.rs`
- **`RenderTemplate`** — Minijinja template renderer used by all handlers. `src/console/middleware.rs`
- **`I18n`** — internationalization engine with translation loading, locale detection, and variable interpolation. `src/console/i18n.rs`

## Sub-modules

- `auth.rs` — Session-based authentication (login, logout, registration, password reset)
- `middleware.rs` — Template rendering middleware and `RenderTemplate` type
- `i18n.rs` — Internationalization engine
- `handlers/` — 18 handler modules:
  - `dashboard.rs` — Main dashboard view
  - `extension.rs` — Extension (user endpoint) management
  - `sip_trunk.rs` — SIP trunk CRUD
  - `routing.rs` — Route rule management
  - `did.rs` — DID number management
  - `call_record.rs` — CDR browser and detail view
  - `call_control.rs` — Live call control panel
  - `presence.rs` — User presence view
  - `user.rs` — User account management
  - `setting.rs` — System settings editor
  - `diagnostics.rs` — System diagnostics and health checks
  - `sipflow.rs` — SIP flow viewer
  - `addons.rs` — Addon marketplace and configuration
  - `notifications.rs` — System notification management
  - `metrics.rs` — System metrics dashboard
  - `forms.rs` — Shared form helpers
  - `licenses.rs` — License management (commerce feature)
  - `utils.rs` — Shared handler utilities

## Configuration

Config section `[console]` controls:

- `session_secret` — Session encryption key
- `base_path` — URL prefix (default `/console`)
- `allow_registration` — Enable self-registration
- `demo_mode` — Demo mode flag
- `locale_default` — Default locale
- `locales` — Available locale definitions
- `alpine_js`, `tailwind_js`, `chart_js` — CDN overrides for JS libraries

## Public API surface

All console routes are served under the configured `base_path` (default `/console`).
Routes include login, dashboard, CRUD pages for all entities, and settings.

## See also

- [handler.md](handler.md) — HTTP API layer (separate from console)
- [addons.md](addons.md) — Addon system that extends the console sidebar

---
**Status:** ✅ Shipped
**Source:** `src/console/`
**Related phases:** (core infrastructure)
**Last reviewed:** 2026-04-16
