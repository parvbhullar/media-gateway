# Phase 2: Trunk Groups Schema & Core CRUD — Context

**Gathered:** 2026-04-16
**Status:** Ready for planning
**Source:** Upstream planning context + Phase 1 adapter convention

<domain>
## Phase Boundary

Phase 2 introduces a **new entity layer** — `rustpbx_trunk_groups` + `rustpbx_trunk_group_members` — above the existing `rustpbx_sip_trunks` table, ships the `/api/v1/trunks` CRUD surface, and wires the five ready distribution modes into the existing dispatch selector. The legacy `sip_trunk` row shape is untouched; this is additive only. Legacy `sip_trunk` → `gateway` semantics from Phase 1 remain the source of truth for per-PSTN-peer connectivity. A `trunk_group` is a logical **bundle** of one or more gateways with a distribution policy, credentials, ACL, and failover metadata.

**Route inventory for Phase 2:**

| Group | Routes | Source module |
|---|---|---|
| Trunk Groups core | `GET /api/v1/trunks`, `POST /api/v1/trunks`, `GET /api/v1/trunks/{name}`, `PUT /api/v1/trunks/{name}`, `DELETE /api/v1/trunks/{name}` (5) | NEW `src/handler/api_v1/trunks.rs` |

**Schema changes (TRK-01, MIG-01):**

- NEW table `rustpbx_trunk_groups` — one row per group, owns name/direction/distribution_mode/credentials/acl/nofailover_sip_codes
- NEW table `rustpbx_trunk_group_members` — (trunk_group_id, gateway_name, weight, priority, position)
- NEW nullable column `rustpbx_dids.trunk_group_name` — forward-reference for DID → trunk_group routing (exists in this phase so TRK-04 engagement check has a real target; populated by Phase 3+)
- Zero `ALTER` on `rustpbx_sip_trunks`
- Zero data copy from `rustpbx_sip_trunks`
- Migration registered in `src/models/migration.rs::Migrator::migrations` as a new `Box::new(super::trunk_group::Migration)` entry **appended to the end** of the vector (migrations run in order; appending preserves Phase 1's already-applied history)

**Out of scope for Phase 2** — these are deferred to the listed phases:

- Trunk sub-resource endpoints (credentials CRUD, origination URIs, media config, capacity, ACL sub-resource, nofailover codes sub-resource) — Phase 3 & 5 (TSUB-01..07). Phase 2 stores these as columns/JSON on the parent row per TRK-02 but ships NO sub-resource endpoints.
- Per-trunk-group capacity enforcement in the proxy hot path — Phase 5 (TSUB-04..07)
- Routing CRUD — Phase 6 (RTE-01..05). Phase 2's delete-409 check scans `rustpbx_routes.target_trunks` as a best-effort string scan with a documented Phase-6 follow-up.
- Active call dispatcher rewrite — Phase 2 extends the existing `select_trunk` path at `src/proxy/routing/matcher.rs:1004` with a trunk-group-aware projection; does NOT refactor the dispatcher.
</domain>

<decisions>
## Implementation Decisions (LOCKED)

### Adapter convention (inherits from Phase 1 SHELL-05 ADR)

- Phase 2 lives entirely in `src/handler/api_v1/trunks.rs` — a **new sub-router file** that mounts alongside `gateways`, `dids`, `cdrs`, `diagnostics`, `system` in `src/handler/api_v1/mod.rs::api_v1_router`.
- The model layer (`src/models/trunk_group.rs`, `src/models/trunk_group_member.rs`) is the shared sink between the new JSON handlers and any future console UI or routing pipeline consumers. **No console handler extraction** is required (legacy console reads `sip_trunk` directly and is untouched in this phase).
- `TrunkView` owns the wire format. `trunk_group::Model` is **never** serialized directly (SHELL-04).
- `ApiError::bad_request`, `::conflict`, `::not_found`, `::internal` are all already available from `src/handler/api_v1/error.rs` after Phase 1.
- `PaginatedResponse<TrunkView>` for list results from `src/handler/api_v1/common.rs`.
- Pattern reference: copy the structure of `src/handler/api_v1/gateways.rs` (list/get/create/update/delete + `trunk_by_name` helper + `validate_name` helper + engagement-tracked delete) and `src/handler/api_v1/dids.rs` (paginated list with filters).
- Bearer auth middleware is already layered at the root via `api_v1_router` — the new sub-router just needs to be `.merge()`-ed into the `protected` router.

### Bridging decision: trunk_group vs legacy sip_trunk coexistence

- Legacy `rustpbx_sip_trunks` rows continue to exist and Phase 1's `/api/v1/gateways` surface continues to operate on them. These are the "gateway" concept (a single PSTN peer).
- Phase 2's `rustpbx_trunk_groups` rows are a **new bundling concept** that references 1..N gateways by name via the member join table. Each member is `(trunk_group_id, gateway_name, weight, priority, position)`.
- Gateway names and trunk_group names share a string namespace at the routing dispatch layer but occupy **distinct tables**. Phase 2 enforces a hard uniqueness check: **creating a trunk group with a name that collides with an existing gateway name → 400 bad_request**, and vice versa is enforced in Phase 1's gateway create path as a follow-up (tracked as a TODO in the plan — Phase 1 is already shipped and cannot be retro-modified without a new gap-closure plan; Phase 2 handles its side of the check only).
- Dispatch (`select_trunk`) is extended so that a routing target that resolves to a trunk_group name expands to the group's member gateway names via a database-driven `TrunkConfig` HashMap build, then hands off to the existing `rr`/`hash`/`weighted` branches.
- Routing targets that resolve to a legacy `sip_trunk` name (the gateway's name) flow through the unchanged single-trunk code path.
- **No data migration.** No legacy `sip_trunk` row is auto-promoted into a trunk_group. Operators must explicitly create groups via `POST /api/v1/trunks` and add gateway members.

### Schema (TRK-01)

`rustpbx_trunk_groups`:

| Column | Type | Notes |
|---|---|---|
| `id` | `big_integer` PK auto_increment | — |
| `name` | `string char_len=120` UNIQUE | `^[a-zA-Z0-9_-]{1,64}$` enforced at handler layer |
| `display_name` | `string_null char_len=160` | — |
| `direction` | `string char_len=32` default `bidirectional` | Same enum as `sip_trunk.direction` — `inbound`/`outbound`/`bidirectional` |
| `distribution_mode` | `string char_len=32` default `round_robin` | Enum: `round_robin`, `weight_based`, `hash_callid`, `hash_src_ip`, `hash_destination`, `parallel` |
| `credentials` | `json_null` | Shape `{auth_username, auth_password, realm}` — stored for later sub-resource promotion in Phase 3 |
| `acl` | `json_null` | Shape `{allowed_cidrs: [..], denied_cidrs: [..]}` — stored for later promotion in Phase 5 |
| `nofailover_sip_codes` | `json_null` | `[i32]` — response codes that must NOT trigger failover |
| `is_active` | `boolean` default `true` | — |
| `metadata` | `json_null` | Free-form for forward compat |
| `created_at` | `timestamp` default CURRENT_TIMESTAMP | — |
| `updated_at` | `timestamp` default CURRENT_TIMESTAMP | — |

Indexes: unique on `name`, non-unique on `(direction, is_active)`.

`rustpbx_trunk_group_members`:

| Column | Type | Notes |
|---|---|---|
| `id` | `big_integer` PK auto_increment | — |
| `trunk_group_id` | `big_integer NOT NULL` | FK → `rustpbx_trunk_groups.id` ON DELETE CASCADE |
| `gateway_name` | `string char_len=120 NOT NULL` | Soft reference — no FK at DB layer to decouple from `rustpbx_sip_trunks.id` churn. Handler validates existence at write time. |
| `weight` | `integer` default `100` | Used by `weight_based` mode |
| `priority` | `integer` default `0` | Reserved for failover ordering (Phase 3 will consume) |
| `position` | `integer` default `0` | Stable ordering inside the group |

Indexes: unique on `(trunk_group_id, gateway_name)`, non-unique on `trunk_group_id`.

Additive column on existing table:

- `rustpbx_dids.trunk_group_name` — `string_null char_len=120`. Additive only; Phase 1 DID handlers do not touch it, so no regression. The TRK-04 engagement check scans this column. Field is populated by Phase 3's DID routing work.

### Validation sequence (TRK-03)

On `POST /api/v1/trunks` and `PUT /api/v1/trunks/{name}`, run these checks **in this order** and fail fast on the first failure:

1. `name` non-empty, trimmed, matches `^[a-zA-Z0-9_-]{1,64}$` → else 400.
2. `direction` in `{inbound, outbound, bidirectional}` → else 400.
3. `distribution_mode` in the allowed set. If mode is `parallel` and feature `parallel-trunk-dial` is **disabled**, reject with 400 `"parallel distribution requires the parallel-trunk-dial feature"`. The other 5 modes are always allowed.
4. `members` non-empty → else 400.
5. For each `members[i].gateway_name`: look up `rustpbx_sip_trunks` by name; if missing, collect into an error list. After the loop, if the list is non-empty → 400 with body `{error: "unknown gateway(s): [name1, name2]"}`.
6. No gateway name exists that collides with this trunk_group name → else 400 `"trunk group name '{n}' collides with existing gateway"`.
7. Wrap row insert + member rows in a single SeaORM transaction (`db.begin()` / `tx.commit()`). On any failure → rollback and return 500 (unless mapped above).

On `PUT`: the handler loads the existing row, validates the incoming body exactly like `POST`, then replaces members atomically inside the transaction (delete all existing member rows for this group, insert new rows). Direction/distribution_mode/credentials/acl/nofailover_sip_codes are simple column updates.

### Engagement check for delete (TRK-04)

Before deleting a trunk_group row, run **both** of these checks:

1. **DIDs scan:** `SELECT 1 FROM rustpbx_dids WHERE trunk_group_name = {name} LIMIT 1`. If any row exists → 409 `"trunk group '{n}' is referenced by DID '{did.number}' and cannot be deleted"`.
2. **Routing records best-effort scan:** `SELECT id, name, target_trunks FROM rustpbx_routes`, iterate, for each row parse the `target_trunks` JSON (it's a `Vec<String>` or a `{primary, backup}` shape — test both), and if any entry equals `{name}` → 409 `"trunk group '{n}' is referenced by route '{route.name}' and cannot be deleted"`.
3. If both checks pass, begin a transaction: delete member rows, delete the group row, commit.

**Phase 6 follow-up:** once routing tables have a first-class `trunk_group_id` reference column (RTE-01/04), the best-effort string scan in step 2 becomes an indexed equality check and the Phase 2 scan path is replaced. Track this as a TODO comment at the top of the engagement check fn body.

### Distribution mode dispatch (TRK-05)

The existing dispatch entry point is `src/proxy/routing/matcher.rs::select_trunk` at line 1004. It receives a `DestConfig` (single trunk name or list), a `select_method` string (`"rr"` / `"hash"` / `"weighted"` / `"random"`), a `hash_key` option, an `InviteOption`, routing state, and the `TrunkConfig` HashMap.

Phase 2's wiring:

1. **New fn** `resolve_trunk_group_to_dest_config(db, group_name) -> Result<(DestConfig, select_method, hash_key)>` in a new `src/proxy/routing/trunk_group_resolver.rs` module. It looks up the group + its members, reads `distribution_mode`, and returns a `DestConfig::Multiple(member_gateway_names)` plus a translated `select_method`:
   - `round_robin` → `"rr"`
   - `weight_based` → `"weighted"`
   - `hash_callid` → `"hash"` + `hash_key = Some("call-id")`
   - `hash_src_ip` → `"hash"` + `hash_key = Some("from.user")` (nearest existing key — note: matcher.rs treats `from.user` as the source identity; see `src/proxy/routing/matcher.rs:1034`)
   - `hash_destination` → `"hash"` + `hash_key = Some("to.user")` (see matcher.rs:1035)
   - `parallel` → feature-gated; see below
2. **Call site**: a new async helper `async fn select_gateway_for_trunk_group(db, group_name, option, routing_state, trunks_config) -> Result<String>` in the same resolver file that calls `resolve_trunk_group_to_dest_config` then calls existing `select_trunk(...)` from matcher.rs. This new helper is the exposed surface — routing rules that point at a trunk_group name invoke it instead of `select_trunk` directly.
3. **Integration with matcher.rs**: do NOT refactor the existing `select_trunk`. Add a **single branch** at the call sites (matcher.rs:319 and matcher.rs:427) that, before calling `select_trunk`, checks whether the `dest_config` names a trunk_group (by querying the DB through the new helper). If it does, delegate to the new helper. If it does not, call the existing `select_trunk` unchanged. This keeps the existing single-trunk path byte-for-byte identical.
4. **Hash determinism**: use `std::collections::hash_map::DefaultHasher` seeded per-call from the chosen key (call-id, src-ip, or destination). Test with at least 3 fixed inputs asserting consistent index selection across runs. `DefaultHasher` is NOT cryptographically stable across Rust versions but IS stable within a single process run, which is sufficient for Phase 2 tests. Note this limitation in a code comment.
5. **`parallel` feature flag**: add `parallel-trunk-dial = []` to `[features]` in `Cargo.toml`. Gate the `parallel` match arm with `#[cfg(feature = "parallel-trunk-dial")]` stub that returns `Err(anyhow!("parallel distribution not yet implemented"))` — the point of Phase 2 is the feature flag + reject path, not the implementation. Without the feature, a trunk_group with `distribution_mode == "parallel"` must be rejected at CREATE/UPDATE time (TRK-03 validation sequence step 3).

### View types (SHELL-04)

```rust
#[derive(Debug, Serialize)]
pub struct TrunkView {
    pub name: String,
    pub display_name: Option<String>,
    pub direction: String,
    pub distribution_mode: String,
    pub members: Vec<TrunkMemberView>,
    pub credentials: Option<serde_json::Value>,
    pub acl: Option<serde_json::Value>,
    pub nofailover_sip_codes: Option<Vec<i32>>,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
pub struct TrunkMemberView {
    pub gateway_name: String,
    pub weight: i32,
    pub priority: i32,
    pub position: i32,
}
```

`From<(trunk_group::Model, Vec<trunk_group_member::Model>)> for TrunkView` is the canonical construction path.

### Router wiring (SHELL-01)

Edit `src/handler/api_v1/mod.rs`:

```rust
pub mod trunks; // NEW

pub fn api_v1_router(state: AppState) -> Router {
    let protected: Router<AppState> = Router::new()
        .merge(gateways::router())
        .merge(dids::router())
        .merge(cdrs::router())
        .merge(diagnostics::router())
        .merge(system::router())
        .merge(trunks::router()); // NEW
    // ...
}
```

### Integration test convention (IT-01)

- New file `tests/api_v1_trunks.rs` following the same structure as `tests/api_v1_gateways.rs`.
- Minimum cases per route:
  1. 401 without Bearer token
  2. Happy path with valid token (seed gateways first, then POST a trunk_group referencing them, assert JSON shape)
  3. 404 on missing resource (GET/PUT/DELETE against an unknown name)
  4. 400 on bad input — test at minimum: missing members, unknown gateway reference, invalid name regex, invalid distribution_mode, `parallel` without feature
  5. 409 on delete-with-references — test at minimum: DID references blocks delete, routing record references blocks delete
- Fixtures seed via the existing `tests/common/` helpers (`test_state_with_api_key`).

### Claude's Discretion

- Exact wire format for `credentials` / `acl` JSON — prefer flat JSON objects with documented keys. Phase 3 promotes these to typed sub-resource schemas; Phase 2 does not need to lock the final shape as long as GET→PUT round-trips losslessly.
- Whether to enforce `direction` compatibility between a trunk_group and its member gateways (e.g. an outbound-only group cannot include an inbound-only gateway). **Recommend: skip this check in Phase 2.** Phase 5 is the enforcement phase. Direction on the group is metadata only in Phase 2.
- Whether to add an index on `rustpbx_trunk_group_members.gateway_name` — **recommend yes** (for the eventual Phase 3 sub-resource lookups); cheap and additive.
- Whether the member position is automatically assigned from array index on create/update — **recommend yes**; operators set order via array order, not explicit position integers.
</decisions>

<specifics>
## File Touch Map

**New files:**

- `src/models/trunk_group.rs` (entity + Migration)
- `src/models/trunk_group_member.rs` (entity + Migration)
- `src/models/add_did_trunk_group_name_column.rs` (additive migration adding `trunk_group_name` to `rustpbx_dids`)
- `src/handler/api_v1/trunks.rs` (sub-router: list/get/create/update/delete + helpers)
- `src/proxy/routing/trunk_group_resolver.rs` (new module: trunk_group → `DestConfig` + dispatch wiring helper)
- `tests/api_v1_trunks.rs` (integration tests — IT-01 compliance)
- `tests/trunk_group_dispatch.rs` (unit tests for the distribution mode resolver + hash determinism)

**Modified files:**

- `src/models/migration.rs` — append 3 new `Box::new(super::<name>::Migration)` entries (one per new migration)
- `src/models/mod.rs` — declare 3 new sub-modules (`pub mod trunk_group;`, `pub mod trunk_group_member;`, `pub mod add_did_trunk_group_name_column;`)
- `src/models/did.rs` — add `Column::TrunkGroupName` enum variant + struct field (nullable `Option<String>`); Phase 1 handlers do not set it so no API shape change
- `src/handler/api_v1/mod.rs` — `pub mod trunks;` + `.merge(trunks::router())`
- `src/proxy/routing/mod.rs` — declare `pub mod trunk_group_resolver;`
- `src/proxy/routing/matcher.rs` — at lines 319 and 427 (both `select_trunk` call sites), add a database-driven trunk_group delegation branch **before** calling `select_trunk`. The existing branch is unchanged for legacy gateway names.
- `Cargo.toml` — add `parallel-trunk-dial = []` to `[features]`

**Not touched:**

- `src/console/handlers/sip_trunk.rs` (legacy console owns `sip_trunk` only; untouched)
- `src/models/sip_trunk.rs` (zero schema change — TRK-01 hard constraint)
- `src/handler/api_v1/gateways.rs` (Phase 1 shipped; reserving name collision on the gateway side is deferred to a future gap-closure plan)

## Cargo.toml diff preview

```toml
[features]
# ... existing features ...
parallel-trunk-dial = []
```

## Test fixtures

The new tests use `tests/common/test_state_with_api_key` (already exists, used by `api_v1_gateways.rs`). Phase 2 tests seed 2–3 `sip_trunk` rows at setup (directly via `sip_trunk::ActiveModel::insert`, following `tests/api_v1_gateways.rs::insert_trunk`), then POST a trunk_group referencing those gateway names.

## Verification commands

```bash
cargo build -p rustpbx --all-targets
cargo test -p rustpbx --test api_v1_trunks
cargo test -p rustpbx --test trunk_group_dispatch
cargo test -p rustpbx                  # full suite must stay green (Phase 1: 78/78)
cargo clippy -p rustpbx --all-targets -- -D warnings
# Feature-gated parallel path:
cargo check -p rustpbx --features parallel-trunk-dial
```
</specifics>

<deferred>
## Deferred Ideas (out of Phase 2 — tracked for future phases)

- Per-trunk-group credentials CRUD endpoints → Phase 3 (TSUB-01)
- Per-trunk-group origination URIs CRUD → Phase 3 (TSUB-02)
- Per-trunk-group media config (codec list, dtmf mode, srtp, media mode) GET/PUT → Phase 3 (TSUB-03)
- Per-trunk-group capacity (max_calls, max_cps) enforcement → Phase 5 (TSUB-04)
- Per-trunk-group ACL CRUD + enforcement → Phase 5 (TSUB-05)
- Codec filter (488 on mismatch) → Phase 5 (TSUB-06)
- Routing table CRUD + record sub-route → Phase 6 (RTE-01..05)
- Trunk-group-aware routing targets in the routing record schema (first-class `trunk_group_id` column on `rustpbx_routes`) → Phase 6
- `parallel` distribution mode actual implementation → future feature release (v2.1+); Phase 2 only ships the feature flag and the reject path
- Name collision enforcement on the GATEWAY side (reject gateway create if name matches an existing trunk_group) → follow-up gap-closure plan against Phase 1
- Direction compatibility check between group and member gateways → Phase 5 (enforcement phase)

## Validation Architecture

**Unit-level (required):**

- `tests/trunk_group_dispatch.rs`: assert `round_robin` / `weight_based` / `hash_callid` / `hash_src_ip` / `hash_destination` all translate to the correct `(select_method, hash_key)` pairs, assert hash determinism across 3 fixed inputs, assert `parallel` without feature returns an error.

**Integration-level (required — IT-01):**

- `tests/api_v1_trunks.rs`: full 401/happy/404/400/409 matrix across all 5 routes.

**Regression:**

- Full existing `cargo test` suite must stay at 78/78 (Phase 1 baseline). Phase 2 adds new tests on top.

**Manual:**

- None required — Phase 2 does not touch console HTML templates (MIG-03 does not apply).
</deferred>

---

*Phase: 02-trunk-groups-schema-core-crud*
*Context gathered: 2026-04-16*
*Source: upstream planner invocation + Phase 1 adapter convention (01-CONTEXT.md)*
