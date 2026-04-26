# Phase 6: Routing Tables, Records & Distribution — Context

**Gathered:** 2026-04-25
**Status:** Ready for planning
**Source:** Discussion (9 areas batched into 4 groups; all-recommended)

<domain>
## Phase Boundary

Phase 6 ships a NEW routing table CRUD surface with embedded-document record storage (mongo-style records-as-JSON-array within a table row). Five match types are introduced (`Lpm`, `ExactMatch`, `Regex`, `Compare`, `HttpQuery`) and integrated into the existing `match_invite_with_trace` matcher. The legacy `rustpbx_routes` table stays in the schema as data-only (no longer consulted by matching) and is no longer writable via `/api/v1`. The Phase 3 `/resolve` dry-run becomes "complete" once `matched_table` and `matched_record_index` are populated from the new tables.

**Routes shipped (8 endpoints):**

| Route | Purpose | Source module |
|---|---|---|
| `GET /api/v1/routing/tables` | List routing tables | NEW `src/handler/api_v1/routing_tables.rs` |
| `POST /api/v1/routing/tables` | Create table (metadata only) | same |
| `GET /api/v1/routing/tables/{name}` | Get one table | same |
| `PUT /api/v1/routing/tables/{name}` | Replace table metadata (NOT records) | same |
| `DELETE /api/v1/routing/tables/{name}` | Delete table (and all its records) | same |
| `GET /api/v1/routing/tables/{name}/records` | List records (ordered by position) | NEW `src/handler/api_v1/routing_records.rs` (or same file) |
| `POST /api/v1/routing/tables/{name}/records` | Append record (server generates record_id) | same |
| `GET /api/v1/routing/tables/{name}/records/{record_id}` | Get one record | same |
| `PUT /api/v1/routing/tables/{name}/records/{record_id}` | Replace record (preserves position) | same |
| `DELETE /api/v1/routing/tables/{name}/records/{record_id}` | Remove record (no shift; stable IDs) | same |

**Schema changes:**

- NEW table `supersip_routing_tables` (id, name UNIQUE, description, direction, priority, is_active, records: Json, created_at, updated_at) — `records` is a JSON array of record objects
- NO new sub-table for records — embedded JSON column per Q1 decision (matches "console stores embedded documents" requirement)
- Legacy `rustpbx_routes` STAYS UNTOUCHED in Phase 6 (no migration, no drop). Operator may keep using it via console or directly, but no /api/v1 surface and not consulted by matcher.

**Matcher integration:**

- EXTEND `match_invite_with_trace` to consult `supersip_routing_tables` BEFORE legacy logic (or instead of)
- NEW `src/proxy/routing/table_matcher.rs` — evaluates the 5 match types against caller/destination/headers
- NEW `src/proxy/routing/match_types.rs` — `MatchType` enum + per-variant evaluation helpers
- HttpQuery client uses existing `reqwest`/`hyper` dependency (verify in research); 2s timeout
- Chain depth (next_table) capped at 3 to prevent loops

**Out of scope** — explicitly deferred:

- Migration of `rustpbx_routes` data into `supersip_routing_tables` — operator does this manually
- Routing table versioning / rollback — v2.1
- Pre-fetched HttpQuery cache — v2.1
- Per-record metrics (hit count, last-matched-at) — v2.1
- Sub-account isolation on routing tables — Phase 13
- Bulk import/export of routing tables — operator can use repeated POSTs in Phase 6
</domain>

<decisions>
## Implementation Decisions (LOCKED)

### Storage shape (RTE-01, RTE-02)

- **D-01:** Single NEW table `supersip_routing_tables` with `records: Json` embedded array. Mongo-style. Matches the "console stores them as embedded documents" requirement (RTE-02). One row per table.
- **D-02:** Records carry stable server-generated `record_id` (UUID v4) — concurrent-edit-safe. POST returns the generated ID. URL path `/records/{record_id}` is stable across DELETE/POST sequences (no array-index shift confusion).
- **D-03:** Records JSON shape:
  ```json
  {
    "record_id": "uuid",
    "position": 0,
    "match": {"type": "lpm", "prefix": "+1415"},
    "target": {"kind": "trunk_group", "name": "us-carrier"},
    "is_default": false,
    "is_active": true
  }
  ```
  `position` is server-managed (not user-settable on PUT; settable on POST via optional `position` to insert-at-index, default = append).
- **D-04:** PUT-table-metadata excludes records. PUT-record replaces single record by ID. POST-records appends or inserts. DELETE-record removes by ID with no index shift (positions of remaining records preserved). This prevents accidental records wipe.

### Coexistence with legacy `rustpbx_routes`

- **D-05:** Legacy `rustpbx_routes` stays in schema (Phase 1 deliverable, has CDR/audit dependencies). Phase 6 does NOT migrate data, does NOT drop the table, does NOT expose it via `/api/v1` (no /api/v1/routes endpoints).
- **D-06:** `match_invite_with_trace` is REWIRED to consult `supersip_routing_tables` ONLY. Legacy `rustpbx_routes` rows are ignored by matching. This is a behavior change — document in 06-SUMMARY.md as "operator must re-create routing rules in /api/v1/routing/tables".
- **D-07:** No data migration in Phase 6. Operator re-creates rules via the new API. Phase 6's manual deploy guide explicitly calls this out.

### Match types (RTE-04)

All 5 match types are tagged-enum variants in the record's `match` field. JSON shape is locked.

- **D-08 (Lpm):** Longest-prefix-match against `destination_number`. Pattern field: `{prefix: String}`. Multiple Lpm records: longest matching prefix wins. Within a single table, all Lpm records are evaluated together; the longest match is the chosen one.
- **D-09 (ExactMatch):** Full string equality against `destination_number`. Pattern field: `{value: String}`. Case-sensitive.
- **D-10 (Regex):** Rust `regex` crate match against `destination_number`. Pattern field: `{pattern: String}`. Pattern is pre-compiled at table-load time and cached in memory; PUT/POST validates by compiling once.
- **D-11 (Compare):** Numeric comparison against `destination_number.len()` (digit count). Pattern fields: `{op: "eq"|"lt"|"gt"|"in", value: u32 | [u32, u32]}`. Use case: "match all 11-digit numbers" → `{op: "eq", value: 11}`. "match 7-15 digits" → `{op: "in", value: [7, 15]}`.
- **D-12 (HttpQuery):** External HTTP lookup at match time. Pattern fields: `{url: String, timeout_ms: Option<u32>, headers: Option<Map<String,String>>}`. Default timeout 2000ms; max 5000ms.

### HttpQuery wire shape (D-12 details)

- **D-13:** HTTP request: `POST {url}` with body `{caller_number, destination_number, src_ip, headers}` (JSON). Same body shape as `/api/v1/routing/resolve` request. Operator may include extra headers via record's `headers` field (e.g., bearer token).
- **D-14:** HTTP response shape: `{"matched": bool, "target": {"kind": "trunk_group" | "gateway", "name": "..."}}`. Only `matched: true` is consumed; `false` falls through to next record. Other JSON shapes → log warning, fall through.
- **D-15:** Failure mode: timeout, 5xx, connection error, malformed JSON → fall through to next record (treat as no-match). Log warning. **Not 503** — HttpQuery failure must not fail the entire dispatch (per Q8).
- **D-16:** Per-record `timeout_ms` defaults to 2000, max 5000. Higher values rejected with 400 at PUT/POST.
- **D-17:** HttpQuery records are evaluated SERIALLY in record `position` order — one HTTP call at a time. Parallel HttpQuery is a v2.1 optimization (note: this means HttpQuery records add latency proportional to count + position; recommend operators put cheap matches before HttpQuery).

### Default record (RTE-05)

- **D-18:** `is_default: bool` field on each record (default false). At most ONE record per table can have `is_default: true`. Validated at PUT/POST: violation → 400.
- **D-19:** Resolution order: scan records in `position` order; first matching record wins. If no record matches, scan for `is_default: true` and return its target. If no default and no match → `RouteResult::Abort` (existing behavior, no change).
- **D-20:** Default record's `match` field is ignored at evaluation time (it's the fallback). Convention: store `{type: "lpm", prefix: ""}` or similar; matcher skips evaluation when `is_default: true`.

### Match priority across tables (RTE-04)

- **D-21:** Direction (`inbound`/`outbound`/`both`) acts as hard filter. Inbound INVITE only consults inbound + both tables; outbound only consults outbound + both. Direction stored at table level (column `direction`), not per-record.
- **D-22:** Within direction, tables sorted by `priority: i32` ASC (lower = first). First-match-wins across tables. Default `priority: 100`. Re-uses existing `rustpbx_routes.priority` semantic for operator familiarity.
- **D-23:** Within a single table, records evaluated in `position` order. First-match-wins within table. (Lpm is the exception: all Lpm records in the same table compete for longest prefix; non-Lpm records evaluated in position order separately. Recommend operators use a single match type per table for clarity, but mixing is allowed.)

### Routing record target type (Q7)

- **D-24:** Tagged enum in `target` field with 4 variants:
  - `{kind: "trunk_group", name: "us-carrier"}` — dispatch to trunk_group (most common)
  - `{kind: "gateway", name: "twilio-us"}` — direct gateway dispatch
  - `{kind: "next_table", name: "us-overflow"}` — chain to another table
  - `{kind: "reject", code: u16, reason: String}` — explicit reject (e.g., `{code:404, reason:"not_handled"}`). Maps to `RouteResult::Reject` from Phase 5.
- **D-25:** `next_table` chain depth capped at 3. Loop detection: track visited table names in matcher state; on revisit → `RouteResult::Abort` with reason `routing_loop_detected`.
- **D-26:** Target name validation at PUT/POST: warn (not error) if target name doesn't exist at write time. Targets are resolved at match time, so write-time validation is best-effort.

### CRUD endpoints (RTE-01, RTE-02)

- **D-27:** Table-level endpoints (`GET/POST/PUT/DELETE /api/v1/routing/tables[/{name}]`):
  - POST body: `{name, description?, direction?, priority?, is_active?, records?: []}` — records optional on create (default empty array)
  - PUT body: `{description?, direction?, priority?, is_active?}` — does NOT accept records (records-only-via-records-endpoints per D-04)
  - DELETE: cascade deletes the row (no FK; records are JSON column → goes away with the row)
- **D-28:** Record-level endpoints (`GET/POST/PUT/DELETE /api/v1/routing/tables/{name}/records[/{record_id}]`):
  - POST body: `{match, target, is_default?, position?}` — server generates `record_id`. `position` defaults to append (= current record count). If specified, inserts at that index, shifting others.
  - PUT body: `{match, target, is_default?}` — full replace. Preserves `record_id` and `position`.
  - DELETE: removes record by ID. Other records' `position` values are NOT renumbered (stays sparse if needed; UI/list endpoint resorts by position).
  - GET list returns records ordered by `position` ASC.

### Resolve integration (Phase 3 RTE-03 + Phase 6 completion)

- **D-29:** `match_invite_with_trace` reads `supersip_routing_tables` rows on each INVITE (no caching in Phase 6 — fresh DB read per invite, mirrors Phase 5 D-17 ACL pattern). Caching is v2.1 optimization.
- **D-30:** Resolve dry-run response (`/resolve` from Phase 3) populates `matched_table: Option<String>` (table name) and `matched_record_index: Option<i32>` (the matched record's `position` field). Phase 3 left these as `None`; Phase 6 wires them.
- **D-31:** RouteTrace events extended to include match-type-specific events: `LpmMatch{prefix}`, `ExactMatch{value}`, `RegexMatch{pattern}`, `CompareMatch{op, value}`, `HttpQueryMatch{url, latency_ms}`, `HttpQueryFailed{url, error}`, `DefaultRecordUsed{table}`, `NoMatch{table}`. Trace stays JSON-serializable for the resolve response.

### Wire types (skeleton — planner finalizes)

```rust
// routing_tables.rs
#[derive(Serialize, Deserialize)]
pub struct RoutingTableView {
    pub name: String,
    pub description: Option<String>,
    pub direction: String,  // "inbound" | "outbound" | "both"
    pub priority: i32,
    pub is_active: bool,
    pub record_count: u32,  // computed; not in body for PUT
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct CreateRoutingTableRequest {
    pub name: String,
    pub description: Option<String>,
    pub direction: Option<String>,  // default "both"
    pub priority: Option<i32>,       // default 100
    pub is_active: Option<bool>,     // default true
    pub records: Option<Vec<RoutingRecord>>,  // default empty
}

// routing_records.rs
#[derive(Serialize, Deserialize, Clone)]
pub struct RoutingRecord {
    pub record_id: String,           // UUID v4 (server-generated on POST)
    pub position: i32,
    pub match_: RoutingMatch,        // serde rename "match"
    pub target: RoutingTarget,
    pub is_default: bool,
    pub is_active: bool,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoutingMatch {
    Lpm { prefix: String },
    ExactMatch { value: String },
    Regex { pattern: String },
    Compare { op: CompareOp, value: CompareValue },
    HttpQuery { url: String, timeout_ms: Option<u32>, headers: Option<HashMap<String,String>> },
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutingTarget {
    TrunkGroup { name: String },
    Gateway { name: String },
    NextTable { name: String },
    Reject { code: u16, reason: String },
}
```

### Router wiring

`src/handler/api_v1/mod.rs`:

```rust
pub mod routing_tables;   // NEW
pub mod routing_records;  // NEW (or fold into routing_tables.rs)

let protected: Router<AppState> = Router::new()
    .merge(/* ...existing... */)
    .merge(routing_tables::router())
    .merge(routing_records::router());
```

### Test convention (IT-01)

- `tests/api_v1_routing_tables.rs` — table CRUD: 401, list happy, POST happy, POST duplicate-name 409, GET-by-name happy, GET-missing 404, PUT happy, PUT-missing 404, DELETE happy, DELETE-missing 404, validation 400 on invalid direction
- `tests/api_v1_routing_records.rs` — record CRUD: list, POST happy returns record_id, POST with position inserts, PUT-by-id, DELETE-by-id, PUT-multiple-defaults 400, missing-table 404, missing-record 404
- `tests/proxy_routing_match_types.rs` — match types end-to-end: each of 5 types matches correctly via `match_invite_with_trace`; default record fallback; next_table chain; chain-depth-3 loop detection; HttpQuery happy + timeout + 5xx fall-through

### Migration registration order

`src/models/migration.rs::Migrator::migrations` appends:

```rust
Box::new(super::routing_tables::Migration),  // create supersip_routing_tables
```

(Single migration. No drop-column migration since legacy `rustpbx_routes` stays.)

### Claude's Discretion

- Exact UUID v4 crate (likely `uuid` workspace dep already; verify in research)
- Whether `regex::Regex` is cached at table-load (in-memory `HashMap<record_id, Regex>`) or compiled per-INVITE — recommend cached for perf, invalidated on PUT
- HTTP client choice for HttpQuery — `reqwest` if already a dep; otherwise `hyper`
- Whether to put records and tables in separate handler files or one file — recommend separate for clarity (routing_tables.rs + routing_records.rs)
- Validation: max records per table (recommend 1000 hard cap to prevent DB row size blowups)
- Validation: max regex pattern length (recommend 4096 bytes)
- Whether the table `name` accepts uppercase (recommend lowercase + dashes; reject uppercase with 400 to keep URL paths predictable)
- Specific `RouteResult` extension for "matched_record_id" (the resolve response needs the matched record's ID, not just position) — adapt `RouteResult` enum during planning
</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project specs
- `.planning/REQUIREMENTS.md` §RTE — RTE-01, RTE-02, RTE-04, RTE-05 acceptance criteria
- `.planning/ROADMAP.md` — Phase 6 success criteria (4 must-be-true items)

### Phase 3 hand-off (RTE-03 / resolve integration)
- `.planning/phases/03-trunk-sub-resources-l1-and-routing-resolve/03-CONTEXT.md` §RTE-03 (D-13..D-17) — `/resolve` reuses `match_invite_with_trace`; Phase 6 must populate `matched_table` and `matched_record_index`
- `.planning/phases/03-trunk-sub-resources-l1-and-routing-resolve/03-05-SUMMARY.md` — `RoutingState::new_with_db` plumbing pattern

### Phase 5 hand-off (matcher integration patterns)
- `.planning/phases/05-trunk-enforcement-capacity-acl-codec-filter/05-CONTEXT.md` §D-15 (matcher gate sequence), §D-17 (fresh DB read per INVITE pattern), §RouteResult::Reject contract from Phase 5

### Existing code (read before designing)
- `src/proxy/routing/matcher.rs::match_invite_with_trace` — extension point for new tables source
- `src/proxy/routing/mod.rs` — RouteTrace + match infrastructure
- `src/handler/api_v1/routing.rs` — Phase 3 `/resolve` handler (the only existing /api/v1/routing/* route)
- `src/handler/api_v1/trunks.rs` — CRUD pattern reference (`/api/v1/trunks/{name}`)
- `src/handler/api_v1/trunk_credentials.rs` and `trunk_origination_uris.rs` — Phase 3 sub-resource analog patterns
- `src/handler/api_v1/trunk_acl.rs` (Phase 5) — Phase 5 sub-resource pattern with reusable validate fn
- `src/models/routing.rs` — legacy `rustpbx_routes` schema (DO NOT modify; Phase 6 leaves untouched)
- `src/models/trunk_credentials.rs` and `trunk_origination_uris.rs` — Phase 3 entity reference
- `src/config.rs::RouteResult` — extend with new resolve fields (matched_table, matched_record_id) at planning time

### CARRIER-API spec (if present)
- Search `docs/` and `.planning/specs/` for CARRIER-API references — Phase 3 cited it; Phase 6 should follow the same wire conventions for /api/v1/routing/*

### External crate docs (research will fetch via context7)
- `regex` crate — pattern compilation API
- `uuid` crate — v4 generation
- `reqwest` (if dep) — HTTP client for HttpQuery; Phase 6 uses 2s timeout
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- **`match_invite_with_trace` matcher** at `src/proxy/routing/matcher.rs` — single insertion point for new table source. Phase 6 changes the source; signature stays largely the same.
- **`RouteTrace` events** — already extensible; new variants for match types are additive
- **Phase 3 `/resolve` handler** — already returns `matched_table` and `matched_record_index` as `Option`; Phase 6 wires them
- **Phase 5 `RouteResult::Reject`** — reuse for record `target.kind == "reject"` records
- **Phase 5 sub-resource pattern (trunk_acl)** — clean adapter from JSON-stored to REST-exposed; routing_records reuses this pattern
- **`uuid` crate** — verify dep; if not present, add as workspace dep

### Established Patterns
- **Sub-resource via embedded JSON** — Phase 6 introduces this pattern (Phase 3 used FK rows). Convention: stable record_id, position-ordered, server-managed
- **`supersip_` prefix** for new tables (D-00 from Phase 3)
- **First-match-wins by priority** — existing `rustpbx_routes.priority` convention; Phase 6 reuses
- **Fresh DB read per INVITE** — Phase 5 D-17 pattern; Phase 6 D-29 follows
- **403/488/503 mapping via RouteResult::Reject** — Phase 5 contract; Phase 6 uses the `reject` target variant

### Integration Points
- `src/proxy/routing/matcher.rs` — single insertion point for new tables source
- `src/handler/api_v1/mod.rs` — router merge for two new sub-routers
- `src/models/migration.rs` — append 1 migration (create supersip_routing_tables)
- `tests/common/mod.rs` — extend `seed_*` helpers with routing-table fixtures
- `src/config.rs::RouteResult` — extend with `matched_record_id: Option<String>` (or include in trace)
</code_context>

<specifics>
## Specific Ideas

- HttpQuery records evaluated serially in position order (D-17). Operators should put cheap matches (Lpm/Exact) before HttpQuery to minimize latency.
- Default record's `match` field is ignored at evaluation; convention is to set it to a placeholder. Validator does NOT require a meaningful match for default records.
- Chain depth (next_table) capped at 3. Loop detection via visited-set in matcher state. On revisit → abort with `routing_loop_detected`.
- Legacy `rustpbx_routes` stays in schema but is silent (no API, no matching). Phase 6 SUMMARY explicitly calls out the deprecation and the manual-recreate guidance.
- HTTP client failure modes (timeout, 5xx, connection, malformed JSON) all fall through (NOT 503). 503 only on capacity/CPS exhaustion (Phase 5 contract).
- Resolve response (`matched_table`, `matched_record_index`) populated from new tables (D-30). Resolve test cases must cover all 5 match types.
- Record schema includes `is_active: bool` so operators can disable records without deleting (preserves audit trail). Inactive records skipped during matching.
</specifics>

<deferred>
## Deferred Ideas

- **Migration of `rustpbx_routes` data** into `supersip_routing_tables.records[]` — operator does this manually in Phase 6; bulk import endpoint is a v2.1 candidate
- **Routing table versioning / rollback** — v2.1; operators currently get full replace via PUT
- **Pre-fetched HttpQuery cache** — v2.1 optimization for high-volume HttpQuery deployments
- **Per-record metrics** (hit count, last-matched-at, average latency) — observability follow-up; v2.1
- **Sub-account isolation on routing tables** — Phase 13 (sub-accounts revisits all RBAC)
- **Bulk import/export** of routing tables — operator can use repeated POSTs in Phase 6; bulk endpoints v2.1
- **Parallel HttpQuery evaluation** within a table — v2.1 optimization
- **Regex pattern caching** with PUT-time invalidation — v2.0 may cache; if perf-critical, invalidation is straightforward
- **Hot-reload of routing tables** mid-call — out of scope; tables refreshed on next INVITE
- **Per-record CDR tags** (record_id in CDR for billing/observability) — Phase 11 CDR work
- **`PATCH /tables/{name}/records/{id}`** for partial record updates — v2.0 only PUT; PATCH is v2.1
- **Conditional records (time-of-day, calendar)** — out of v2.0 entirely; tracked as roadmap candidate
</deferred>

---

*Phase: 06-routing-tables-records-distribution*
*Context gathered: 2026-04-25*
