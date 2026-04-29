# Phase 9: Manipulations Engine — Context

**Gathered:** 2026-04-30
**Status:** Ready for planning
**Source:** Discussion (18 areas batched into 4 groups; all-recommended)

<domain>
## Phase Boundary

Phase 9 ships a post-routing rule engine: operator-defined classes evaluate conditions over caller/destination/trunk/header/var sources and fire actions (`set_header`, `remove_header`, `set_var`, `log`, `hangup`, `sleep`) — or anti_actions on the else branch. Engine runs AFTER `match_invite_with_codecs` (knows the chosen trunk per MAN-05) and BEFORE outbound INVITE hits the wire. `hangup` action short-circuits via Phase 5's `RouteResult::Reject` contract, integrating with existing `server_dialog.reject(...)` teardown in `sip_session.rs`.

**Routes shipped (5 endpoints):**

| Route | Purpose | Source module |
|---|---|---|
| `GET /api/v1/manipulations` | List manipulation classes | NEW `src/handler/api_v1/manipulations.rs` |
| `POST /api/v1/manipulations` | Create class | same |
| `GET /api/v1/manipulations/{name}` | Fetch | same |
| `PUT /api/v1/manipulations/{name}` | Replace | same |
| `DELETE /api/v1/manipulations/{name}` | Remove | same |

**Schema changes:**

- NEW table `supersip_manipulations` (D-00 prefix override of `rustpbx_manipulations` literal; same precedent as Phase 7/8)
  - `(id UUID, name UNIQUE lowercase+dashes, description Option<String>, direction enum (inbound|outbound|both), priority i32 default 100, is_active bool default true, rules: Json, created_at, updated_at)`
  - `rules` is a JSON array of Rule objects (mongo-style, like Phase 6 routing tables)

**New runtime infrastructure:**

- NEW `src/proxy/manipulation/mod.rs` + `src/proxy/manipulation/engine.rs`
- `ManipulationEngine` struct with:
  - `regex_cache: Arc<DashMap<String, Arc<Regex>>>` (keyed by `{class_id}::{rule_idx}::{condition_idx}`)
  - `var_scope: Arc<DashMap<String session_id, HashMap<String, String>>>` (per-call variable scope, cleared on call termination)
- `engine.manipulate(invite_option: &mut InviteOption, ctx: ManipulationContext, db: &DatabaseConnection) -> Result<ManipulationOutcome>`
- `ManipulationContext { caller_number, destination_number, trunk_name, direction, session_id }`
- `ManipulationOutcome { Continue { trace }, Hangup { code, reason, trace } }`

**Pipeline integration:**

- `src/proxy/call.rs::route_invite` — INSERT engine call AFTER `match_invite_with_codecs` returns `RouteResult::Trunk{name, ...}` but BEFORE actual gateway dispatch. On `Hangup` outcome, translate to `RouteResult::Reject{code, reason, retry_after_secs: None}` (Phase 5 D-15 contract); existing reject path handles teardown.

**Out of scope** — explicitly deferred:

- Cross-call/persistent variables (only per-call scope in v2.0)
- Multi-value header semantics (set_header replaces or appends single value only)
- Manipulation hot-reload mid-call (fresh DB read per INVITE handles this)
- Sub-account isolation — Phase 13
- Manipulation execution metrics (rule hit counts) — Phase 11
- Conditional manipulations (time-of-day) — out of v2.0
- Bulk import/export — operator uses repeated POSTs
- `manipulation.applied` webhook event — Phase 7 event taxonomy locked
- Header `ends_with` op — operators use `regex` instead
- Variable references inside conditions (`${var:foo}` in condition value) — actions support interpolation; conditions don't (out of v2.0 to keep eval semantics simple)
- Action timeouts beyond `sleep` cap — engine itself doesn't time-bound; operator chains responsibility
</domain>

<decisions>
## Implementation Decisions (LOCKED)

### Storage shape (MAN-01, MAN-02)

- **D-01 (table prefix override):** Table is `supersip_manipulations`. REQUIREMENTS.md literal `rustpbx_manipulations` overridden per Phase 3 D-00. Documented in 09-SUMMARY.md (same precedent as Phase 7/8).
- **D-02 (single row + embedded rules):** One row per manipulation class. `rules: Json` is an array of Rule objects (mongo-style, like Phase 6 routing tables D-01).
- **D-03 (top-level columns):** `(id UUID v4, name UNIQUE lowercase+dashes 1-64 chars, description Option<String>, direction enum (inbound|outbound|both default both), priority i32 default 100 range [-1000,1000], is_active bool default true, rules: Json default [], created_at, updated_at)`
- **D-04 (URL identifier):** `{name}` segment (lowercase+dashes; matches Phase 6/8 convention)

### Rule structure (MAN-02, MAN-07)

- **D-05 (Rule shape):**
  ```json
  {
    "name": "tag-uk-callers",
    "conditions": [...],
    "condition_mode": "and",
    "actions": [...],
    "anti_actions": []
  }
  ```
  - `name` per-rule (optional, for trace observability)
  - `conditions: Vec<Condition>` (≥1 required)
  - `condition_mode: "and" | "or"` (default `"and"`)
  - `actions: Vec<Action>` (≥1 required for is_active rule; empty allowed if anti_actions non-empty)
  - `anti_actions: Vec<Action>` (default `[]`; same Action types allowed)
  - Validation: at least one of `actions`/`anti_actions` must be non-empty (rule with both empty is meaningless → 400)

### Condition DSL (MAN-03)

- **D-06 (condition shape):**
  ```json
  {"source": "caller_number", "op": "regex", "value": "^\\+44"}
  ```
- **D-07 (sources — locked enum):** 5 source types:
  - `caller_number` — current caller (post-translation)
  - `destination_number` — current destination (post-translation)
  - `trunk` — chosen trunk_group name (from matcher)
  - `header:<name>` — SIP header value (case-insensitive name match)
  - `var:<name>` — per-call variable from prior `set_var`
  - Reject unknown source at write time: 400 ("source must be one of: caller_number, destination_number, trunk, header:<name>, var:<name>")
- **D-08 (operators — locked 6):** `equals`, `not_equals`, `regex`, `not_regex`, `starts_with`, `contains`. Reject unknown op with 400.
- **D-09 (regex compilation + cache):** Same pattern as Phase 8 D-20. `Arc<DashMap<String, Arc<Regex>>>` cached lazily on first match. Cache key: `{class_id}::{rule_idx}::{condition_idx}`. Pattern length cap 4096 chars (Phase 6 D-10).
- **D-10 (condition_mode):** `and` (default — all conditions must match → actions fire) or `or` (any condition matches → actions fire). When false → anti_actions fire instead.

### Action types (MAN-04)

- **D-11 (action shape — JSON-tagged enum):**
  ```json
  {"type": "set_header", "name": "X-Country", "value": "UK"}
  {"type": "remove_header", "name": "X-Internal"}
  {"type": "set_var", "name": "greeting", "value": "played"}
  {"type": "log", "level": "info", "message": "Routed via ${trunk}"}
  {"type": "hangup", "sip_code": 403, "reason": "Forbidden"}
  {"type": "sleep", "duration_ms": 100}
  ```
- **D-12 (set_header semantics):** Replace if header exists (case-insensitive name match); append if not. Single value (multi-value semantics deferred). `value` supports variable interpolation per D-19.
- **D-13 (remove_header semantics):** Case-insensitive name match. Removes ALL instances of named header.
- **D-14 (set_var semantics):** Writes to per-call variable scope (D-15). `value` supports variable interpolation. Re-setting same name overwrites.
- **D-15 (variable scope):** Per-call only. `Arc<DashMap<String session_id, HashMap<String, String>>>` on `ManipulationEngine`. Entry created on first `set_var`. Cleared on call termination (hook into existing call-state cleanup; planner identifies exact site). Cross-call/persistent vars deferred to v2.1.
- **D-16 (log action):** Emits `tracing::{info|warn|error}!` event with structured fields `{event: "manipulation_log", message, session_id, class_name, rule_name}`. Level enum: `info|warn|error` (debug omitted to keep operator-facing minimal). `message` supports variable interpolation.
- **D-17 (hangup action):** `sip_code` range [400, 699]; reason is human-readable string ≤256 chars. Engine returns `ManipulationOutcome::Hangup{code, reason, trace}`. Caller in `src/proxy/call.rs` translates to `RouteResult::Reject{code, reason, retry_after_secs: None}` per Phase 5 D-15. Existing reject path → `server_dialog.reject(...)` in `sip_session.rs:639` already handles teardown. CDR records reason `manipulation_hangup_<code>`.
- **D-18 (sleep action):** `duration_ms` range [10, 5000]. Async `tokio::time::sleep`. **5000ms hard cap** at write time (anti-DoS); operators chain multiple sleeps if needed. Reject `> 5000` with 400.

### Variable interpolation (D-12, D-14, D-16 referent)

- **D-19 (interpolation syntax):** `${source}` and `${source:name}` placeholders in action `value` and `message` fields. Same source enum as conditions (D-07).
- **D-20 (unknown source/var):** Resolves to empty string + `tracing::warn!` (warn-and-continue, not error). Operator sees the warning and fixes the rule.
- **D-21 (interpolation site):** Applied to `value` field in `set_header`, `set_var`, and `message` field in `log`. Other fields (header `name`, `sip_code`, `duration_ms`) are NOT interpolated.

### Pipeline integration (MAN-05)

- **D-22 (call site):** `src/proxy/call.rs::route_invite`. INSERT after `match_invite_with_codecs` returns `Ok(RouteResult::Trunk{name, ...})` but BEFORE the gateway dispatch call. ≤15-line additive insertion (mirrors Phase 8 ≤12-line bound but slightly larger since we extract trunk name + build context).
- **D-23 (engine signature):**
  ```rust
  pub async fn manipulate(
      &self,
      invite_option: &mut InviteOption,
      ctx: ManipulationContext,
      db: &DatabaseConnection,
  ) -> Result<ManipulationOutcome>;
  ```
- **D-24 (ManipulationContext shape):**
  ```rust
  pub struct ManipulationContext {
      pub caller_number: String,
      pub destination_number: String,
      pub trunk_name: String,           // chosen trunk from matcher
      pub direction: DialDirection,
      pub session_id: String,
  }
  ```
- **D-25 (ManipulationOutcome):**
  ```rust
  pub enum ManipulationOutcome {
      Continue { trace: ManipulationTrace },
      Hangup { code: u16, reason: String, trace: ManipulationTrace },
  }
  ```
- **D-26 (cleanup hook for var_scope):** On call termination (`sip_session.rs` hangup path), call `engine.cleanup_session(&session_id)` to drop the per-call variable map. Planner identifies the exact integration site; recommend adding to existing hangup handler.

### Multi-rule chaining (MAN-04)

- **D-27 (cascade within class):** All matching rules apply in order (Phase 8 D-08 pattern). Actions accumulate; later rules see prior `set_var`/`set_header` results.
- **D-28 (cascade across classes):** Direction filter (inbound/outbound/both) applies; within direction, classes sorted by `priority` ASC. Cascade across classes (later classes see prior class' mutations).
- **D-29 (hangup short-circuit):** On `hangup` action: stop further action evaluation in current rule, stop further rules in class, stop further classes. Engine returns `Hangup` immediately.
- **D-30 (anti-actions execution):** Anti-actions execute when condition_mode result is false. Same constraints as actions (header allowlist, sleep cap). Anti-actions can include `hangup` (use case: "if NOT from US carrier → reject").

### Header mutation safety (D-12, D-13)

- **D-31 (forbidden header allowlist — reject at write time):** Reject mutation of system-critical headers in `set_header.name` and `remove_header.name`:
  - `Via`, `From`, `To`, `Call-ID`, `CSeq`, `Contact`, `Max-Forwards`, `Content-Length`, `Content-Type`
  - Case-insensitive match
  - 400 with message: "header '<name>' is system-critical and cannot be mutated; allowed examples: User-Agent, P-Asserted-Identity, X-*"
- **D-32 (allowed headers):** Everything else, including all `X-*` custom headers, `User-Agent`, `P-Asserted-Identity`, `Diversion`, `Privacy`, `Remote-Party-ID`, `History-Info`, etc.

### CRUD endpoints (MAN-02)

- **D-33 (endpoints):**
  - `GET /api/v1/manipulations` — list (paginated; max 1000)
  - `POST /api/v1/manipulations` — create (returns 201)
  - `GET /api/v1/manipulations/{name}` — fetch
  - `PUT /api/v1/manipulations/{name}` — replace; calls `engine.invalidate_class(class_id)` + clears regex cache entries for that class
  - `DELETE /api/v1/manipulations/{name}` — remove; same invalidation
- **D-34 (validation pipeline at PUT/POST):**
  1. Name format `^[a-z0-9-]+$`, 1-64 chars
  2. Direction enum
  3. Priority range [-1000, 1000]
  4. Each rule has ≥1 condition
  5. Each rule has ≥1 action OR ≥1 anti_action (D-05)
  6. Condition source in locked enum (D-07)
  7. Condition op in locked enum (D-08)
  8. Regex patterns compile + ≤4096 chars (D-09)
  9. Action types in locked enum (D-11)
  10. `set_header`/`remove_header` name NOT in forbidden list (D-31)
  11. `hangup.sip_code` in [400, 699]
  12. `sleep.duration_ms` in [10, 5000] (D-18)
  13. `log.level` in `info|warn|error`
  14. Variable interpolation syntax valid (parse `${...}` placeholders; unknown source flagged at write-time? — recommend: warn-only at write-time, runtime warn+empty per D-20; do NOT block on unknown var since vars may be set by earlier rules)

### Test fixture (IT-02, MAN tests)

- **D-35 (test approach):** Lightweight integration — invoke pipeline functions directly with seeded translation + manipulation rules. Mirrors Phase 6/8 IT pattern.
- **D-36 (locked test cases — IT-02 + MAN coverage):**
  1. **Cross-engine (IT-02):** Translation `02079460123 → +442079460123`; manipulation `caller_number regex ^\\+44 → set_header X-Country UK`. Assert both: caller rewritten + header set.
  2. **Trunk-source condition (MAN-05):** Manipulation `trunk equals us-carrier → set_header X-Region US`. Assert it sees post-routing trunk name.
  3. **Hangup short-circuit (MAN-06):** Rule `action: hangup 403 Forbidden`. Assert `RouteResult::Reject{code:403, reason:"Forbidden"}`. Assert no further actions execute (test by adding a `set_header X-Should-Not-Set` after hangup; assert header NOT present).
  4. **Anti-actions (MAN-07):** Rule `condition: caller_number regex ^\\+44, actions: [set_header X-Country UK], anti_actions: [set_header X-Country OTHER]`. Caller=US number → assert `X-Country: OTHER`.
  5. **Cascade within class:** 2 rules — rule 1 `set_var x=1`, rule 2 `condition: var:x equals 1, action: set_header X-Cascade YES`. Assert header set.
  6. **Variable interpolation:** `set_header X-Caller "${caller_number}"` → assert header value matches post-translation caller.
  7. **Header allowlist rejection (write-time D-31):** POST manipulation with `set_header Via foo` → 400.
  8. **Sleep cap (write-time D-18):** POST with `sleep 6000` → 400. POST with `sleep 100` → 201; runtime adds ~100ms.
  9. **Direction filter:** outbound-only manipulation does NOT fire on inbound INVITE.
  10. **Or-mode condition:** rule `condition_mode: or, conditions: [{caller =~ ^\\+44}, {caller =~ ^\\+1}]` → matches both UK and US callers.
  11. **set_var/var cross-rule:** rule 1 `set_var country=UK` (always), rule 2 `condition: var:country equals UK, action: log info "got uk"` → assert log emitted (capture via tracing test subscriber).
  12. **Per-call var isolation:** two simulated INVITEs with same rule setting var=A in one call doesn't affect the other (separate session_ids).

### Wire types (skeleton — planner finalizes)

```rust
// manipulations.rs (handler)
#[derive(Serialize, Deserialize)]
pub struct ManipulationView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub direction: String,    // "inbound" | "outbound" | "both"
    pub priority: i32,
    pub is_active: bool,
    pub rules: Vec<Rule>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Rule {
    pub name: Option<String>,
    pub conditions: Vec<Condition>,
    pub condition_mode: ConditionMode,        // and | or
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub anti_actions: Vec<Action>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Condition {
    pub source: String,    // "caller_number" | "destination_number" | "trunk" | "header:<name>" | "var:<name>"
    pub op: ConditionOp,    // equals | not_equals | regex | not_regex | starts_with | contains
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    SetHeader { name: String, value: String },
    RemoveHeader { name: String },
    SetVar { name: String, value: String },
    Log { level: LogLevel, message: String },     // info | warn | error
    Hangup { sip_code: u16, reason: String },
    Sleep { duration_ms: u32 },
}

// engine.rs (proxy)
pub struct ManipulationTrace {
    pub applied_rules: Vec<AppliedRule>,
    pub triggered_actions: Vec<TriggeredAction>,
}

pub enum ManipulationOutcome {
    Continue { trace: ManipulationTrace },
    Hangup { code: u16, reason: String, trace: ManipulationTrace },
}
```

### Router wiring

`src/handler/api_v1/mod.rs` (Wave 1 owns):
```rust
pub mod manipulations;  // NEW

let protected: Router<AppState> = Router::new()
    .merge(/* ...existing... */)
    .merge(manipulations::router());
```

### Migration registration order

`src/models/migration.rs::Migrator::migrations` appends:
```rust
Box::new(super::manipulations::Migration),  // create supersip_manipulations
```

### Test convention (IT-01 + IT-02)

- `tests/api_v1_manipulations.rs` — CRUD + write-time validation: 401, list happy/empty, POST happy, POST duplicate-name 409, POST invalid-rule (no conditions) 400, POST invalid-rule (no actions and no anti_actions) 400, POST invalid-source 400, POST invalid-op 400, POST invalid-regex 400, POST oversized-pattern 400, POST forbidden-header 400, POST sip_code-out-of-range 400, POST sleep-cap-exceeded 400, POST invalid-log-level 400, GET happy/missing-404, PUT happy + invalidate, PUT-missing 404, DELETE happy + invalidate, DELETE-missing 404
- `tests/proxy_manipulations_pipeline.rs` — IT-02 + MAN engine tests (12 cases per D-36)

### Claude's Discretion

- Exact integration site for `engine.cleanup_session(...)` in hangup path — planner identifies during research/exec
- Whether `ManipulationEngine` is constructed eagerly at boot or lazily — recommend eager (matches Phase 7/8)
- Whether log action's structured fields include `class_name` AND `rule_name` or just one — recommend both for trace clarity
- Precise interpolation parser implementation (regex-based vs custom) — recommend `regex::Regex::new("\\$\\{([^}]+)\\}")` with replace_all closure
- Whether `set_var` value is interpolated at set-time or read-time — recommend set-time (deterministic; later rules see resolved value)
- Pagination defaults for GET /manipulations — page=1, page_size=50, max=200 (consistent with Phase 8 D-26)
- Whether to add per-class metadata (e.g., `tags: Vec<String>` for operator search) — out of v2.0; v2.1
- Whether to expose `ManipulationTrace` in CDR — recommend yes (observability follow-up; planner picks field name)
</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project specs
- `.planning/REQUIREMENTS.md` §MAN, §IT-02 — MAN-01..07 + IT-02 acceptance
- `.planning/ROADMAP.md` — Phase 9 success criteria

### Phase hand-offs
- `.planning/phases/03-trunk-sub-resources-l1-and-routing-resolve/03-CONTEXT.md` §D-00 — `supersip_` prefix
- `.planning/phases/05-trunk-enforcement-capacity-acl-codec-filter/05-CONTEXT.md` §D-15 (RouteResult::Reject contract) + §D-17 (fresh DB read pattern)
- `.planning/phases/06-routing-tables-records-distribution/06-CONTEXT.md` §D-01 (embedded JSON pattern), §D-10 (regex DoS cap), §D-21 (RoutingDirection enum reuse)
- `.planning/phases/08-translations-engine/08-CONTEXT.md` §D-08..D-29 — closest analog (engine pattern, cascade, direction filter, cache, IT pattern)

### Existing code (read before designing)
- `src/proxy/proxy_call/sip_session.rs` (esp. line 639 `server_dialog.reject(...)`, line 168 `pending_hangup`, line 178 `hangup_reason`, line 471 `hangup_messages`) — hangup integration site
- `src/proxy/call.rs::route_invite` — call site for engine insertion (Phase 8 already inserts translation engine; Phase 9 inserts manipulation AFTER matcher AFTER translation)
- `src/proxy/call.rs:1414-1420` — example of header push pattern (`headers.push(rsipstack::sip::Header::Other(...))`) — REUSE pattern for `set_header`
- `src/proxy/translation/engine.rs` — Phase 8 closest analog; reuse architecture (DashMap regex cache, fresh DB read, direction filter, priority cascade)
- `src/proxy/routing/matcher.rs::update_uri_user`, `apply_rewrite_pattern_with_match` — URI/regex helpers (Phase 8 reused; Phase 9 doesn't need URI mutation but may use regex helper)
- `src/handler/api_v1/translations.rs` (Phase 8) — CRUD handler analog
- `src/handler/api_v1/routing_records.rs` (Phase 6) — embedded-array CRUD pattern (closest match for embedded `rules: Json`)
- `src/models/translations.rs` (Phase 8) — entity + migration analog
- `src/handler/api_v1/mod.rs` — Wave 1 router merge
- `src/models/migration.rs` — Wave 1 migration register
- `src/proxy/server.rs` — engine construction at boot
- `src/app.rs` — AppState plumbing

### External crates (already deps)
- `regex` — pattern compilation (Phase 6/8 reuse)
- `dashmap` — cache (Phase 5/7/8 reuse)
- `tokio::time::sleep` — sleep action
- `tracing::{info,warn,error}!` — log action
- `uuid` — id generation
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- **Phase 8 `TranslationEngine`** at `src/proxy/translation/engine.rs` — direct architectural analog. Phase 9 mirrors:
  - DashMap regex cache pattern
  - Fresh DB read per INVITE
  - Direction filter
  - Priority ASC cascade
  - `pub async fn ...(&self, &mut InviteOption, ...) -> Result<...Outcome>` signature
- **Phase 5 `RouteResult::Reject{code, reason, retry_after_secs}`** — hangup short-circuit translation target (D-17, D-25)
- **Phase 6 routing_records embedded JSON pattern** — Phase 9 `rules: Json` reuses this exactly
- **`server_dialog.reject(Some(code), Some(reason))` in `sip_session.rs:639`** — existing reject path; manipulation hangup leverages via `RouteResult::Reject` (no new wiring needed)
- **Phase 8 D-26 `engine.invalidate(rule_id)` hook on PUT/DELETE** — Phase 9 reuses pattern (`engine.invalidate_class(class_id)`)

### Established Patterns
- Wave 1 owns mod.rs/migration.rs/server.rs/app.rs (Phase 5/6/7/8 lesson)
- `supersip_` prefix override of REQUIREMENTS literal (Phase 7 D-01, Phase 8 D-01 precedent)
- Stable UUID ids, name-keyed URLs (Phase 6/8)
- Plaintext-anything (Phase 3 D-03 — though not relevant here, no secrets)
- Regex pattern length cap 4096 (Phase 6 D-10)
- `RoutingDirection` enum reuse (Phase 6/8)
- Lightweight integration tests via direct engine invocation with seeded rules (Phase 6/8 IT pattern)

### Integration Points
- `src/proxy/call.rs::route_invite` — Phase 8 already inserts translation engine BEFORE `match_invite_with_codecs`; Phase 9 inserts manipulation engine AFTER matcher returns `RouteResult::Trunk{...}`. Order: translation → matcher (capacity/codec/ACL gates) → manipulation → dispatch.
- `sip_session.rs` hangup cleanup hook — call `engine.cleanup_session(session_id)` to clear per-call var scope. Planner identifies exact site.
- `src/handler/api_v1/manipulations.rs` PUT/DELETE → `engine.invalidate_class(class_id)`
- Existing reject path triggered by `RouteResult::Reject` — handles teardown automatically; no new code needed for hangup integration beyond `Hangup → Reject` translation in call.rs.
</code_context>

<specifics>
## Specific Ideas

- **Engine architecture mirrors Phase 8 1:1** — same DashMap cache, fresh DB read, priority cascade, direction filter. Reduces design risk; reuses tested patterns.
- **Hangup integration is "just" a Reject translation** — no new SIP teardown code needed; existing `RouteResult::Reject` path (Phase 5) handles everything. CDR reason `manipulation_hangup_<code>` mirrors Phase 5's reason taxonomy.
- **Cascade across classes** is the most operator-friendly default — operators write modular classes (e.g., "tag-uk-callers", "carrier-specific-headers") and order via `priority` without per-class first-match-wins surprises.
- **Per-call var scope cleanup** is the only new lifecycle hook — planner picks the integration site in sip_session.rs (likely the hangup completion path).
- **Variable interpolation in `set_var.value`** allows chained transformations: `set_var x="${caller_number}"` then `set_var y="${var:x}_suffix"`.
- **Header allowlist (D-31)** is operator-safety net — prevents accidental SIP-state-machine breakage. The list is conservative (only the 9 most-critical headers); operator can mutate everything else including all `X-*`.
- **IT-02 cross-engine test #1** is the key proof point — translation rewrites caller, manipulation matches the rewritten value, both engines fire in the same call. Validates pipeline ordering AND interaction.
- **Anti-actions enable conditional fallbacks** (D-30): "if caller is from US → set X-Country US; else (anti_action) → set X-Country UNKNOWN". Cleaner than separate else-rule.
- **Sleep 5000ms cap is a deliberate compromise** (D-18): allows operator-driven dispatch delay (e.g., for downstream system warm-up) while protecting against accidental DoS. Operators chain multiple sleeps for longer waits.
</specifics>

<deferred>
## Deferred Ideas

- **Cross-call/persistent variables** (var scope beyond single call) — v2.1
- **Multi-value header semantics** (e.g., add second `Via` header) — v2.1; current single-value covers operator needs
- **Manipulation hot-reload mid-call** — fresh DB read per INVITE handles this implicitly
- **Sub-account isolation** on manipulations — Phase 13
- **Per-rule execution metrics** (hit count, last-fired-at, average latency) — Phase 11 observability
- **Conditional manipulations** (time-of-day, calendar gates) — out of v2.0
- **Bulk import/export** — v2.1
- **`manipulation.applied` webhook event** — Phase 7 event taxonomy locked; new events deferred
- **Header `ends_with` op** — operators use `regex` instead
- **Variable references inside conditions** (`${var:foo}` in condition `value`) — actions support interpolation; conditions don't (keeps eval semantics simple). v2.1.
- **Manipulation simulator/dry-run endpoint** — v2.1 nice-to-have
- **Chained-rule loops** (rule jumps to another rule) — out of v2.0; cascade approximates this
- **Conditional anti-actions** (different anti_actions per condition that failed) — out of v2.0; cascade + multi-rule pattern approximates
- **`sleep` precision below 10ms** — capped at 10ms minimum to reduce CPU thrash; operators don't need sub-10ms precision in dispatch
- **Custom action types** beyond the 6 locked — extensibility deferred to v2.1
- **Rule-level `is_active` field** (toggle individual rule without removing) — v2.1; current granularity is class-level
</deferred>

---

*Phase: 09-manipulations-engine*
*Context gathered: 2026-04-30*
