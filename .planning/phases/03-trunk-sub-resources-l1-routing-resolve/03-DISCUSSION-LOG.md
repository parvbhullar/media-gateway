# Phase 3: Trunk Sub-Resources L1 & Routing Resolve - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-17
**Phase:** 03-trunk-sub-resources-l1-routing-resolve
**Areas discussed:** Credentials storage shape, Origination URIs storage shape, Media config storage shape, /routing/resolve internals

---

## Gray Area Selection

| Option | Description | Selected |
|--------|-------------|----------|
| Credentials storage shape | Promote Phase 2 JSON to typed multi-row | ✓ |
| Origination URIs storage shape | Net new — table vs JSON array | ✓ |
| Media config storage shape | JSON vs typed columns vs sister table | ✓ |
| /routing/resolve internals | Reuse match_invite vs new dispatcher | ✓ |

**User selected:** All 4 areas

---

## Credentials storage shape

| Option | Description | Selected |
|--------|-------------|----------|
| New `supersip_trunk_credentials` table | FK to trunk_group, UNIQUE (trunk_group_id, realm). Standard relational. | ✓ |
| Reshape JSON column to array | Keep JSON, shape `[{realm, username, password}]`. Awkward mutation in SeaORM. | |
| Reshape JSON column to map | Keep JSON, `{realm: {…}}`. Loses ordering. | |

**User's choice:** New table. (Originally suggested as `rustpbx_trunk_credentials`; user later corrected naming to `supersip_` prefix project-wide.)

### Phase 2 backwards compatibility

| Option | Description | Selected |
|--------|-------------|----------|
| Drop credentials from POST/PUT trunks, sub-resource only | Cleanest contract; matches CARRIER-API spec exactly. | ✓ |
| Keep on POST as initial seed, force sub-resource for updates | Compromise. | |
| Keep on POST/PUT as full-replace + sub-resource for incremental | Overlap may confuse clients. | |

**User's choice:** Drop from POST/PUT. Phase 2 tests will be split (ACL stays inline, credentials moves to sub-resource).

---

## Origination URIs storage shape

| Option | Description | Selected |
|--------|-------------|----------|
| New `supersip_trunk_origination_uris` table | Mirrors credentials decision; consistent. | ✓ |
| JSON array column on trunk_group | Lighter schema; awkward read-modify-write. | |
| Reuse trunk_group_members | Requires `gateway.origination_uri` column refactor. | |

**User's choice:** New table.

---

## Media config storage shape

| Option | Description | Selected |
|--------|-------------|----------|
| JSON column on trunk_group | Simplest for GET/PUT contract; Phase 5 enforcement deserializes on demand. | ✓ |
| Inlined typed columns (codecs, dtmf_mode, srtp, media_mode) | Easier indexed queries; more migration churn. | |
| 1:1 sister table `supersip_trunk_media_config` | Cleanest separation; adds JOIN to dispatch reads. | |

**User's choice:** JSON column.

---

## /routing/resolve internals

| Option | Description | Selected |
|--------|-------------|----------|
| Reuse `match_invite_with_trace`, project trace into JSON | Same code path as production = same correctness; constrains response shape. | ✓ |
| New lightweight resolver | Faster, but two paths to keep in sync. | |
| Reuse `match_invite` (no trace), return only chosen target | Misses dry-run debug use case. | |

**User's choice:** Reuse match_invite_with_trace.

### Request shape

| Option | Description | Selected |
|--------|-------------|----------|
| Match production InviteOption fields (caller, destination, src_ip?, headers?) | Operators can paste real call traces for debugging. | ✓ |
| Minimal {caller, destination} only | Can't dry-run hash_src_ip dispatch. | |
| Match Vobiz dry-run shape | Need to verify Vobiz spec first. | |

**User's choice:** Match production InviteOption fields.

---

## Mid-Discussion Naming Convention Change

**User instruction (mid-CONTEXT-write):** "all new names needs to be supersip instead of rustpbx"

**Captured as:** D-00 (project-wide naming convention) + deferred item for bulk rename of existing `rustpbx_*` tables. Applied to the 2 new tables in this phase (credentials, origination_uris) and noted as a forward-going convention for all future phases.

---

## Claude's Discretion

- Exact JSON serialization of RouteTrace events (use serde derive)
- N+1 vs batch on list endpoints (Phase 5 if perf matters)
- Whether to include `created_at` on sub-resource views (recommend yes)
- src_ip default when omitted from /resolve (whatever match_invite_with_trace tolerates)

## Deferred Ideas

- Capacity sub-resource → Phase 5
- ACL sub-resource → Phase 5
- Codec filter enforcement → Phase 5
- Routing tables CRUD → Phase 6
- Credentials encryption at rest → v2.1
- Codec name translation layer → Phase 5
- Bulk rustpbx_* → supersip_* rename → v2.1 candidate
