# SuperSip Wiki Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Create a complete developer+integrator wiki in `docs/` with ~60 pages across an 8-directory layered spine, absorbing all existing docs, deriving subsystem pages from source, and projecting the 13-phase v2.0 roadmap.

**Architecture:** Numbered directory tree (`01-overview/` through `08-contributing/`). Every page uses a shared template with content-first layout and status footer block. Existing docs are moved verbatim into their new locations and deleted from the old location. Source-derived pages are shallow (key types, entry points, config keys) with `TODO: deep-dive pending` markers for future expansion.

**Tech Stack:** Markdown files, git commits. No build step. Product name is **SuperSip** throughout (not RustPBX).

**Important conventions:**
- Working directory for all commands: `/Users/parvbhullar/Drives/Vault/Projects/Unpod/super-voice/media-gateway`
- All git operations run inside the `media-gateway` repo (it has its own `.git`)
- Product name: **SuperSip** — replace "RustPBX" in all absorbed content
- Status tags: `✅ Shipped`, `🟡 Partial`, `🚧 In Progress`, `📋 Planned`, `💭 Proposed`, `⚠️ Deprecated`
- Page template: content sections first, status footer block at bottom separated by `---`
- Every `📋 Planned` tag must link to its phase page in `07-roadmap/`
- Chinese-language docs (`cc.md`, `cc_ai_first_architecture.md`) are absorbed as-is (not translated)
- `docs/config/` files (00-07) are absorbed into `06-operations/configuration.md` as sections
- Files to preserve untouched: `docs/plans/`, `docs/superpowers/`, `docs/screenshots/`, `docs/config/`, `docs/architecture.svg`
- **UPDATE**: `docs/config/` files should be preserved as-is AND referenced from `06-operations/configuration.md` (wrap and link for this subdirectory since it's already well-structured)

---

## Task 1: Directory Skeleton + README TOC + Status Legend

**Files:**
- Create: `docs/README.md`
- Create: `docs/01-overview/index.md`
- Create: `docs/02-getting-started/index.md`
- Create: `docs/03-concepts/index.md`
- Create: `docs/04-subsystems/index.md`
- Create: `docs/05-integration/index.md`
- Create: `docs/06-operations/index.md`
- Create: `docs/07-roadmap/index.md`
- Create: `docs/08-contributing/index.md`

**Step 1: Create all 8 directories with placeholder index.md files**

Each `index.md` gets a title + one-line description + "Contents" section with links to planned pages (using relative paths). Pages listed but not yet created should be marked `(pending)`.

**Step 2: Create `docs/README.md`**

This is the wiki entry point. Contents:

```markdown
# SuperSip — Developer & Integrator Wiki

SuperSip is a high-performance software-defined PBX built in Rust. This wiki covers
everything from first install to extending the core.

## How to Read This Wiki

**Integrators** (deploying SuperSip, wiring it to your systems):
Start at [Getting Started](02-getting-started/index.md), then jump to
[Integration Guides](05-integration/index.md).

**Developers** (extending SuperSip itself):
Start at [Concepts](03-concepts/index.md), then explore
[Subsystems](04-subsystems/index.md) and [Contributing](08-contributing/index.md).

## Contents

| # | Section | Description |
|---|---------|-------------|
| 01 | [Overview](01-overview/index.md) | What SuperSip is, architecture, editions |
| 02 | [Getting Started](02-getting-started/index.md) | Install, first call, first webhook, first RWI session |
| 03 | [Concepts](03-concepts/index.md) | SIP/B2BUA, routing pipeline, media fabric, RWI model |
| 04 | [Subsystems](04-subsystems/index.md) | Developer reference — one page per core module |
| 05 | [Integration](05-integration/index.md) | HTTP Router, RWI protocol, webhooks, carrier API |
| 06 | [Operations](06-operations/index.md) | Configuration, deployment, observability, tuning |
| 07 | [Roadmap](07-roadmap/index.md) | v2.0 Carrier Control Plane — 13 phases |
| 08 | [Contributing](08-contributing/index.md) | Dev setup, code map, testing, style guide |

## Status Legend

Every feature, API endpoint, and subsystem page carries a status tag:

| Tag | Meaning |
|-----|---------|
| ✅ Shipped | In `main`, tests passing, safe to depend on |
| 🟡 Partial | Exists but has known gaps (listed inline) |
| 🚧 In Progress | Active phase work underway |
| 📋 Planned | Spec'd in `.planning/phases/` but no code yet |
| 💭 Proposed | On the roadmap but not yet spec'd |
| ⚠️ Deprecated | Still works, scheduled for removal |

Tags appear at three levels:
- **Page footer** — overall status of the subsystem/feature
- **Section headings** — status of major features within a page
- **Table rows** — per-endpoint or per-command status in API tables

## Related Resources

- [Architecture Diagram](architecture.svg)
- [Screenshots](screenshots/)
- [Configuration Reference](config/)
- [Planning Artifacts](../. planning/ROADMAP.md) (internal)
```

**Step 3: Commit**

```bash
git add docs/README.md docs/01-overview/index.md docs/02-getting-started/index.md \
  docs/03-concepts/index.md docs/04-subsystems/index.md docs/05-integration/index.md \
  docs/06-operations/index.md docs/07-roadmap/index.md docs/08-contributing/index.md
git commit -m "docs(wiki): step 1 — directory skeleton, README TOC, status legend"
```

---

## Task 2: Absorb Existing Docs

**Files:**
- Move content from: `docs/api_integration_guide.md` → `docs/05-integration/http-router.md` + `docs/05-integration/console-api.md`
- Move content from: `docs/rwi.md` → `docs/05-integration/rwi-protocol.md`
- Move content from: `docs/rwi_case.md` → append to `docs/05-integration/rwi-protocol.md` (as "Common Use Cases" section)
- Move content from: `docs/cc.md` → `docs/03-concepts/call-center.md`
- Move content from: `docs/cc_ai_first_architecture.md` → `docs/03-concepts/ai-first-architecture.md`
- Move content from: `docs/configuration.md` → `docs/06-operations/configuration.md`
- Move content from: `docs/observability.md` → `docs/06-operations/observability.md`
- Move content from: `docs/i18n.md` → `docs/03-concepts/i18n.md`
- Delete originals after moving

**Step 1: Read each source file and write to its new location**

For each file:
1. Read the full content
2. Replace "RustPBX" with "SuperSip" throughout (case-sensitive, preserve "rustpbx" in code/config identifiers like `rustpbx.sqlite3`, `rustpbx_trunk_groups`, crate names)
3. Add the status footer block at the bottom:
   ```markdown
   ---
   **Status:** ✅ Shipped
   **Source:** (original file path)
   **Last reviewed:** 2026-04-16
   ```
4. Write to the new location
5. Fix any internal cross-references (e.g., links to `rwi.md` become `rwi-protocol.md`)

**Content routing rules:**
- `api_integration_guide.md`: Split into two files:
  - HTTP Router, User Backend, Locator Webhook, CDR Event Push sections → `05-integration/http-router.md`
  - Active Call Control API, System Management, AMI sections → `05-integration/console-api.md`
- `rwi.md` + `rwi_case.md`: Merge into `05-integration/rwi-protocol.md` (rwi.md content first, then rwi_case.md as "## Common Use Cases" section at the end)
- `cc.md`: Absorb as `03-concepts/call-center.md` (Chinese, kept as-is)
- `cc_ai_first_architecture.md`: Absorb as `03-concepts/ai-first-architecture.md` (Chinese, kept as-is)
- `configuration.md`: Absorb as `06-operations/configuration.md` — this file is currently just a navigation index pointing to `docs/config/00-07`. Rewrite it as a proper overview page that embeds the navigation links to `../config/00-overview.md` through `../config/07-addons-admin-storage.md`
- `observability.md`: Absorb as `06-operations/observability.md`
- `i18n.md`: Absorb as `03-concepts/i18n.md`

**Step 2: Delete the original files**

```bash
git rm docs/api_integration_guide.md docs/rwi.md docs/rwi_case.md \
  docs/cc.md docs/cc_ai_first_architecture.md docs/configuration.md \
  docs/observability.md docs/i18n.md
```

**Step 3: Update index.md files**

Update `docs/03-concepts/index.md`, `docs/05-integration/index.md`, and `docs/06-operations/index.md` to list the newly created pages.

**Step 4: Commit**

```bash
git add docs/03-concepts/ docs/05-integration/ docs/06-operations/
git commit -m "docs(wiki): step 2 — absorb 8 existing docs into new wiki structure"
```

---

## Task 3: Draft 01-overview/

**Files:**
- Create: `docs/01-overview/architecture.md`
- Create: `docs/01-overview/editions.md`
- Create: `docs/01-overview/glossary.md`
- Modify: `docs/01-overview/index.md`

**Source material:**
- Repository `README.md` (lines 1-50, architecture section, editions table, core capabilities)
- `docs/architecture.svg` (reference, don't copy)
- `.planning/PROJECT.md` (architectural anchors, key decisions)

**Step 1: Write `01-overview/index.md`**

Derive from README.md lines 1-19. Replace "RustPBX" with "SuperSip". Include:
- What SuperSip is (1 paragraph from README intro)
- The three integration channels table (from README)
- Links to sub-pages

**Step 2: Write `01-overview/architecture.md`**

Derive from:
- README "AI-Native UCaaS Architecture" section — embed `![Architecture](../architecture.svg)`
- README "Core Capabilities" section — reorganize into layers: Access, Core, App Service
- `.planning/PROJECT.md` architectural anchors (one rsipstack, one DB, one binary, shared AppStateInner)

**Step 3: Write `01-overview/editions.md`**

Derive from README "Editions" table. Add the Community vs Commerce matrix verbatim. Add status tags per row.

**Step 4: Write `01-overview/glossary.md`**

Create a glossary of SIP/PBX/CPaaS terms used throughout the wiki:
- B2BUA, SDP, RTP, SRTP, DTMF, CDR, DID, IVR, ACD, RWI, SipFlow, WebRTC, SRTP, ACME, NAT, ICE, STUN, TURN, Codec, Trunk, Gateway, Queue, Hold Music, Ringback, REFER, 3PCC, INVITE, REGISTER, BYE, Dialplan, Manipulation, Translation

Each entry: term + one-line definition + link to the concept/subsystem page that covers it in depth.

**Step 5: Update `01-overview/index.md`** to link all sub-pages

**Step 6: Commit**

```bash
git add docs/01-overview/
git commit -m "docs(wiki): step 3 — 01-overview from README + PROJECT.md"
```

---

## Task 4: Draft 07-roadmap/ Phase Pages

**Files:**
- Create: `docs/07-roadmap/phase-01-api-shell.md`
- Create: `docs/07-roadmap/phase-02-trunk-groups.md`
- Create: `docs/07-roadmap/phase-03-trunk-sub-resources.md`
- Create: `docs/07-roadmap/phase-04-active-calls.md`
- Create: `docs/07-roadmap/phase-05-trunk-enforcement.md`
- Create: `docs/07-roadmap/phase-06-routing.md`
- Create: `docs/07-roadmap/phase-07-webhooks.md`
- Create: `docs/07-roadmap/phase-08-translations.md`
- Create: `docs/07-roadmap/phase-09-manipulations.md`
- Create: `docs/07-roadmap/phase-10-security.md`
- Create: `docs/07-roadmap/phase-11-system-polish.md`
- Create: `docs/07-roadmap/phase-12-listeners-recordings.md`
- Create: `docs/07-roadmap/phase-13-cpaas.md`
- Modify: `docs/07-roadmap/index.md`

**Source material:**
- `.planning/ROADMAP.md` (all 13 phases with goals, requirements, success criteria)
- `.planning/REQUIREMENTS.md` (requirement IDs referenced by each phase)
- `.planning/STATE.md` (current progress — Phase 1 verified, Phase 2 executing)
- `.planning/phases/01-*/` and `.planning/phases/02-*/` (CONTEXT.md, VERIFICATION.md, PLANs)

**Step 1: Write `07-roadmap/index.md`**

Overview page covering:
- v2.0 milestone goal (from ROADMAP.md overview)
- Phase dependency graph (text-based, showing which phases depend on which)
- Progress table with status tags:
  - Phase 1: `✅ Shipped` (verified 23/26 + 3 deferred)
  - Phase 2: `🚧 In Progress` (3 plans, executing)
  - Phases 3-13: `📋 Planned`
- Link to each phase page

**Step 2: Write one page per phase**

Each phase page follows this template:

```markdown
# Phase N: <Name>

## Goal
<from ROADMAP.md>

## Dependencies
<from ROADMAP.md "Depends on">

## Requirements
<bullet list of requirement IDs from ROADMAP.md, with one-line descriptions from REQUIREMENTS.md>

## Success Criteria
<numbered list from ROADMAP.md>

## Affected Subsystems
<bullet list linking to 04-subsystems/ pages that this phase touches>

## Plans
<for Phase 1 and 2: list the plan files with status>
<for Phases 3-13: "Plans not yet created">

---
**Status:** <tag based on STATE.md>
**Planning artifacts:** `.planning/phases/NN-*/`
**Last reviewed:** 2026-04-16
```

For **Phase 1**: Include a "Completion Summary" section noting 78/78 tests, the 3 deferred items, and gap closures from VERIFICATION.md.

For **Phase 2**: Include "Current Plans" listing 02-01, 02-02, 02-03 with their objectives.

For **Phases 3-13**: Use the goal, dependencies, requirements, and success criteria from ROADMAP.md. Mark all as `📋 Planned`.

**Step 3: Commit**

```bash
git add docs/07-roadmap/
git commit -m "docs(wiki): step 4 — 07-roadmap with 13 phase pages from .planning/"
```

---

## Task 5: Draft 04-subsystems/ Shallow Pages

**Files:**
- Create: `docs/04-subsystems/proxy.md`
- Create: `docs/04-subsystems/call.md`
- Create: `docs/04-subsystems/media.md`
- Create: `docs/04-subsystems/sipflow.md`
- Create: `docs/04-subsystems/callrecord.md`
- Create: `docs/04-subsystems/rwi.md`
- Create: `docs/04-subsystems/console.md`
- Create: `docs/04-subsystems/handler.md`
- Create: `docs/04-subsystems/services.md`
- Create: `docs/04-subsystems/storage.md`
- Create: `docs/04-subsystems/routing.md`
- Create: `docs/04-subsystems/addons.md`
- Create: `docs/04-subsystems/models.md`
- Create: `docs/04-subsystems/upload-retry.md`
- Modify: `docs/04-subsystems/index.md`

**Source material:** The module map gathered from `src/` — use the pub types, traits, entry points, and router registrations documented in the exploration agent's report.

**Step 1: Write `04-subsystems/index.md`**

Code map overview linking all subsystem pages with one-line descriptions:

```markdown
# Subsystems Reference

Developer reference for SuperSip's core modules. Each page covers purpose,
key types, entry points, configuration, and roadmap status.

| Module | Source | Description |
|--------|--------|-------------|
| [Proxy](proxy.md) | `src/proxy/` | SIP stack, B2BUA, registration, NAT |
| [Call](call.md) | `src/call/` | Call application logic, state machine, domain models |
| ... | ... | ... |
```

**Step 2: Write each subsystem page**

Each page follows the shared template. For each module, derive content from source:

**proxy.md** (`src/proxy/`, ~40 files):
- Purpose: SIP proxy server — registration, authentication, call routing, NAT, WebSocket SIP
- Key traits: `ProxyModule`, `UserBackend`
- Key types: `SipServerInner`, `SipServerBuilder`, `ProxyAction`
- Sub-modules: `routing/` (matchers, DID index, HTTP routing), `proxy_call/` (session, media bridge, state machine, session timer)
- Entry point: `server.rs` → `SipServer::start()`
- Config: `[proxy]` section — addr, ports, modules, user_backends, media_proxy, routes, trunks
- Roadmap: Phase 5 (trunk enforcement), Phase 8 (translations), Phase 9 (manipulations), Phase 10 (security)
- Mark `TODO: deep-dive pending` for call flow diagrams

**call.md** (`src/call/`, ~35 files):
- Purpose: Call application layer — dialplan, IVR, queue, conference, domain commands
- Key traits: `CallAppFactory`, `CallFailureHandler`, `RouteInvite`
- Key types: `Dialplan`, `DialplanFlow`, `RoutingState`, `Location`, `QueuePlan`
- Sub-modules: `domain/` (command, hangup, leg, policy, state), `runtime/` (app_runtime, command dispatch/executor, queue/conference managers), `app/` (controller, event loop, IVR, queue), `adapters/`
- Config: dialplan, ringback mode
- Roadmap: Phase 4 (active calls + mid-call control)
- Mark `TODO: deep-dive pending`

**media.md** (`src/media/`, ~20 files):
- Purpose: Media processing — codec negotiation, RTP bridging, recording, transcoding, conference mixing
- Key traits: `StreamWriter`, `Track`, `AudioSource`
- Key types: `MediaStream`, `RtcTrack`, `Recorder`, `MediaMixer`, `Transcoder`, `SdpBridge`
- Codecs: PCMU, PCMA, G722, G729, Opus (feature-gated), telephone-event
- Config: `[proxy]` media_proxy mode (auto/all/nat/none), codec list
- Mark `TODO: deep-dive pending`

**sipflow.md** (`src/sipflow/`, ~10 files):
- Purpose: Unified SIP+RTP packet capture for post-call analysis
- Key trait: `SipFlowBackend`
- Key types: `SipFlowItem`, `SipFlowQuery`, `StorageManager`, `ProcessedPacket`
- Backends: Local (SQLite), Remote (HTTP POST)
- Config: `[sipflow]` type, root, subdirs

**callrecord.md** (`src/callrecord/`, ~8 files):
- Purpose: CDR generation, formatting, hook pipeline, S3 upload
- Key traits: `CallRecordHook`, `CallRecordFormatter`
- Key types: `CallRecord`, `CallDetails`, `CallRecordManager`, `SipFlowUploadHook`
- Config: `[callrecord]` type, root
- Roadmap: Phase 7 (webhooks), Phase 11 (CDR export), Phase 12 (recordings first-class)

**rwi.md** (`src/rwi/`, ~11 files):
- Purpose: Real-time WebSocket Interface for programmatic call control
- Key types: `RwiAuth`, `RwiGateway`, `RwiSession`, `SmartRouter`, `TransferController`, `RuleExecutor`
- Key enums: `RwiCommand`, `RwiEvent`, `TransferMode`
- Sub-modules: auth, gateway, handler, processor, proto, routing, rule_engine, session, transfer
- Config: `[rwi]` tokens, contexts
- Cross-ref: `05-integration/rwi-protocol.md` for the full protocol spec
- Mark `TODO: deep-dive pending`

**console.md** (`src/console/`, ~25 files):
- Purpose: Built-in web management UI (feature-gated: "console")
- Key types: `ConsoleState`, `RenderTemplate`
- Handlers: 18 handler modules (addons, call_control, call_record, dashboard, diagnostics, did, extension, forms, licenses, metrics, notifications, presence, routing, setting, sip_trunk, sipflow, user, utils)
- Template system: Minijinja with Alpine.js frontend
- Config: `[console]` session_secret, base_path, allow_registration
- i18n: TOML-based translation files in `locales/`

**handler.md** (`src/handler/`, ~20 files):
- Purpose: HTTP API layer — AMI + v1 API + middleware
- Key routers: `ami_router()` at `/ami/v1/*`, `api_v1_router()` at `/api/v1/*`
- AMI endpoints: health, dialogs, hangup, transactions, reload_*
- API v1 modules: auth, cdrs, common, diagnostics, dids, error, gateways, reload_steps, system
- Middleware: ami_auth, clientaddr, request_log
- Roadmap: Every phase adds routes here

**services.md** (`src/services/`):
- Purpose: Reserved for future service abstractions
- Current state: Empty module (mod.rs is blank)
- Status: `💭 Proposed`

**storage.md** (`src/storage/`):
- Purpose: Object storage abstraction (local filesystem + S3-compatible)
- Key types: `Storage`, `StorageConfig`
- Vendors: AWS, GCP, Azure, Aliyun, Tencent, Minio, DigitalOcean
- Config: `storage_dir` or S3 vendor/bucket/credentials

**routing.md** (`src/proxy/routing/`):
- Purpose: Route matching engine — DID index, pattern matching, HTTP routing backend
- Sub-modules: `did_index`, `http` (HTTP routing backend), `matcher` (pattern evaluation)
- Match types: Lpm, ExactMatch, Regex, Compare, HttpQuery
- Config: `[proxy]` routes file, `[proxy.http_router]` url + timeout
- Roadmap: Phase 6 (routing tables CRUD), Phase 8 (translations), Phase 9 (manipulations)
- New in Phase 2: `trunk_group_resolver.rs` for distribution-mode dispatch

**addons.md** (`src/addons/`, ~50+ files):
- Purpose: Plugin system for extending SuperSip
- Key trait: `Addon` (id, name, initialize, router, sidebar_items)
- Key types: `AddonRegistry`, `AddonInfo`, `SidebarItem`, `ScriptInjection`
- Categories: Community, Commercial
- Bundled addons: acme (TLS), archive, queue, transcript, voicemail, observability, telemetry, enterprise_auth, ivr_editor, endpoint_manager
- Config: each addon has its own config section

**models.md** (`src/models/`, ~35 files):
- Purpose: SeaORM database entities + migrations
- Entities: api_key, call_record, department, did, extension, frequency_limit, policy, presence, rbac, routing, sip_trunk, system_config, system_notification, pending_upload, user, wholesale_agent
- Migration runner: `pub async fn create_db()`
- DB support: SQLite, MySQL, PostgreSQL
- Roadmap: Phase 2 adds trunk_group + trunk_group_member entities

**upload-retry.md** (`src/upload_retry/`):
- Purpose: Background retry scheduler for failed S3/storage uploads
- Key functions: `spawn()`, `sweep()`
- Behavior: 60s tick, 50 items/batch, 10 max attempts, exponential backoff
- Config: Automatic (no config keys, uses storage config)

**Step 3: Commit**

```bash
git add docs/04-subsystems/
git commit -m "docs(wiki): step 5 — 04-subsystems shallow pages for all src/ modules"
```

---

## Task 6: Draft 02-getting-started/

**Files:**
- Create: `docs/02-getting-started/install.md`
- Create: `docs/02-getting-started/first-call.md`
- Create: `docs/02-getting-started/first-webhook.md`
- Create: `docs/02-getting-started/first-rwi-session.md`
- Modify: `docs/02-getting-started/index.md`

**Source material:**
- Repository `README.md` (Quick Start, Build from Source, HTTP Router, Troubleshooting sections)
- `config.toml.example`
- `docs/05-integration/http-router.md` (absorbed from api_integration_guide.md)
- `docs/05-integration/rwi-protocol.md` (absorbed from rwi.md)

**Step 1: Write `02-getting-started/install.md`**

Derive from README "Quick Start (Docker)" + "Build from Source" sections:
- Docker pull (community + commerce images)
- Minimal `config.toml` (from README)
- docker run command
- Create first admin
- Build from source (dependencies + cargo build)
- Cross-compilation note
- Verify: web console URL + SIP proxy address

**Step 2: Write `02-getting-started/first-call.md`**

A minimal walkthrough:
1. Configure a SIP user in `config.toml` (memory backend, user 1001)
2. Point a SIP client (e.g., Ooh! SIP, Ooh! SIP, Ooh! SIP, Ooh! SIP or built-in WebRTC phone) at `udp://localhost:5060`
3. Register as user 1001
4. Place a call to another registered user (or echo test if available)
5. Check the CDR in the web console

Reference `06-operations/configuration.md` for full config details.

**Step 3: Write `02-getting-started/first-webhook.md`**

Derive from README "HTTP Router" section + absorbed `05-integration/http-router.md`:
1. Add `[proxy.http_router]` to config
2. Create a minimal webhook receiver (Python/curl example from api_integration_guide)
3. Place a call and observe the webhook fire
4. Return a routing decision
5. Link to `05-integration/http-router.md` for the full protocol reference

**Step 4: Write `02-getting-started/first-rwi-session.md`**

Derive from absorbed `05-integration/rwi-protocol.md`:
1. Configure RWI tokens in config
2. Connect via `wscat` or browser
3. Authenticate with Bearer token
4. Subscribe to a context
5. Originate a call
6. Observe events
7. Link to `05-integration/rwi-protocol.md` for the full protocol reference

**Step 5: Update `02-getting-started/index.md`**

```markdown
# Getting Started

Get SuperSip running and make your first call in under 10 minutes.

## Contents

1. [Install](install.md) — Docker or build from source
2. [First Call](first-call.md) — Register a SIP client and place a call
3. [First Webhook](first-webhook.md) — Wire up the HTTP Router
4. [First RWI Session](first-rwi-session.md) — Real-time call control via WebSocket

## Prerequisites

- Docker (recommended) or Rust toolchain (1.75+)
- A SIP client (Ooh! SIP, Ooh! SIP, Ooh! SIP, Ooh! SIP, Ooh! SIP) or the built-in WebRTC phone
- Port 5060 (SIP) and 8080 (HTTP) available
```

**Step 6: Commit**

```bash
git add docs/02-getting-started/
git commit -m "docs(wiki): step 6 — 02-getting-started from README quick-start + config"
```

---

## Task 7: Draft 03-concepts/

**Files:**
- Create: `docs/03-concepts/sip-and-b2bua.md`
- Create: `docs/03-concepts/routing-pipeline.md`
- Create: `docs/03-concepts/media-fabric.md`
- Create: `docs/03-concepts/rwi-model.md`
- Create: `docs/03-concepts/recording-model.md`
- Create: `docs/03-concepts/security-model.md`
- Modify: `docs/03-concepts/index.md` (already has call-center.md, ai-first-architecture.md, i18n.md from Task 2)

**Source material:**
- Absorbed docs (already in `03-concepts/`: cc.md → call-center.md, cc_ai_first_architecture.md → ai-first-architecture.md, i18n.md → i18n.md)
- `src/proxy/` module structure (for SIP/B2BUA concepts)
- `src/proxy/routing/` + `src/call/` (for routing pipeline)
- `src/media/` (for media fabric)
- `src/rwi/` (for RWI model)
- `src/sipflow/` + `src/callrecord/` (for recording model)
- `src/proxy/acl.rs` + addons/enterprise_auth (for security model)
- `05-integration/rwi-protocol.md` (absorbed RWI doc — reference, don't duplicate)

**Step 1: Write `03-concepts/sip-and-b2bua.md`**

Explain how SuperSip acts as a B2BUA (not a simple proxy):
- What B2BUA means — two independent call legs, full media control
- SIP dialog lifecycle in SuperSip (INVITE → provisional → 200 OK → ACK → BYE)
- How `proxy_call/session.rs` manages both legs
- Transport options (UDP, TCP, TLS, WebSocket, WebRTC)
- Registration flow via `registrar.rs` + user backends
- Link to `04-subsystems/proxy.md` for implementation details

**Step 2: Write `03-concepts/routing-pipeline.md`**

The full call-processing pipeline:
1. INVITE arrives at proxy
2. Authentication (user backends)
3. Translations (number rewrite — `📋 Planned Phase 8`)
4. Route evaluation (static routes, HTTP router, DID index)
5. Trunk group resolution (distribution modes — `🚧 In Progress Phase 2`)
6. Manipulations (SIP header rewrite — `📋 Planned Phase 9`)
7. Call dispatch to target
- Link to `04-subsystems/routing.md` and `05-integration/http-router.md`

**Step 3: Write `03-concepts/media-fabric.md`**

How SuperSip handles audio:
- RTP relay modes (auto, all, nat, none) — from `src/media/`
- Codec negotiation via `src/media/negotiate.rs`
- WebRTC ↔ SIP bridging via `src/media/sdp_bridge.rs`
- Conference mixing via `src/media/conference_mixer.rs`
- DTMF handling (in-band, RFC 2833, SIP INFO)
- SRTP and ICE support
- Link to `04-subsystems/media.md`

**Step 4: Write `03-concepts/rwi-model.md`**

The RWI conceptual model (NOT the protocol spec — that's in 05-integration):
- What RWI is — a JSON-over-WebSocket control plane for programmatic call control
- Command/event architecture with `action_id` correlation
- Context-based subscription model
- Smart routing and rule engine (three-layer architecture from rwi.md)
- How RWI relates to the rest of SuperSip (RwiGateway dispatches to proxy_call/session)
- Link to `05-integration/rwi-protocol.md` for the full spec

**Step 5: Write `03-concepts/recording-model.md`**

How recording and CDR work:
- SipFlow: unified SIP+RTP capture into hourly files (`src/sipflow/`)
- Call records: CDR generation pipeline (`src/callrecord/`)
- Recording hooks: `CallRecordHook` trait for post-call processing
- Storage backends: local filesystem, S3-compatible (AWS/GCP/Azure/etc.)
- Upload retry: background scheduler for failed uploads (`src/upload_retry/`)
- Transcription: SenseVoice offline transcription addon
- Roadmap: Phase 7 (webhook delivery), Phase 11 (CDR export), Phase 12 (recordings CRUD)
- Link to `04-subsystems/sipflow.md`, `04-subsystems/callrecord.md`

**Step 6: Write `03-concepts/security-model.md`**

Security layers:
- SIP authentication via user backends (memory, DB, HTTP, plain, extension)
- ACL (IP-based access control) via `src/proxy/acl.rs`
- RBAC (role-based access control) via `src/models/rbac.rs`
- Console authentication via `src/console/auth.rs`
- API authentication: Bearer tokens, API keys (`src/handler/api_v1/auth.rs`)
- TLS/SRTP with ACME auto-renewal (`src/addons/acme/`)
- Roadmap: Phase 10 (runtime firewall, flood/brute-force trackers, topology hiding)
- Link to `04-subsystems/proxy.md` (ACL), `04-subsystems/addons.md` (ACME)

**Step 7: Update `03-concepts/index.md`**

```markdown
# Concepts

Core ideas behind SuperSip. Read these to understand how the system works
before diving into subsystem details or integration guides.

## Contents

### Architecture
- [SIP & B2BUA](sip-and-b2bua.md) — How SuperSip handles SIP dialogs
- [Routing Pipeline](routing-pipeline.md) — From INVITE to call dispatch
- [Media Fabric](media-fabric.md) — RTP, codecs, WebRTC bridging, conferencing

### Control & Recording
- [RWI Model](rwi-model.md) — Real-time WebSocket call control concepts
- [Recording Model](recording-model.md) — CDRs, SipFlow, transcription

### Security
- [Security Model](security-model.md) — Auth, ACL, RBAC, TLS

### Domain Knowledge
- [Call Center](call-center.md) — CC requirements & architecture (中文)
- [AI-First Architecture](ai-first-architecture.md) — AI-native CC design (中文)
- [Internationalization](i18n.md) — Multi-language support system
```

**Step 8: Commit**

```bash
git add docs/03-concepts/
git commit -m "docs(wiki): step 7 — 03-concepts synthesized from source + absorbed docs"
```

---

## Task 8: Draft 05-integration/ (Remaining Pages)

**Files:**
- Create: `docs/05-integration/webhooks.md`
- Create: `docs/05-integration/carrier-api.md`
- Create: `docs/05-integration/sdk-examples.md`
- Modify: `docs/05-integration/index.md`

**Source material:**
- `.planning/ROADMAP.md` Phase 7 (webhooks), Phase 13 (CPaaS)
- `.planning/REQUIREMENTS.md` (WH-*, CALL-*, EPUA-*, APP-*)
- Absorbed docs already in `05-integration/`: http-router.md, rwi-protocol.md, console-api.md
- `src/handler/api_v1/` modules (existing endpoints)

**Step 1: Write `05-integration/webhooks.md`**

- Current state: CDR events pushed via `[callrecord]` config (`✅ Shipped`)
- Roadmap: Phase 7 webhook registry with HMAC, retries, disk fallback (`📋 Planned`)
- Document the existing CDR webhook format from the absorbed http-router.md
- List the planned webhook CRUD API endpoints with `📋 Planned` tags

**Step 2: Write `05-integration/carrier-api.md`**

The `/api/v1/*` REST surface — the centerpiece of the v2.0 roadmap:
- Overview of the carrier API vision (from ROADMAP.md)
- Table of ALL endpoint groups with status tags:

| Group | Endpoints | Status | Phase |
|-------|-----------|--------|-------|
| Gateways | CRUD `/api/v1/gateways` | ✅ Shipped | 1 |
| DIDs | CRUD `/api/v1/dids` | ✅ Shipped | 1 |
| CDRs | List/Get/Delete `/api/v1/cdrs` | ✅ Shipped | 1 |
| Diagnostics | Route-evaluate, registrations, probe | ✅ Shipped | 1 |
| System | Health, reload | ✅ Shipped | 1 |
| Trunks | CRUD `/api/v1/trunks` | 🚧 In Progress | 2 |
| Trunk Sub-Resources | credentials, origination URIs, media | 📋 Planned | 3 |
| Active Calls | List/control `/api/v1/calls` | 📋 Planned | 4 |
| Routing | CRUD `/api/v1/routing` | 📋 Planned | 6 |
| Webhooks | CRUD `/api/v1/webhooks` | 📋 Planned | 7 |
| Translations | CRUD `/api/v1/translations` | 📋 Planned | 8 |
| Manipulations | CRUD `/api/v1/manipulations` | 📋 Planned | 9 |
| Security | Firewall, flood, blocks | 📋 Planned | 10 |
| Recordings | CRUD `/api/v1/recordings` | 📋 Planned | 12 |
| Endpoints | CRUD `/api/v1/endpoints` | 📋 Planned | 13 |
| Applications | CRUD `/api/v1/applications` | 📋 Planned | 13 |
| Sub-Accounts | CRUD `/api/v1/sub-accounts` | 📋 Planned | 13 |

- For shipped endpoints: document request/response shapes (derive from `src/handler/api_v1/`)
- For planned endpoints: link to the corresponding phase page in `07-roadmap/`
- Authentication: Bearer token (from api_v1/auth.rs)
- Pagination: `{items, page, page_size, total}` envelope
- Error format: `{error, code, details}`

**Step 3: Write `05-integration/sdk-examples.md`**

Minimal examples showing how to integrate with SuperSip:
- curl examples for the shipped `/api/v1/*` endpoints (gateways, DIDs, CDRs, system health)
- Python example for the HTTP Router webhook handler (from absorbed api_integration_guide.md)
- JavaScript/wscat example for RWI connection (from absorbed rwi.md)
- Each example: 10-20 lines max, self-contained, runnable

**Step 4: Update `05-integration/index.md`**

```markdown
# Integration Guides

How to connect your systems to SuperSip.

## Contents

| Guide | Description | Status |
|-------|-------------|--------|
| [HTTP Router](http-router.md) | Dynamic call routing via webhooks | ✅ Shipped |
| [RWI Protocol](rwi-protocol.md) | Real-time WebSocket call control | ✅ Shipped |
| [Console API](console-api.md) | Active call control + system management | ✅ Shipped |
| [Carrier API](carrier-api.md) | `/api/v1/*` REST surface | 🟡 Partial |
| [Webhooks](webhooks.md) | CDR + event delivery | 🟡 Partial |
| [SDK Examples](sdk-examples.md) | curl, Python, JavaScript quick-starts | ✅ Shipped |
```

**Step 5: Commit**

```bash
git add docs/05-integration/
git commit -m "docs(wiki): step 8 — 05-integration remaining pages (webhooks, carrier-api, examples)"
```

---

## Task 9: Draft 06-operations/ (Remaining Pages)

**Files:**
- Create: `docs/06-operations/deployment.md`
- Create: `docs/06-operations/tuning-and-capacity.md`
- Create: `docs/06-operations/troubleshooting.md`
- Modify: `docs/06-operations/index.md`
- Modify: `docs/06-operations/configuration.md` (already created in Task 2, may need updates)

**Source material:**
- Repository `README.md` (Docker, build, troubleshooting, benchmark sections)
- `config.toml.example`
- `docs/config/00-07` files (referenced from configuration.md)
- README benchmark tables and scaling estimates

**Step 1: Write `06-operations/deployment.md`**

- Docker deployment (from README Quick Start)
- Docker Compose example (derive from README)
- Systemd unit file (reference `media-gateway` repo if one exists, otherwise note TODO)
- TLS/ACME setup (from addons/acme + config)
- Environment variables
- Dockerfile variants (Dockerfile, Dockerfile.commerce, Dockerfile.cross-*)

**Step 2: Write `06-operations/tuning-and-capacity.md`**

Derive from README "Benchmark" section:
- Full benchmark comparison table (verbatim from README)
- Per-channel overhead table
- Resource scaling estimates (the formulas)
- RTP port range tuning (`rtp_start_port`, `rtp_end_port`)
- Database tuning (SQLite WAL mode, MySQL connection pool)
- Media proxy mode selection (when to use auto vs all vs none)

**Step 3: Write `06-operations/troubleshooting.md`**

Derive from README "Troubleshooting" section + common operational knowledge:
- SIP 401 behind NAT/Docker (realm configuration)
- Common SIP error codes and what they mean in SuperSip context
- How to read SipFlow captures
- Debug logging (`log_level = "debug"`)
- AMI endpoints for runtime diagnostics (`/ami/v1/health`, `/ami/v1/dialogs`, `/ami/v1/transactions`)
- Link to `04-subsystems/handler.md` for the full AMI endpoint list

**Step 4: Update `06-operations/configuration.md`**

Ensure it properly references the `docs/config/` sub-directory files:

```markdown
# Configuration

SuperSip is configured via a TOML file (typically `config.toml`).

## Quick Reference

For the complete configuration reference, see the detailed guides:

| Section | Guide | Topics |
|---------|-------|--------|
| Overview | [00-overview](../config/00-overview.md) | Sources, precedence, reload behavior |
| Platform | [01-platform](../config/01-platform.md) | HTTP, logging, database, RTP/NAT |
| Proxy Core | [02-proxy-core](../config/02-proxy-core.md) | Listeners, transports, SIP identity |
| Auth & Users | [03-auth-users](../config/03-auth-users.md) | User backends, locator, webhooks |
| Routing | [04-routing](../config/04-routing.md) | Static routes, HTTP router |
| Trunks & Queues | [05-trunks-queues](../config/05-trunks-queues.md) | Trunk config, queue strategies |
| Media & Recording | [06-media-recording](../config/06-media-recording.md) | Media proxy, codecs, CDR storage |
| Addons & Admin | [07-addons-admin](../config/07-addons-admin-storage.md) | Console, AMI, storage, addons |

## Minimal Configuration

<embed the minimal config.toml from README>
```

**Step 5: Update `06-operations/index.md`**

```markdown
# Operations

Deploy, configure, monitor, and tune SuperSip.

## Contents

| Guide | Description |
|-------|-------------|
| [Configuration](configuration.md) | All config options (links to detailed reference) |
| [Deployment](deployment.md) | Docker, systemd, TLS/ACME |
| [Observability](observability.md) | Prometheus metrics, OpenTelemetry tracing |
| [Tuning & Capacity](tuning-and-capacity.md) | Benchmarks, scaling estimates, tuning knobs |
| [Troubleshooting](troubleshooting.md) | Common issues and debug techniques |
```

**Step 6: Commit**

```bash
git add docs/06-operations/
git commit -m "docs(wiki): step 9 — 06-operations deployment, tuning, troubleshooting"
```

---

## Task 10: Draft 08-contributing/

**Files:**
- Create: `docs/08-contributing/dev-setup.md`
- Create: `docs/08-contributing/code-map.md`
- Create: `docs/08-contributing/adding-a-phase.md`
- Create: `docs/08-contributing/testing.md`
- Create: `docs/08-contributing/style-guide.md`
- Modify: `docs/08-contributing/index.md`

**Source material:**
- Repository `README.md` (Build from Source, dependencies)
- `Cargo.toml` (features, dependencies)
- `.planning/` structure (for adding-a-phase)
- `src/` structure (for code-map)
- Test files in `src/` (for testing guide)

**Step 1: Write `08-contributing/dev-setup.md`**

- System dependencies (Linux: cmake, pkg-config, libasound2-dev, libssl-dev, libopus-dev; macOS: cmake, openssl, pkg-config)
- Clone + build (`cargo build --release`)
- Run with example config (`cargo run --bin rustpbx -- --conf config.toml.example`)
- Feature flags (from Cargo.toml: console, commerce features, parallel-trunk-dial)
- IDE setup tips (rust-analyzer recommended settings)

**Step 2: Write `08-contributing/code-map.md`**

Pointer into `04-subsystems/`:
```markdown
# Code Map

SuperSip's source lives in `src/` with 14 top-level modules.
See [Subsystems Reference](../04-subsystems/index.md) for detailed pages.

## Entry Points

- `src/bin/rustpbx.rs` — Main server binary
- `src/bin/sipflow.rs` — Standalone SipFlow recording service
- `src/app.rs` — Router construction + server startup
- `src/lib.rs` — Crate root, all module exports

## Module Dependency Flow

```
bin/rustpbx.rs
  → app.rs (CoreContext, AppState, create_router)
    → handler/ (HTTP API routes)
    → console/ (Web UI, feature-gated)
    → proxy/server.rs (SIP server)
      → proxy/call.rs → call/ (application logic)
        → media/ (RTP, codecs, recording)
      → proxy/routing/ (route matching)
    → rwi/ (WebSocket control plane)
    → addons/ (plugin system)
  → models/ (SeaORM entities + migrations)
  → callrecord/ (CDR pipeline)
  → sipflow/ (packet capture)
  → storage/ (S3/local abstraction)
```
```

**Step 3: Write `08-contributing/adding-a-phase.md`**

How the v2.0 development workflow works:
- `.planning/` directory structure (PROJECT.md, ROADMAP.md, REQUIREMENTS.md, STATE.md, phases/)
- Phase directory structure (NN-name/ with CONTEXT.md, PLANs, VERIFICATION.md)
- How to read a CONTEXT.md (boundary, decisions, files touched)
- How to read a PLAN.md (objectives, must-haves, artifacts, interfaces)
- How to add a new phase to ROADMAP.md
- The discuss → plan → execute → verify cycle

**Step 4: Write `08-contributing/testing.md`**

- Running the full test suite: `cargo test`
- Integration test conventions (from Phase 1 IT-01): 401/happy/404/400-409 per sub-router
- Test file locations: `src/*/tests/`, `tests/`
- How to add a test for a new API route
- Fixtures: `src/fixtures.rs`

**Step 5: Write `08-contributing/style-guide.md`**

- Rust style: standard rustfmt
- Naming: SeaORM entities use `rustpbx_` prefix, view types never serialize Model directly
- Error handling: `ApiError` constructors (bad_request, conflict, not_found, not_implemented, internal)
- Config: TOML sections match module names
- Adapter pattern (from SHELL-05): pure data fns with `pub(crate)`, no State/Response in signatures
- Commit messages: conventional commits (`feat`, `fix`, `docs`, `test`, `refactor`)

**Step 6: Update `08-contributing/index.md`**

```markdown
# Contributing

How to set up, navigate, extend, and test SuperSip.

## Contents

| Guide | Description |
|-------|-------------|
| [Dev Setup](dev-setup.md) | Dependencies, build, run, feature flags |
| [Code Map](code-map.md) | Entry points, module dependency flow |
| [Adding a Phase](adding-a-phase.md) | How v2.0 planning + execution works |
| [Testing](testing.md) | Test suite, conventions, fixtures |
| [Style Guide](style-guide.md) | Naming, errors, adapters, commits |
```

**Step 7: Commit**

```bash
git add docs/08-contributing/
git commit -m "docs(wiki): step 10 — 08-contributing dev setup, code map, testing, style guide"
```

---

## Final Verification

After all 10 tasks are complete, run a quick verification pass:

**Step 1: Check all files exist**

```bash
find docs/ -name "*.md" -not -path "docs/plans/*" -not -path "docs/superpowers/*" -not -path "docs/config/*" | sort | wc -l
```

Expected: ~60 files

**Step 2: Check no broken internal links**

```bash
# Find all markdown links and verify targets exist
grep -roh '\[.*\](\.\.*/[^)]*\.md)' docs/ | grep -oP '\(\.\.*/[^)]*\.md\)' | tr -d '()' | sort -u | while read link; do
  # Resolve relative to docs/
  if [ ! -f "docs/$link" ] && [ ! -f "$link" ]; then
    echo "BROKEN: $link"
  fi
done
```

**Step 3: Check old files are gone**

```bash
ls docs/api_integration_guide.md docs/rwi.md docs/cc.md docs/cc_ai_first_architecture.md \
   docs/configuration.md docs/observability.md docs/i18n.md docs/rwi_case.md 2>&1
```

Expected: all "No such file or directory"

**Step 4: Check preserved files still exist**

```bash
ls docs/architecture.svg docs/config/00-overview.md docs/screenshots/ docs/plans/ docs/superpowers/ 2>&1
```

Expected: all exist

**Step 5: Final commit (if any fixes needed)**

```bash
git add docs/
git commit -m "docs(wiki): final verification fixes"
```

---

## Summary

| Task | Step | What | Commit Message |
|------|------|------|----------------|
| 1 | 1 | Directory skeleton + README TOC + status legend | `docs(wiki): step 1 — directory skeleton, README TOC, status legend` |
| 2 | 2 | Absorb 8 existing docs | `docs(wiki): step 2 — absorb 8 existing docs into new wiki structure` |
| 3 | 3 | 01-overview/ | `docs(wiki): step 3 — 01-overview from README + PROJECT.md` |
| 4 | 4 | 07-roadmap/ phase pages | `docs(wiki): step 4 — 07-roadmap with 13 phase pages from .planning/` |
| 5 | 5 | 04-subsystems/ shallow pages | `docs(wiki): step 5 — 04-subsystems shallow pages for all src/ modules` |
| 6 | 6 | 02-getting-started/ | `docs(wiki): step 6 — 02-getting-started from README quick-start + config` |
| 7 | 7 | 03-concepts/ | `docs(wiki): step 7 — 03-concepts synthesized from source + absorbed docs` |
| 8 | 8 | 05-integration/ remaining | `docs(wiki): step 8 — 05-integration remaining pages` |
| 9 | 9 | 06-operations/ remaining | `docs(wiki): step 9 — 06-operations deployment, tuning, troubleshooting` |
| 10 | 10 | 08-contributing/ | `docs(wiki): step 10 — 08-contributing dev setup, code map, testing, style guide` |

Total: ~60 new markdown files, ~8 absorbed+deleted, 10 atomic commits.
