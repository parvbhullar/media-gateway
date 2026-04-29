# Phase 8: Translations Engine — Context

**Gathered:** 2026-04-29
**Status:** Ready for planning
**Source:** Discussion (12 areas batched into 3 groups; all-recommended)

<domain>
## Phase Boundary

Phase 8 ships a pre-routing rewrite engine: operator-defined regex rules normalize caller/destination numbers (and SIP URI users) on the inbound INVITE BEFORE `match_invite_with_trace` sees them. The engine is independent of the existing per-route `rewrite_rules` (which still runs at matcher time on the already-translated values). All-matching cascade ordering by priority ASC; direction-filtered (inbound/outbound/both); regexes compiled lazily and cached.

**Routes shipped (5 endpoints):**

| Route | Purpose | Source module |
|---|---|---|
| `GET /api/v1/translations` | List translations | NEW `src/handler/api_v1/translations.rs` |
| `POST /api/v1/translations` | Create translation | same |
| `GET /api/v1/translations/{name}` | Fetch by name | same |
| `PUT /api/v1/translations/{name}` | Replace | same |
| `DELETE /api/v1/translations/{name}` | Remove | same |

**Schema changes:**

- NEW table `supersip_translations` (D-00 prefix override of REQUIREMENTS.md literal `rustpbx_translations`):
  - `id` (UUID v4 string)
  - `name` (UNIQUE, lowercase + dashes — URL path segment)
  - `description: Option<String>`
  - `caller_pattern: Option<String>` (regex; ≤4096 chars; null = don't touch caller)
  - `destination_pattern: Option<String>` (regex; ≤4096 chars; null = don't touch destination)
  - `caller_replacement: Option<String>` (Rust regex `$1`/`${name}` syntax; non-empty if caller_pattern set)
  - `destination_replacement: Option<String>` (same; non-empty if destination_pattern set)
  - `direction` enum (`inbound`/`outbound`/`both`) — reuses Phase 6's routing direction enum (extend if needed)
  - `priority: i32` (default 100; ASC = first)
  - `is_active: bool` (default true)
  - `created_at, updated_at: DateTimeUtc`

**New runtime infrastructure:**

- NEW `src/proxy/translation/mod.rs`, `src/proxy/translation/engine.rs` per TRN-03
- `TranslationEngine` struct holds `Arc<DashMap<rule_id (String), Arc<Regex>>>` for compiled-regex cache
- `engine.translate(invite_option: &mut InviteOption, direction: DialDirection) -> Result<TranslationTrace>`:
  1. Fresh DB read: `Translation::find().filter(is_active=true).all(&db)`
  2. Filter by direction (inbound rule fires on inbound INVITE; both fires on either)
  3. Sort by `priority` ASC
  4. For each rule: if non-null caller_pattern matches current caller → apply replacement; same for destination. Cascade: later rules see prior rules' rewrites.
  5. Return trace of applied rules for observability/CDR
- Engine called from `src/proxy/call.rs` (or wherever the INVITE handoff to matcher happens) BEFORE `match_invite_with_trace`. Pipeline: ACL → **translation** → matcher → capacity → codec → dispatch.

**Out of scope** — explicitly deferred:

- Per-tenant translation isolation (sub-account scope) — Phase 13
- Translation execution metrics (rule hit counts) — Phase 11 observability
- Conditional translations (time-of-day, calendar) — out of v2.0
- Bulk import/export — operator uses repeated POSTs in v2.0
- Hot-reload of cache mid-call — fresh DB read per INVITE handles this
- Pre-fetched country-code lookup tables (E.164 normalization helpers) — operator writes their own regex rules
</domain>

<decisions>
## Implementation Decisions (LOCKED)

### Storage shape (TRN-01, TRN-02)

- **D-01 (table prefix override):** Table is `supersip_translations`. REQUIREMENTS.md literal `rustpbx_translations` overridden per Phase 3 D-00 lock-in (project-wide convention). Document override in 08-SUMMARY.md (same precedent as Phase 7 D-01).
- **D-02 (columns full-shape):** Per the schema list above. UUID v4 ids; name UNIQUE lowercase+dashes; pattern columns Option<String>; replacement columns Option<String> with non-empty validation when paired pattern is set; direction enum reuses Phase 6's `RoutingDirection`; priority i32 default 100; is_active default true.
- **D-03 (validation):**
  - `name`: lowercase + dashes (regex `^[a-z0-9-]+$`); range 1-64 chars
  - `caller_pattern`/`destination_pattern`: ≤4096 chars (Phase 6 regex DoS cap); must compile via `regex::Regex::new`; reject 400 on compile error
  - `caller_replacement`/`destination_replacement`: non-empty if paired pattern is set; validated by attempting `regex.replace_all` against a probe string
  - At least one of caller_pattern or destination_pattern must be non-null (rule with both null is meaningless → 400)
  - `direction`: enum (inbound/outbound/both); reject unknown
  - `priority`: i32 range [-1000, 1000]
- **D-04 (URL identifier):** `{name}` segment (lowercase + dashes), matches Phase 6 routing tables convention. Not UUID id.

### Match scope (TRN-04)

- **D-05 (independent fields):** A rule with caller_pattern=Some(p) and destination_pattern=None rewrites only the caller. A rule with both set rewrites both. A rule with both null fails validation (D-03).
- **D-06 (rule-fires-if-any-match):** Rule "fires" if at least one of its non-null patterns matches its respective field. The non-matching field is left untouched. (E.g., rule with caller_pattern set but caller doesn't match → rule does nothing; if destination_pattern set and matches → destination is rewritten regardless.)
- **D-07 (per-field independence):** A field whose pattern matches is rewritten. A field whose pattern is null is skipped. A field whose pattern doesn't match is skipped.

### Multi-rule chaining (TRN-04)

- **D-08 (all-matching cascade):** All matching rules apply in priority order (ASC = first). Each rule sees the OUTPUT of earlier rules.
- **D-09 (priority semantics):** Lower priority value = applied earlier. Default priority 100. Operators wanting "strip leading 0 first, then prepend +44" assign priority 10 to strip and 20 to prepend.
- **D-10 (cascade safety):** No infinite-loop guard needed — each rule applies once per INVITE; cascade is bounded by rule count. If rule list grows large (>100 rules per direction), planner should warn in 08-SUMMARY.md.

### Engine integration (TRN-03, TRN-04)

- **D-11 (pipeline position):** Engine runs BEFORE `match_invite_with_trace`. Pipeline order: global ACL → trunk identification → per-trunk ACL → **translation engine (D-11)** → matcher → trunk capacity gates → codec filter → dispatch. Translation runs once per INVITE; result is the matcher's input.
- **D-12 (engine surface):** `pub fn translate(invite_option: &mut InviteOption, direction: DialDirection, db: &DatabaseConnection) -> Result<TranslationTrace>` in `src/proxy/translation/engine.rs`. Mutates the InviteOption's caller/callee URIs in place.
- **D-13 (DB read pattern):** Fresh DB read per INVITE (Phase 5 D-17 / Phase 6 D-29 pattern). Compiled `Regex` instances cached in `DashMap<rule_id, Arc<Regex>>`; cache miss triggers compile + insert. Cache invalidation on PUT/DELETE via `engine.invalidate(rule_id)` called from CRUD handlers (in-process invalidation only; multi-process deployments rely on fresh DB read + compile-on-miss for new patterns).
- **D-14 (caller call site):** Called from `src/proxy/call.rs` (or wherever the matcher is invoked) BEFORE `match_invite_with_trace`. Pass-through `direction` derived from existing `DialDirection` field on call state.
- **D-15 (URI mutation helpers):** Reuse `update_uri_user` from `src/proxy/routing/matcher.rs` for caller/callee URI rewrites (existing helper). Engine doesn't reinvent URI manipulation.

### Coexistence with legacy `rewrite_rules`

- **D-16 (independent layers):** `rustpbx_routes.rewrite_rules` JSON column (existing, per-route, applied at matcher time via `apply_rewrite_rules` in `src/proxy/routing/matcher.rs:1342`) is UNCHANGED. Phase 8's translation engine runs BEFORE matcher; its output becomes the input to per-route rewrite_rules.
- **D-17 (scope boundary):** Translations = global pre-routing normalization (caller/destination user fields only). `rewrite_rules` = per-route post-match touch-ups (broader: from_user/from_host/to_user/to_host/headers). Document this boundary in CONTEXT.

### Regex semantics + caching (TRN-03)

- **D-18 (Rust regex crate):** `regex::Regex` (linear-time, no catastrophic backtracking). No `regex_syntax::with_extensions` or PCRE — keep simple.
- **D-19 (capture syntax):** Native Rust `$1`, `${name}` syntax in replacements. Validated at write-time: compile pattern, then `regex.replace_all(&probe, &replacement)` with a probe string of `"0123456789"` and catch errors.
- **D-20 (cache implementation):** `Arc<DashMap<rule_id (String), Arc<Regex>>>` — compiled regexes shared across requests. Cache miss → compile + insert. PUT/DELETE handler calls `engine.invalidate(rule_id)`. POST creates new id; cache populated on first match.
- **D-21 (pattern length cap):** ≤4096 chars per pattern (Phase 6 D-10 reuse). Reject longer patterns at PUT/POST with 400.

### Direction filter (TRN-05)

- **D-22 (direction enum):** Reuse Phase 6 `RoutingDirection` enum (`inbound`/`outbound`/`both`) at `src/models/routing_tables.rs` (or wherever it landed in Phase 6). If field name conflicts, planner picks a stable shared module location.
- **D-23 (filter logic):** `inbound` rule fires on inbound INVITE only; `outbound` on outbound only; `both` on either. Rule's `direction` is matched against the call's `DialDirection` (existing field on call state — verify exact path during planning).
- **D-24 (TRN-05 negative test):** Integration test must include an outbound-direction INVITE with inbound-only rule loaded → assert NO rewrite happens.

### Empty replacement (Q10)

- **D-25 (reject empty replacement):** PUT/POST validates: if `caller_pattern.is_some()` then `caller_replacement.is_some() && !caller_replacement.unwrap().is_empty()`. Same for destination. Empty replacement → 400 ("replacement may not be empty; use a non-empty literal or capture group").

### CRUD endpoints (TRN-02)

- **D-26 (endpoints):**
  - `GET /api/v1/translations` — list (paginated; max 1000 results)
  - `POST /api/v1/translations` — create (body shape per D-02; returns 201 with full row)
  - `GET /api/v1/translations/{name}` — fetch
  - `PUT /api/v1/translations/{name}` — replace (full record); triggers `engine.invalidate(rule_id)`
  - `DELETE /api/v1/translations/{name}` — remove; triggers `engine.invalidate(rule_id)`
- **D-27 (response shape):**
  ```json
  {
    "id": "uuid",
    "name": "uk-normalize",
    "description": null,
    "caller_pattern": "^0(\\d+)$",
    "destination_pattern": null,
    "caller_replacement": "+44$1",
    "destination_replacement": null,
    "direction": "inbound",
    "priority": 100,
    "is_active": true,
    "created_at": "...",
    "updated_at": "..."
  }
  ```

### Test fixture (TRN-06)

- **D-28 (test approach):** Lightweight integration — invoke `engine.translate(&mut invite_option, direction, &db)` directly with seeded rules. Assert mutated InviteOption caller/callee URI fields. Mirrors Phase 6 IT-04 pattern (no full proxy startup).
- **D-29 (locked test cases):**
  1. `02079460123 → +442079460123` via inbound rule `{caller_pattern: "^0(\\d+)$", caller_replacement: "+44$1", direction: inbound}`
  2. `4155551234 → +14155551234` via inbound rule `{caller_pattern: "^([2-9]\\d{9})$", caller_replacement: "+1$1", direction: inbound}`
  3. **Direction filter (TRN-05):** outbound INVITE with the above inbound-only rules → NO rewrite (asserts `caller_pattern` matches but rule skipped due to direction mismatch)
  4. **Cascade:** 2 rules in priority order — `{priority: 10, caller_pattern: "^0(\\d+)$", caller_replacement: "$1"}` (strip leading 0) + `{priority: 20, caller_pattern: "^(\\d+)$", caller_replacement: "+44$1"}` (prepend +44) → input `02079460123` → after rule 1: `2079460123` → after rule 2: `+442079460123`
  5. **Independent fields:** rule with only caller_pattern set leaves destination unchanged
  6. **Both-null rejected at write time:** POST `{name, direction: both}` (no patterns) → 400
  7. **Empty replacement rejected:** POST with non-empty pattern + empty replacement → 400

### Wire types (skeleton — planner finalizes)

```rust
// translations.rs (handler)
#[derive(Serialize, Deserialize)]
pub struct TranslationView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub caller_pattern: Option<String>,
    pub destination_pattern: Option<String>,
    pub caller_replacement: Option<String>,
    pub destination_replacement: Option<String>,
    pub direction: String,  // "inbound" | "outbound" | "both"
    pub priority: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct CreateTranslationRequest {
    pub name: String,
    pub description: Option<String>,
    pub caller_pattern: Option<String>,
    pub destination_pattern: Option<String>,
    pub caller_replacement: Option<String>,
    pub destination_replacement: Option<String>,
    pub direction: Option<String>,  // default "both"
    pub priority: Option<i32>,       // default 100
    pub is_active: Option<bool>,     // default true
}

// engine.rs (proxy)
pub struct TranslationTrace {
    pub applied_rules: Vec<AppliedRule>,
}

pub struct AppliedRule {
    pub rule_id: String,
    pub rule_name: String,
    pub field: String,           // "caller" | "destination"
    pub before: String,
    pub after: String,
}
```

### Router wiring

`src/handler/api_v1/mod.rs` (Wave 1 owns):
```rust
pub mod translations;  // NEW

let protected: Router<AppState> = Router::new()
    .merge(/* ...existing... */)
    .merge(translations::router());
```

### Migration registration order

`src/models/migration.rs::Migrator::migrations` appends:
```rust
Box::new(super::translations::Migration),  // create supersip_translations
```

### Test convention (IT-01)

- `tests/api_v1_translations.rs` — CRUD + validation: 401, list happy/empty, POST happy, POST duplicate-name 409, POST both-null-patterns 400, POST empty-replacement 400, POST invalid-regex 400, POST oversized-pattern 400, POST invalid-direction 400, GET happy/missing-404, PUT happy/missing-404, DELETE happy/missing-404
- `tests/proxy_translation_engine.rs` — IT-TRN-06 end-to-end (the 7 cases from D-29). Helper `seed_translation(name, ...)` to insert rules; invoke engine.translate; assert URI mutations.

### Claude's Discretion

- Exact location of `RoutingDirection` enum (Phase 6) — planner picks a shared module path
- Whether to expose `TranslationTrace` in CDR (recommend yes — observability follow-up; planner decides field name)
- Whether engine constructor takes `Arc<DatabaseConnection>` or borrows from caller — recommend Arc for cloneability
- Pagination defaults for GET /translations — default page=1, page_size=50, max page_size=200
- Whether the engine emits `translation.applied` webhook event — out of scope for Phase 8 (Phase 7 D-05 locked the event taxonomy; adding a new event is a follow-up); recommend adding to deferred list
</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project specs
- `.planning/REQUIREMENTS.md` §TRN — TRN-01..06 acceptance (note: TRN-01 says `rustpbx_translations`, overridden to `supersip_translations` per D-01)
- `.planning/ROADMAP.md` — Phase 8 success criteria (4 must-be-true items)

### Phase hand-offs
- `.planning/phases/03-trunk-sub-resources-l1-and-routing-resolve/03-CONTEXT.md` §D-00 — `supersip_` prefix lock-in
- `.planning/phases/06-routing-tables-records-distribution/06-CONTEXT.md` §D-10 (regex pattern length cap), §D-21 (RoutingDirection enum reuse), §D-29 (fresh DB read per INVITE pattern)
- `.planning/phases/05-trunk-enforcement-capacity-acl-codec-filter/05-CONTEXT.md` §D-17 — fresh DB read pattern
- `.planning/phases/07-webhook-pipeline/07-CONTEXT.md` §D-01 — same prefix-override precedent

### Existing code (read before designing)
- `src/proxy/routing/matcher.rs:1342-1404` — `apply_rewrite_rules` reference; `apply_rewrite_pattern_with_match` regex-replace helper (REUSE)
- `src/proxy/routing/matcher.rs::update_uri_user` — URI mutation helper (REUSE for caller rewrite)
- `src/proxy/routing/matcher.rs::update_uri_host` — same (REUSE if needed)
- `src/proxy/data.rs:1380-1423` — `handle_rewrite_key`, `normalize_rewrite_rules` for the existing per-route layer (DO NOT modify; coexist)
- `src/models/routing.rs` — legacy `rustpbx_routes.rewrite_rules` (DO NOT modify)
- `src/models/routing_tables.rs` (Phase 6) — RoutingDirection enum source
- `src/models/trunk_capacity.rs` (Phase 5), `src/models/webhooks.rs` (Phase 7) — entity + migration pattern reference
- `src/handler/api_v1/trunk_capacity.rs` (Phase 5), `routing_tables.rs` (Phase 6), `webhooks.rs` (Phase 7) — CRUD handler pattern reference
- `src/handler/api_v1/mod.rs` — router merge (Wave 1 owns)
- `src/models/migration.rs` — migration registration (Wave 1 appends)
- `src/proxy/server.rs` — engine construction at boot (verify if engine needs spawn or just on-demand call)
- `src/proxy/call.rs` — caller of `match_invite_with_trace` (Phase 8 inserts translation call BEFORE this)
- `src/call/runtime/...` — `DialDirection` enum source (verify exact path)

### External crates
- `regex` — already a dep (used by Phase 6); reuse
- `dashmap` — already a dep (used by Phase 5/7); reuse for compiled-regex cache
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- **`apply_rewrite_pattern_with_match`** at `src/proxy/routing/matcher.rs` — exact regex-replace helper translation engine reuses. No need to reinvent.
- **`update_uri_user`** / **`update_uri_host`** at same file — URI mutation primitives.
- **Phase 6 `validate_routing_record`** SSRF-style URL validators — pattern reference for translation rule validation (regex compile + length cap).
- **Phase 7 `WebhookCancelRegistry` DashMap pattern** — same shape for `TranslationRegexCache: DashMap<rule_id, Arc<Regex>>`.
- **Phase 5/6/7 sub-resource handler pattern** — Phase 8 mirrors directly.

### Established Patterns
- `supersip_` prefix override of REQUIREMENTS.md literal (Phase 7 D-01 precedent)
- Wave 1 owns mod.rs registration with stub router; downstream waves don't touch mod.rs
- Fresh DB read per INVITE (Phase 5 D-17, Phase 6 D-29)
- Stable UUID ids for sub-resources; URL path uses `name` for human-readability
- Regex pattern length cap 4096 chars (Phase 6 D-10)
- Plaintext-anything-secret-like fields (Phase 3 D-03; not relevant here — no secrets in translations)

### Integration Points
- `src/proxy/call.rs` — invocation site BEFORE `match_invite_with_trace`. Add ~3 lines: `let direction = ...; engine.translate(&mut invite_option, direction, &db).await?;`
- `src/proxy/server.rs` — engine construction at boot OR lazy on first call (engine is stateless after construction; recommend lazy via `OnceCell`)
- `src/handler/api_v1/translations.rs` PUT/DELETE handlers → call `engine.invalidate(rule_id)` (engine accessible via `state` — same pattern as cancel registry from Phase 7)
</code_context>

<specifics>
## Specific Ideas

- **Lightweight test approach** for IT-TRN-06: invoke `engine.translate` directly with seeded rules; no full proxy startup. Mirrors Phase 6 IT-04 pattern.
- **D-29 test cases include a TRN-05 negative test** — outbound INVITE with inbound-only rules → no rewrite (explicit assertion).
- **Cascade test** has 2 rules in priority order proving rule 2 sees rule 1's output: `02079460123 → 2079460123 → +442079460123`.
- **Coexistence with `rewrite_rules` layer is clean** — translation engine output flows into matcher; matcher's per-route rewrite_rules then runs on already-translated values. No changes to existing code.
- **Cache invalidation:** PUT/DELETE handlers explicitly call `engine.invalidate(rule_id)` so the next INVITE recompiles the new pattern. Multi-process deployments rely on fresh DB read + lazy compile (no shared cache).
- **TranslationTrace observability** captures rule_id/name/field/before/after for every applied rule — feeds future CDR/observability work.
- **At-least-one-of-pattern validation** at write-time prevents meaningless rules (both null → 400).
</specifics>

<deferred>
## Deferred Ideas

- **Sub-account translation isolation** — Phase 13
- **Translation hit metrics** (per-rule counters) — Phase 11 observability
- **Conditional translations** (time-of-day, calendar gates) — out of v2.0
- **Bulk import/export** — operator uses repeated POSTs in v2.0
- **Pre-fetched country-code lookup tables** (E.164 normalization helpers) — operator writes regex; lookup tables are v2.1
- **Hot-reload mid-call** — fresh DB read per INVITE handles this implicitly
- **`translation.applied` webhook event** — Phase 7 locked event taxonomy; adding new event is follow-up
- **Translation chains across tenants** — out of scope for v2.0
- **Multi-process compiled-regex cache** (shared via Redis or similar) — v2.1; current design relies on lazy per-process compile + fresh DB read
- **Translation simulator/dry-run endpoint** (e.g., `POST /api/v1/translations/simulate {caller, destination, direction}`) — v2.1 nice-to-have
- **Caller-only rules with destination-aware conditions** (e.g., "rewrite caller iff destination matches X") — out of v2.0; complex; multi-rule cascade approximates this
</deferred>

---

*Phase: 08-translations-engine*
*Context gathered: 2026-04-29*
