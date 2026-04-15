# SuperSip Wiki — Design

**Date:** 2026-04-16
**Branch:** `console_sip`
**Status:** Approved, pending implementation

## Context

SuperSip (formerly referred to as RustPBX in the codebase) is a Rust-based
software-defined PBX with a mature data plane (SIP proxy, media fabric,
SipFlow recording, console UI, RWI WebSocket interface) and an ambitious but
mostly unimplemented v2.0 Carrier Control Plane (13 phases, ~75 planned
`/api/v1/*` routes).

Existing documentation in [media-gateway/docs/](../) is a flat collection of
topic files written at different times for different audiences:
`api_integration_guide.md`, `rwi.md`, `configuration.md`, `observability.md`,
`cc.md`, `cc_ai_first_architecture.md`, `i18n.md`, `rwi_case.md`. There is no
unified wiki, no clear path from "I just installed this" to "I am extending
the proxy", and no way for a reader to see at a glance what is shipped vs.
what is planned.

This design replaces the flat docs layout with a layered, numbered wiki that
serves developers and integrators in one structure, absorbs all existing
content (no paraphrasing), and tags every feature with its shipped/planned
status so readers can tell reality from roadmap.

## Goals

1. One wiki that serves both developers extending SuperSip and integrators
   consuming it, without hard-splitting the material.
2. Honest status tagging at three granularities (page, section, row) so
   readers never confuse planned work for shipped features.
3. Absorb every existing doc file verbatim — no information is lost and no
   summary replaces authoritative text.
4. Roadmap phases surfaced inside the wiki so `.planning/` does not need to
   be opened to understand direction.
5. Every top-level `src/` module has a subsystem page — no silent gaps.
6. Nothing is invented. Where content is missing, pages carry explicit
   `TODO: not yet documented` markers.

## Non-Goals

- Deep call-flow / sequence diagrams for every subsystem (marked for a
  follow-up pass on 4–5 load-bearing modules).
- Auto-generated API reference from code (utoipa, etc.).
- Rewriting the repo-root [README.md](../../README.md).
- Translating the wiki itself (product-level i18n is absorbed as a concept
  page from `i18n.md`).

## Wiki Structure

```
media-gateway/docs/
├── README.md                          # Wiki entry point, TOC, status legend
├── 01-overview/
│   ├── index.md
│   ├── architecture.md
│   ├── editions.md
│   └── glossary.md
├── 02-getting-started/
│   ├── index.md
│   ├── install.md
│   ├── first-call.md
│   ├── first-webhook.md
│   └── first-rwi-session.md
├── 03-concepts/
│   ├── index.md
│   ├── sip-and-b2bua.md
│   ├── routing-pipeline.md
│   ├── media-fabric.md
│   ├── rwi-model.md
│   ├── recording-model.md
│   └── security-model.md
├── 04-subsystems/                     # Developer reference, shallow pages
│   ├── index.md
│   ├── proxy.md
│   ├── call.md
│   ├── media.md
│   ├── sipflow.md
│   ├── callrecord.md
│   ├── rwi.md
│   ├── console.md
│   ├── handler.md
│   ├── services.md
│   ├── storage.md
│   ├── routing.md
│   └── addons.md
├── 05-integration/                    # Integrator reference
│   ├── index.md
│   ├── http-router.md                 # absorbs api_integration_guide.md
│   ├── rwi-protocol.md                # absorbs rwi.md
│   ├── webhooks.md
│   ├── console-api.md                 # absorbs cc.md
│   ├── carrier-api.md
│   └── sdk-examples.md
├── 06-operations/
│   ├── index.md
│   ├── configuration.md               # absorbs configuration.md
│   ├── deployment.md
│   ├── observability.md               # absorbs observability.md
│   ├── tuning-and-capacity.md
│   └── troubleshooting.md
├── 07-roadmap/
│   ├── index.md
│   ├── phase-01-api-shell.md
│   ├── ... (13 phase pages)
│   └── phase-13-cpaas.md
└── 08-contributing/
    ├── index.md
    ├── dev-setup.md
    ├── code-map.md
    ├── adding-a-phase.md
    ├── testing.md
    └── style-guide.md
```

**Preserved (not touched):** [plans/](../plans/), [superpowers/](../superpowers/),
[screenshots/](../screenshots/), [config/](../config/), `architecture.svg`.

**Absorbed and deleted:** `api_integration_guide.md`, `rwi.md`,
`configuration.md`, `observability.md`, `cc.md`, `cc_ai_first_architecture.md`,
`i18n.md`, `rwi_case.md`.

## Status Tag System

Defined once in `docs/README.md`, referenced everywhere.

| Tag | Meaning |
|---|---|
| ✅ Shipped | Exists in `main`, tests passing, safe to depend on |
| 🟡 Partial | Exists but has known gaps — gaps listed inline |
| 🚧 In Progress | Active phase work, PR or branch underway |
| 📋 Planned | Spec'd in `.planning/phases/NN-*/` but no code yet |
| 💭 Proposed | Mentioned in roadmap but not yet spec'd |
| ⚠️ Deprecated | Still works, scheduled for removal |

**Three granularities:**

1. **Page-level** — in the status footer block.
2. **Section-level** — next to H2/H3 headings for major features.
3. **Row-level** — inside API tables, one tag per endpoint/command.

Every `📋 Planned` / `🚧 In Progress` tag links into `.planning/phases/NN-*/`.

## Page Template

Content-first; status context sits in a footer block.

```markdown
# <Name>

## What it does
## Architecture
## Key types & entry points
## Configuration
## Public API surface  (inline row-level tags)
## Extension points
## See also

---
**Status:** <page-level tag>
**Source:** `src/<module>/`
**Related phases:** <links>
**Last reviewed:** 2026-04-16
```

Concept pages use a lighter shape (What / Why / How / Gotchas / See also) and
carry no status tags — concepts are timeless.

Roadmap phase pages are one-to-one projections of `.planning/phases/NN-*/`:
goal, requirements, success criteria, affected subsystems with back-links,
current status. They re-expose the planning artifact inside the wiki so
readers never need to open `.planning/`.

## Content Derivation Rules

Priority order:

1. **Existing docs being absorbed** — moved verbatim, lightly reformatted,
   never paraphrased from memory.
2. **Planning artifacts** — `.planning/ROADMAP.md`, `PROJECT.md`,
   `REQUIREMENTS.md`, `phases/NN-*/`. Projected into roadmap pages.
3. **Source code** — top-of-file comments, `pub` items in `mod.rs`, axum
   router registrations, `config.rs`, `config.toml.example`. No invention.
4. **Honest gaps** — explicit `TODO: not yet documented` markers, not filler.

**What will NOT happen:**

- Inventing `/api/v1/*` request/response shapes for unimplemented routes.
- Deep sequence diagrams or call flows in this pass.
- Writing new concept docs where an absorbed doc covers the ground.
- Writing into `.planning/` — the wiki only reads from it.

**Delegation:** `/code-documentation:doc-generate` may be invoked for 4–5
load-bearing subsystems (proxy, routing, rwi, recording/sipflow, media) in a
follow-up pass. Remaining subsystems are derived shallowly inline.

## Authoring Sequence

Each step lands as its own atomic commit on `console_sip` with a conventional
commit message `docs(wiki): step N — <what>`.

1. Create the 8-directory skeleton + `docs/README.md` TOC + status legend.
2. Absorb existing docs into their new homes; delete the originals.
3. Draft `01-overview/` from repo README + `architecture.svg` + `PROJECT.md`.
4. Draft `07-roadmap/` phase pages from `ROADMAP.md` + `phases/NN-*/`.
5. Draft `04-subsystems/` shallow pages — one per top-level `src/` module.
6. Draft `02-getting-started/` from repo README quick-start + config examples.
7. Draft `03-concepts/` synthesized from absorbed docs + source.
8. Draft `05-integration/` cross-linked to subsystems (mostly absorbed).
9. Draft `06-operations/` from absorbed configuration + observability + bench.
10. Draft `08-contributing/` — dev setup + code-map pointer.

## Success Criteria

1. Every file in the old `docs/` root is either absorbed or deliberately
   preserved (plans/, superpowers/, screenshots/, config/, architecture.svg).
2. The 8-directory spine exists with an `index.md` in every directory.
3. Every feature/API page carries a status footer block; every API table row
   carries an inline status tag.
4. Every roadmap phase (1–13) has a page in `07-roadmap/`.
5. Every top-level `src/` module has a subsystem page in `04-subsystems/`.
6. Integration and subsystem pages cross-link in both directions.
7. The wiki is navigable without opening `.planning/`.
8. Nothing is invented — missing content is marked `TODO: not yet documented`.
9. Each authoring step lands as its own atomic commit.

## Deliverables

- ~60 new markdown files across the 8-directory tree
- ~8 existing docs absorbed and deleted
- 10 atomic commits on `console_sip`
- This design document at `docs/plans/2026-04-16-media-gateway-wiki-design.md`

## Follow-Up Work (Out of Scope)

- Deep-dive expansions for proxy / routing / rwi / recording / media with
  sequence diagrams via `/code-documentation:doc-generate`.
- Auto-generated API reference from utoipa annotations.
- Harmonizing voice across absorbed content in a later editorial pass.
