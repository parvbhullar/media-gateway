# Phase 3: Trunk Sub-Resources L1 & Routing Resolve — Context

**Gathered:** 2026-04-17
**Status:** Ready for planning
**Source:** Discussion + Phase 2 hand-off + CARRIER-API spec

<domain>
## Phase Boundary

Phase 3 promotes Phase 2's free-form `trunk_group.credentials` JSON column into a typed multi-row sub-resource, ships two net-new sub-resources (`origination_uris`, `media_config`), and ships the `/api/v1/routing/resolve` dry-run endpoint by reusing the existing `match_invite_with_trace` dispatch path.

**Routes shipped (4 sub-resource routes + 1 dry-run = 6 endpoints):**

| Route | Purpose | Source module |
|---|---|---|
| `GET /api/v1/trunks/{name}/credentials` | List credentials | NEW `src/handler/api_v1/trunk_credentials.rs` |
| `POST /api/v1/trunks/{name}/credentials` | Add credential | same |
| `DELETE /api/v1/trunks/{name}/credentials/{realm}` | Remove credential by realm | same |
| `GET /api/v1/trunks/{name}/origination_uris` | List origination URIs | NEW `src/handler/api_v1/trunk_origination_uris.rs` |
| `POST /api/v1/trunks/{name}/origination_uris` | Add URI | same |
| `DELETE /api/v1/trunks/{name}/origination_uris/{uri}` | Remove URI | same |
| `GET /api/v1/trunks/{name}/media` | Get media config | NEW `src/handler/api_v1/trunk_media.rs` |
| `PUT /api/v1/trunks/{name}/media` | Set media config (replace) | same |
| `POST /api/v1/routing/resolve` | Dry-run route resolution | NEW `src/handler/api_v1/routing.rs` |

**Schema changes:**

- NEW table `supersip_trunk_credentials` (id, trunk_group_id FK CASCADE, realm, auth_username, auth_password, created_at) — UNIQUE (trunk_group_id, realm)
- NEW table `supersip_trunk_origination_uris` (id, trunk_group_id FK CASCADE, uri, position, created_at) — UNIQUE (trunk_group_id, uri)
- NEW column `rustpbx_trunk_groups.media_config` — `Option<Json>`, additive, replaces nothing
- DROP column `rustpbx_trunk_groups.credentials` — Phase 2 left this as `Option<Json>`; promoted to the new table. **Destructive but safe** because no production deployment exists yet (sip_fix branch unmerged).

**Out of scope for Phase 3** — explicitly deferred:

- Per-trunk capacity (TSUB-04) — Phase 5 (proxy hot-path enforcement)
- Per-trunk ACL CRUD + enforcement (TSUB-05) — Phase 5
- Codec filter (488 on mismatch) (TSUB-06) — Phase 5; Phase 3 only stores the codec list
- Capacity active-count observability (TSUB-07) — Phase 5
- Routing tables CRUD (RTE-01, RTE-02, RTE-04, RTE-05) — Phase 6
</domain>

<decisions>
## Implementation Decisions (LOCKED)

### Naming Convention (PROJECT-WIDE — applies to all phases from here forward)

- **D-00:** All NEW tables introduced from Phase 3 onward use the `supersip_` prefix instead of `rustpbx_`. This reflects the project rebrand to SuperSip.
- **Existing `rustpbx_*` tables stay untouched** in Phase 3 — renaming them is destructive and would break Phase 1 + Phase 2 migrations already shipped on `sip_fix`. Cross-table FKs continue to reference `rustpbx_trunk_groups.id` etc.
- **New COLUMNS on existing tables keep the existing prefix** (e.g., `rustpbx_trunk_groups.media_config` stays — adding a column doesn't trigger a rename).
- A future dedicated migration phase (post v2.0, tracked as v2.1 candidate) handles the bulk `rustpbx_* → supersip_*` rename via paired CREATE-RENAME-DROP migrations with downtime planning.

### Credentials storage (TSUB-01)

- **D-01:** New table `supersip_trunk_credentials` keyed on `(trunk_group_id, realm)` UNIQUE. Mirrors the `trunk_group_member` pattern from Phase 2. Wire format: `[{realm, username, password}, …]` returned by GET; POST takes `{realm, username, password}`.
- **D-02:** Drop `trunk_group.credentials` JSON column in the same migration. POST/PUT `/trunks` no longer accepts a `credentials` field. Phase 2's `create_trunk_persists_credentials_acl_nofailover` test must be split — credentials assertion moves to a new `tests/api_v1_trunk_credentials.rs` happy-path test, ACL assertion stays.
- **D-03:** Password stored as plaintext (consistent with how Phase 1 `gateways.rs` stores SIP auth passwords). No encryption layer in Phase 3 — that's a v2.1 hardening concern.
- **D-04:** DELETE-by-realm is strict: 404 if realm not found. (Phase 2 trunks DELETE is also 404-on-miss, so this stays consistent.)
- **D-05:** Realm is the path-segment identifier and must be URL-encoded by clients. Validation: 1-255 chars, no slashes (router conflicts).

### Origination URIs storage (TSUB-02)

- **D-06:** New table `supersip_trunk_origination_uris` (id, trunk_group_id FK CASCADE, uri, position auto-assigned from row order, created_at). UNIQUE (trunk_group_id, uri).
- **D-07:** Wire format: GET returns `[{uri, position}, …]` ordered by position. POST takes `{uri}` and assigns next position. DELETE-by-uri is strict 404-on-miss; URL-encoded.
- **D-08:** URI validation: must parse as a valid `rsip::Uri` via existing rsipstack helpers. 400 with `{error: "invalid SIP URI: …"}` on parse failure.

### Media config storage (TSUB-03)

- **D-09:** Add `media_config: Option<Json>` to `trunk_group`. JSON shape: `{codecs: ["pcmu", "pcma"], dtmf_mode: "rfc2833", srtp: null, media_mode: null}`. Same column gets read on GET and replaced atomically on PUT.
- **D-10:** Codec list canonical form: lowercase strings per CARRIER-API example. Phase 5 enforcement will need a translation layer to/from rsipstack's uppercase RFC 3551 form — note this as a Phase 5 follow-up.
- **D-11:** GET returns `{codecs: [], dtmf_mode: null, srtp: null, media_mode: null}` when `media_config` is NULL (vs 404). PUT with all-null fields stores `Some(Json{nulls})`, not NULL — keeps the schema observable.
- **D-12:** Validation in Phase 3: codec strings non-empty if present, dtmf_mode in `{rfc2833, info, inband}`, srtp in `{srtp, srtp_optional, null}`, media_mode in `{relay, transcode, null}`. Unknown values → 400. Phase 5 adds enforcement; Phase 3 just validates the field set.

### /routing/resolve dry-run (RTE-03)

- **D-13:** Reuse existing `src/proxy/routing/matcher.rs::match_invite_with_trace`. Build an `InviteOption` from request body, pass a fresh `RouteTrace`, project the result + trace into a JSON response. **Same code path as production** — by construction, dry-run cannot drift from real dispatch.
- **D-14:** Request body matches production InviteOption fields:
  ```json
  {
    "caller_number": "+14155551234",
    "destination_number": "+442079460123",
    "src_ip": "10.0.0.5",
    "headers": {"From": "...", "To": "...", "Call-ID": "..."}
  }
  ```
  `src_ip` and `headers` are optional; missing values use placeholder defaults. Documented as such in OpenAPI later.
- **D-15:** Response shape:
  ```json
  {
    "result": "matched" | "not_handled" | "abort" | "rejected",
    "matched_table": "outbound-us",
    "matched_record_index": 2,
    "match_reason": "Lpm: 6505 matched 6505551234",
    "target": {"kind": "trunk_group" | "gateway", "name": "us-carrier"},
    "selected_gateway": "twilio-us",  // for trunk_group dispatch
    "trace": [...]  // RouteTrace events serialized
  }
  ```
- **D-16:** Endpoint requires Bearer auth like all other `/api/v1/*` routes. Mounted under the existing `protected` router in `src/handler/api_v1/mod.rs`.
- **D-17:** Reuses `RoutingState::new_with_db(db.clone())` from Phase 2 — no new dispatch state plumbing.

### Wire types (SHELL-04)

```rust
// trunk_credentials.rs
#[derive(Serialize)]
pub struct TrunkCredentialView { pub realm: String, pub username: String, pub password: String }

#[derive(Deserialize)]
pub struct AddTrunkCredentialRequest { pub realm: String, pub username: String, pub password: String }

// trunk_origination_uris.rs
#[derive(Serialize)]
pub struct TrunkOriginationUriView { pub uri: String, pub position: i32 }

#[derive(Deserialize)]
pub struct AddTrunkOriginationUriRequest { pub uri: String }

// trunk_media.rs
#[derive(Serialize, Deserialize)]
pub struct TrunkMediaConfig {
    pub codecs: Vec<String>,
    pub dtmf_mode: Option<String>,
    pub srtp: Option<String>,
    pub media_mode: Option<String>,
}

// routing.rs
#[derive(Deserialize)]
pub struct ResolveRouteRequest {
    pub caller_number: String,
    pub destination_number: String,
    pub src_ip: Option<String>,
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Serialize)]
pub struct ResolveRouteResponse {
    pub result: String,
    pub matched_table: Option<String>,
    pub matched_record_index: Option<i32>,
    pub match_reason: Option<String>,
    pub target: Option<ResolveTarget>,
    pub selected_gateway: Option<String>,
    pub trace: Vec<serde_json::Value>,
}
```

### Router wiring (SHELL-01)

`src/handler/api_v1/mod.rs`:

```rust
pub mod trunk_credentials;       // NEW
pub mod trunk_origination_uris;  // NEW
pub mod trunk_media;             // NEW
pub mod routing;                 // NEW

let protected: Router<AppState> = Router::new()
    .merge(gateways::router())
    .merge(dids::router())
    .merge(cdrs::router())
    .merge(diagnostics::router())
    .merge(system::router())
    .merge(trunks::router())
    .merge(trunk_credentials::router())       // NEW
    .merge(trunk_origination_uris::router())  // NEW
    .merge(trunk_media::router())             // NEW
    .merge(routing::router());                // NEW
```

### Integration test convention (IT-01)

Per CARRIER-API spec + IT-01 contract, each new sub-router gets its own test file:

- `tests/api_v1_trunk_credentials.rs` — 401, list happy, POST happy, POST duplicate-realm 409, DELETE happy, DELETE-missing 404, parent-trunk-missing 404
- `tests/api_v1_trunk_origination_uris.rs` — 401, list, POST happy, POST invalid-URI 400, DELETE happy, DELETE-missing 404, parent-missing 404
- `tests/api_v1_trunk_media.rs` — 401, GET-empty-defaults, PUT happy + GET round-trip, PUT invalid-codec/dtmf/srtp/media_mode 400, parent-missing 404
- `tests/api_v1_routing_resolve.rs` — 401, happy LPM match, no-match returns `result: "not_handled"`, with-trunk-group target returns selected_gateway, abort case (e.g. blocked source IP), invalid request body 400

### Migration registration order

`src/models/migration.rs::Migrator::migrations` appends in this order (FK-dependent):

```rust
Box::new(super::trunk_credentials::Migration),         // creates table
Box::new(super::trunk_origination_uris::Migration),    // creates table
Box::new(super::add_media_config_column::Migration),   // adds column to existing trunk_group
Box::new(super::drop_credentials_column::Migration),   // drops Phase 2 JSON column
```

Drop migration runs LAST so any in-flight reads succeed during deploy. The drop is destructive — but Phase 2 is unmerged (still on `sip_fix` branch), so no production rows have credentials yet.

### Claude's Discretion

- Exact JSON serialization of `RouteTrace` events for the resolve response — pick whatever round-trips cleanly via `serde_json::to_value(&trace)`. If trace events don't already implement Serialize, add the derive.
- N+1 vs batch on the credentials/origination_uris list endpoints — Phase 5 will revisit if perf matters. Phase 3 may use simple per-row queries.
- Whether to add `created_at` to credentials/origination_uris views — recommend yes for consistency with TrunkView, but not strictly required.
- Whether `src_ip` defaults to `"0.0.0.0"` or `"127.0.0.1"` when omitted — pick what `match_invite_with_trace` tolerates; document in the handler.
</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### CARRIER-API spec (source of truth for route shapes)
- `../docs/CARRIER-API.md` — Trunk sub-resource routes (lines covering `/credentials`, `/origination_uris`, `/media`) + routing/resolve route + media config example with `codecs/dtmf_mode/srtp/media_mode` shape
- `../docs/CARRIER-ARCHITECTURE.md` — reference only

### Prior phase context
- `.planning/phases/01-api-shell-cheap-wrappers/01-CONTEXT.md` — SHELL-01..05 conventions (sub-router mount, view types, ApiError, paginated envelope)
- `.planning/phases/02-trunk-groups-schema-core-crud/02-CONTEXT.md` — trunk_group entity, RoutingState DB threading decision, SeaORM migration patterns, IT-01 test fixture convention
- `.planning/phases/02-trunk-groups-schema-core-crud/02-01-SUMMARY.md` — entity hand-off notes (incl. `..Default::default()` ActiveModel pattern)
- `.planning/phases/02-trunk-groups-schema-core-crud/02-03-SUMMARY.md` — `RoutingState::new_with_db` ctor + matcher_level integration test pattern (template for resolve dry-run test)
- `.planning/phases/01-api-shell-cheap-wrappers/deferred-items.md` — SHELL-05 ADR (model-layer sharing)

### Phase 1 implementation patterns (read for code-style template)
- `src/handler/api_v1/gateways.rs` — sub-router structure, validate_name, engagement-tracked delete pattern
- `src/handler/api_v1/dids.rs` — paginated list with filters
- `src/handler/api_v1/error.rs` — ApiError variants
- `src/handler/api_v1/common.rs` — Pagination extractor + PaginatedResponse envelope

### Phase 2 implementation (template for sub-router wiring)
- `src/handler/api_v1/trunks.rs` — full CRUD pattern incl. transactions (`db.begin()`)
- `src/handler/api_v1/mod.rs` — router merge convention
- `src/models/trunk_group.rs` + `src/models/trunk_group_member.rs` — entity templates
- `src/models/migration.rs` — Migrator append-only convention

### Routing dispatch (RTE-03 reuse target)
- `src/proxy/routing/matcher.rs:120` — `match_invite_with_trace` signature (this is what /resolve calls)
- `src/proxy/routing/matcher.rs:22-90` — `RouteTrace` struct (response source)
- `src/proxy/routing/trunk_group_resolver.rs` — Phase 2 resolver (dispatch returns this for trunk_group targets)
- `src/call/mod.rs:989-1280` — `RoutingState::new_with_db` ctor + db accessor
- `tests/trunk_group_dispatch.rs::matcher_level_trunk_group_dispatch` — integration test pattern showing how to seed a real DB and exercise match_invite

### Test fixtures
- `tests/common/mod.rs` — `test_state_with_api_key` and seeding helpers
- `tests/api_v1_trunks.rs` — IT-01 reference (auth/happy/404/400/409 matrix)
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `Pagination` extractor + `PaginatedResponse<T>` envelope in `src/handler/api_v1/common.rs` — wrap list endpoints
- `ApiError::bad_request | not_found | conflict | internal` in `src/handler/api_v1/error.rs` — all 4 sub-routers need these
- `..Default::default()` on SeaORM ActiveModel — works (verified Phase 2)
- `RoutingState::new_with_db(server.database.clone())` already plumbed at `src/proxy/call.rs:385` — /resolve handler uses the same pattern
- `match_invite_with_trace` already used by `tests/trunk_group_dispatch.rs::matcher_level_trunk_group_dispatch` — copy that test's fixture setup

### Established Patterns
- One sub-router file per resource group; merged into `protected` router in `mod.rs`
- View types named `{Entity}View`, never serialize SeaORM Model directly (SHELL-04)
- DELETE returns 204 on success, 404-on-miss is strict (Phase 2)
- Validation helpers live in the same file as the handlers (e.g. `validate_trunk_group_name` in trunks.rs)
- Transactional writes wrap parent-and-children operations in `db.begin()` / `tx.commit()`
- Integration tests use direct ActiveModel inserts for fixture seeding (faster than going through the API)
- `add_*_column.rs` and `drop_*_column.rs` migrations follow the additive pattern from Phase 2's `add_did_trunk_group_name_column.rs`

### Integration Points
- `src/handler/api_v1/mod.rs::api_v1_router` — 4 new `.merge()` calls
- `src/models/migration.rs::Migrator::migrations` — 4 new `Box::new(...)` entries appended
- `src/models/mod.rs` — 4 new `pub mod` declarations
- `src/models/trunk_group.rs` — modify Model struct (add `media_config`, drop `credentials`); migration file gets a `drop_column` migration co-located via the new drop_credentials_column module
</code_context>

<specifics>
## Specific Ideas

- **CARRIER-API example for media** uses lowercase codec strings (`"pcmu"`, `"pcma"`) — adopt as canonical wire format. rsipstack uses uppercase internally; Phase 5 enforcement adds the translation layer.
- **CARRIER-API example for create_trunk** shows credentials as a list `[{realm, username, password}]` — confirms multi-row decision; the create_trunk handler itself drops this in Phase 3 (sub-resource only).
- The Phase 2 `matcher_level_trunk_group_dispatch` test in `tests/trunk_group_dispatch.rs` is the closest analog for the /resolve integration test — start from that fixture pattern.
- `AppStateInner.console: Option<Arc<ConsoleState>>` exists from Phase 1 but **not used** by these new handlers — sub-resources are model-layer only (no console UI exists for trunk_groups, so no console refactor needed).
</specifics>

<deferred>
## Deferred Ideas (out of Phase 3 — tracked for future phases)

- **Capacity sub-resource** (`/api/v1/trunks/{name}/capacity` GET/PUT) → Phase 5 (TSUB-04, TSUB-07) — needs proxy hot-path enforcement before shipping the route
- **ACL sub-resource** (`/api/v1/trunks/{name}/acl` CRUD) → Phase 5 (TSUB-05)
- **Codec filter enforcement (488 on no-overlap)** → Phase 5 (TSUB-06) — Phase 3 stores the list; Phase 5 enforces it
- **Routing tables CRUD** (`/api/v1/routing/tables` + records sub-route) → Phase 6 (RTE-01, RTE-02, RTE-04, RTE-05)
- **Credentials encryption at rest** → v2.1 hardening milestone (currently plaintext like gateways.rs)
- **Codec name canonical translation layer (lowercase wire ↔ uppercase rsipstack)** → Phase 5 (lands when enforcement does)
- **N+1 batch optimization for credentials/origination_uris list endpoints** → Phase 5 if perf matters
- **OpenAPI documentation of /resolve request/response shapes** → v2.1 (OpenAPI publication is deferred milestone-wide)
- **Bulk `rustpbx_* → supersip_*` table rename** → v2.1 candidate (separate destructive migration phase with downtime planning; Phase 3+ only uses the new prefix on net-new tables)
</deferred>

## Validation Architecture

**Unit-level (none required for Phase 3):** all logic is handler-level. Validation helpers can be tested through integration tests.

**Integration-level (required — IT-01):**

- `tests/api_v1_trunk_credentials.rs` — auth/list/POST/duplicate/DELETE/missing matrix
- `tests/api_v1_trunk_origination_uris.rs` — auth/list/POST/invalid-URI/DELETE/missing matrix
- `tests/api_v1_trunk_media.rs` — auth/GET-defaults/PUT-roundtrip/invalid-fields/missing-parent matrix
- `tests/api_v1_routing_resolve.rs` — auth/match/no-match/trunk-group-target/abort/invalid-body matrix

**Regression:** Full Phase 1 + Phase 2 baseline (114 tests) must stay green. Phase 3 adds new tests on top. Specifically:
- Phase 2's `create_trunk_persists_credentials_acl_nofailover` test must be **split** into ACL-only (stays in api_v1_trunks.rs) + credentials-via-sub-resource (moves to api_v1_trunk_credentials.rs).
- Any Phase 2 test that POSTs `credentials: {…}` against /trunks must drop that field.

**Manual:** None. No console templates touched (MIG-03 N/A).

---

*Phase: 03-trunk-sub-resources-l1-routing-resolve*
*Context gathered: 2026-04-17*
*Source: discuss-phase + Phase 2 hand-off + CARRIER-API spec*
