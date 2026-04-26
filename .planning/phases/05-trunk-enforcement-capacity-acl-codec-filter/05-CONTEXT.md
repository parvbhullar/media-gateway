# Phase 5: Trunk Enforcement (Capacity, ACL, Codec Filter) — Context

**Gathered:** 2026-04-25
**Status:** Ready for planning
**Source:** Discussion (8 areas batched into 3 groups)

<domain>
## Phase Boundary

Phase 5 promotes Phase 2/3 schema fields (`trunk_group.acl`, `media_config.codecs`) and a NEW capacity sub-resource into proxy hot-path enforcement. Caller→trunk identification, dispatch-entry capacity gate, ingress-level per-trunk ACL, and pre-dispatch codec intersection all observe and act on these fields. Capacity is also surfaced for live observability via GET.

**Routes shipped (5 endpoints):**

| Route | Purpose | Source module |
|---|---|---|
| `GET /api/v1/trunks/{name}/capacity` | Read capacity + live counts | NEW `src/handler/api_v1/trunk_capacity.rs` |
| `PUT /api/v1/trunks/{name}/capacity` | Replace capacity (max_calls, max_cps) | same |
| `GET /api/v1/trunks/{name}/acl` | List ACL entries | NEW `src/handler/api_v1/trunk_acl.rs` |
| `POST /api/v1/trunks/{name}/acl` | Add ACL entry | same |
| `DELETE /api/v1/trunks/{name}/acl/{entry}` | Remove ACL entry by rule | same |

**Schema changes:**

- NEW table `supersip_trunk_capacity` (id, trunk_group_id FK CASCADE UNIQUE, max_calls Option<i32>, max_cps Option<i32>, created_at, updated_at) — 1 row per trunk_group
- NEW table `supersip_trunk_acl_entries` (id, trunk_group_id FK CASCADE, rule String, position i32, created_at) — UNIQUE (trunk_group_id, rule)
- DROP column `rustpbx_trunk_groups.acl` JSON — promoted to multi-row table (Phase 2 pattern; safe because sub-account isolation hasn't shipped — sip_fix branch unmerged)

**Proxy enforcement changes (no schema):**

- NEW `src/proxy/trunk_capacity_state.rs` — `DashMap<i64, TrunkCapacityGate>` where gate = `{atomic_active: AtomicU32, token_bucket: TokenBucket}`
- NEW `src/proxy/routing/codec_normalize.rs` — lowercase ↔ RFC 3551 codec name normalization (D-10 follow-up)
- EXTEND `src/proxy/routing/matcher.rs` — pre-dispatch codec intersection check after trunk resolution; capacity gate check at dispatch entry; per-trunk ACL check after `src_ip` is known
- 488 Not Acceptable Here on codec mismatch (TSUB-06); 503 Service Unavailable + `Retry-After: 5` on capacity exhaustion (TSUB-04); 403 Forbidden on per-trunk ACL deny

**Out of scope** — explicitly deferred:

- Per-trunk capacity persistence across restarts (active count is in-memory only) — operator concern; observable through `current_active`
- Carrier-side congestion feedback (overload throttling) — v2.1
- Sub-account isolation on enforcement (cross-tenant ACL) — Phase 13
- Codec transcoding fallback (instead of 488) — out of v2.0; carrier MUST match
- Hot-reload of capacity/ACL changes mid-call — capacity changes take effect on next INVITE; in-flight calls unaffected
</domain>

<decisions>
## Implementation Decisions (LOCKED)

### Capacity storage & tracking (TSUB-04, TSUB-07)

- **D-01:** Capacity lives in NEW table `supersip_trunk_capacity` with UNIQUE FK to `rustpbx_trunk_groups.id` (sub-resource pattern, consistent with TSUB-01..03). 1 row per trunk_group max. PUT is upsert-replace; GET joins. No row → `max_calls=null, max_cps=null` (unlimited).
- **D-02:** Active count source is `ActiveProxyCallRegistry` filtered by `trunk_group_name` for **GET response observability** (read-side, eventually consistent, no new state).
- **D-03:** Enforcement uses an in-memory atomic gate at dispatch entry: `TrunkCapacityState: DashMap<i64 trunk_group_id, TrunkCapacityGate>`. Increment-and-check pattern: `gate.try_acquire(max_calls) -> Result<Permit, CapacityFull>`. Permit drops when call ends (registry cleanup hook). Gate is separate from registry to keep the registry leg-counting clean.
- **D-04:** GET response shape:
  ```json
  {
    "max_calls": 100,
    "max_cps": 10,
    "current_active": 42,
    "current_cps_rate": 7
  }
  ```
  `current_active` from registry snapshot; `current_cps_rate` from token bucket's last-refill state. Both null when no capacity row exists (still observable, just shows live counts with no limit).
- **D-05:** PUT body: `{max_calls?: u32, max_cps?: u32}` — both optional (null = unlimited). No values stored as 0; reject 0 with 400 ("use null for unlimited").

### CPS rate limiting (TSUB-04)

- **D-06:** Token-bucket algorithm in `TrunkCapacityGate`. Capacity = `max_cps`, refill rate = `max_cps tokens/sec`. Each new INVITE consumes 1 token. Empty bucket → 503.
- **D-07:** Implementation: `governor` crate (or hand-rolled with `AtomicU64` epoch-millis + `AtomicU32` available tokens). Decide in research phase based on existing crate dependencies; prefer `governor` if already a dep, otherwise hand-rolled to avoid new dep.
- **D-08:** No persistence — token bucket resets on restart. CPS is a burst-smoothing concern, not a billing one.

### Capacity exhaustion response

- **D-09:** Both `max_calls` exhaustion and CPS bucket-empty return **SIP 503 Service Unavailable** with `Retry-After: 5` header. Matches Twilio/Telnyx carrier convention. CDR records the rejection with reason `trunk_capacity_exhausted` or `trunk_cps_exhausted` distinguishably.

### ACL CRUD shape (TSUB-05)

- **D-10:** ACL promoted from `trunk_group.acl: Option<Json>` JSON column to NEW table `supersip_trunk_acl_entries` (id, trunk_group_id FK CASCADE, rule String, position i32 auto-assigned, created_at). UNIQUE (trunk_group_id, rule). Mirror Phase 3 credentials/origination_uris pattern (D-00 prefix, IT-01 test convention).
- **D-11:** DROP `rustpbx_trunk_groups.acl` column in same migration. Existing data is empty (no production deploy). Migration registration order: create new table → drop old column.
- **D-12:** Wire format. GET returns `[{rule: "allow 1.2.3.4/24", position: 0}, ...]` ordered by position. POST takes `{rule}`, assigns next position. DELETE-by-rule is strict 404-on-miss; URL-encoded.
- **D-13:** Rule format: `^(allow|deny) (all|<CIDR>|<IP>)$`. Validate via existing CIDR parser (`src/proxy/acl.rs` patterns from `tests/test_acl.rs`). 400 on parse failure.
- **D-14:** Default policy = **allow**. ACL is evaluated top-to-bottom; first match wins; if no match, allow. Mirrors Phase 1 global ACL semantics.

### ACL enforcement integration

- **D-15:** Extend the existing global ACL handler to also consult per-trunk ACL **after** trunk identification (i.e., after `match_invite_with_trace` resolves the inbound trunk_group from `src_ip`+route lookup). Sequence: global firewall → trunk resolved → per-trunk ACL check → capacity gate → codec intersection → dispatch.
- **D-16:** Per-trunk ACL deny returns SIP 403 Forbidden (matches global ACL convention). CDR records `reason: trunk_acl_blocked`.
- **D-17:** Per-trunk ACL is loaded fresh from DB on each INVITE (no cache) for Phase 5. If perf becomes an issue, cache layer is a v2.1 hardening concern. Caching is NOT a Phase 5 requirement.

### Codec mismatch rejection (TSUB-06)

- **D-18:** Pre-dispatch SDP parse + early 488 in `match_invite_with_trace` (or just after, before gateway selection). Once trunk is identified, intersect caller's SDP `m=audio` codec list with `media_config.codecs`. Empty intersection → 488 Not Acceptable Here.
- **D-19:** Codec normalization helper at NEW `src/proxy/routing/codec_normalize.rs`:
  - `normalize(codec: &str) -> CanonicalCodec` — lowercase storage form ↔ RFC 3551 uppercase wire form
  - Accepts both: `"pcmu" | "PCMU" | "0"` (RTP payload type) → `Pcmu`
  - Used by both Phase 3 wire validation (already lowercase) and Phase 5 SDP intersection
- **D-20:** When `media_config.codecs` is empty/null, codec filter is **disabled** (allow all). This matches Phase 3 D-11 (empty config means "no opinion").
- **D-21:** CDR records `reason: codec_mismatch_488` with the unmatched caller-offer codec list for observability.

### Test fixture strategy (IT-03)

- **D-22:** Each new sub-router gets its own test file (IT-01 convention):
  - `tests/api_v1_trunk_capacity.rs` — 401, GET-empty-defaults, PUT happy + GET round-trip, PUT zero-value 400, DELETE not exposed (replace via PUT), parent-missing 404
  - `tests/api_v1_trunk_acl.rs` — 401, list, POST happy, POST duplicate-rule 409, POST invalid-syntax 400, DELETE happy, DELETE-missing 404, parent-missing 404
- **D-23:** IT-03 enforcement integration tests live in NEW `tests/proxy_trunk_enforcement.rs` (proxy-layer test, not api_v1):
  - capacity exhaustion → 503 + Retry-After header
  - CPS exhaustion → 503 + Retry-After header
  - codec mismatch → 488
  - per-trunk ACL deny → 403
  - happy path through all gates → dispatch succeeds
- **D-24:** Test fixture pattern mirrors Phase 4 `seed_active_call` and Phase 3 `tests/api_v1_routing_resolve.rs`: helper `seed_trunk_with_enforcement(name, capacity, acl_entries, codecs)` + simulated INVITE through `match_invite_with_trace` asserting `MatchOutcome::Reject{code, reason}` or success.

### Migration registration order

`src/models/migration.rs::Migrator::migrations` appends in this order (FK-dependent):

```rust
Box::new(super::trunk_capacity::Migration),         // creates supersip_trunk_capacity
Box::new(super::trunk_acl_entries::Migration),      // creates supersip_trunk_acl_entries
Box::new(super::drop_acl_column::Migration),        // drops rustpbx_trunk_groups.acl LAST
```

### Wire types (skeleton — planner finalizes)

```rust
// trunk_capacity.rs
#[derive(Serialize)]
pub struct TrunkCapacityView {
    pub max_calls: Option<u32>,
    pub max_cps: Option<u32>,
    pub current_active: u32,
    pub current_cps_rate: u32,
}

#[derive(Deserialize)]
pub struct PutTrunkCapacityRequest {
    pub max_calls: Option<u32>,
    pub max_cps: Option<u32>,
}

// trunk_acl.rs
#[derive(Serialize)]
pub struct TrunkAclEntryView { pub rule: String, pub position: i32 }

#[derive(Deserialize)]
pub struct AddTrunkAclEntryRequest { pub rule: String }
```

### Router wiring

`src/handler/api_v1/mod.rs`:

```rust
pub mod trunk_capacity;  // NEW
pub mod trunk_acl;       // NEW

let protected: Router<AppState> = Router::new()
    .merge(/* ...existing... */)
    .merge(trunk_capacity::router())  // NEW
    .merge(trunk_acl::router());      // NEW
```

### Claude's Discretion

- Exact `governor` crate version vs hand-rolled token bucket — research phase decides based on existing dependencies
- Whether `current_cps_rate` reads the token bucket directly or computes from a rolling 1-second window — both are reasonable; pick whatever's cleaner once `governor` choice is locked
- Permit lifecycle for capacity gate — RAII `Drop` impl on the active-call registry entry vs explicit `gate.release()` call; recommend RAII for crash safety
- ACL rule case sensitivity — recommend case-insensitive on `allow`/`deny` keywords, exact match on CIDR/IP (rust standard)
- CDR rejection-reason field name — pick something consistent with Phase 4 hangup reasons; check existing CDR schema during research
- Whether codec normalization caches lookup tables — micro-optimization; profile in research phase
- Test for race condition where capacity gate releases concurrent with new INVITE — `loom`-style test or just integration test with sequential calls; pick whatever's pragmatic
</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project specs
- `.planning/REQUIREMENTS.md` §TSUB / §IT — TSUB-04, TSUB-05, TSUB-06, TSUB-07, IT-03 acceptance criteria
- `.planning/ROADMAP.md` — Phase 5 success criteria (4 must-be-true items)

### Phase 3 hand-off (relevant decisions)
- `.planning/phases/03-trunk-sub-resources-l1-and-routing-resolve/03-CONTEXT.md` §Naming Convention (D-00 — supersip_ prefix), §Media config storage (D-09 codec list shape, D-10 normalization deferred to Phase 5)
- `.planning/phases/03-trunk-sub-resources-l1-and-routing-resolve/03-01-SUMMARY.md` — sub-resource table pattern reference

### Existing code (read before designing)
- `src/proxy/routing/matcher.rs::match_invite_with_trace` — dispatch entry, codec hints already attached at `hints.allow_codecs`
- `src/proxy/active_call_registry.rs` — registry source for `current_active` count (filter by trunk_group_name)
- `src/proxy/call.rs` — codec setup at `dialplan.allow_codecs`, 488 already returned for "service or option not available"
- `src/proxy/tests/test_acl.rs` — global ACL parse + match patterns (allow/deny CIDR/all)
- `src/models/trunk_group.rs:75` — existing `acl: Option<Json>` column to be DROPPED
- `src/models/trunk_credentials.rs` + `trunk_origination_uris.rs` — Phase 3 sub-resource table reference patterns
- `src/handler/api_v1/trunks.rs` — parent trunk lookup pattern (404 on parent-missing)
- `src/proxy/data.rs` — `max_calls`/`max_cps` legacy mapping from `sip_trunk` (not the same path; legacy)

### CARRIER-API spec (if present)
- Search `docs/` and `.planning/specs/` for `CARRIER-API` references — Phase 3 cited it; Phase 5 should reuse the same wire conventions
</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- **`ActiveProxyCallRegistry` snapshot** — `registry.list_recent()` filtered by `trunk_group_name` gives `current_active` for free; no new state for observability
- **`src/proxy/tests/test_acl.rs` parse helpers** — CIDR/IP/`all` matching is already implemented; per-trunk ACL just needs the same parser pointed at a different rule list
- **Phase 3 sub-resource pattern** — `trunk_credentials.rs` + `trunk_origination_uris.rs` are the analog files for `trunk_capacity.rs` + `trunk_acl.rs`. Same parent-trunk-name routing, same FK cascade, same DELETE-by-segment 404 semantics
- **`hints.allow_codecs` plumbing in matcher.rs** — codec list already flows from rule to dialplan; Phase 5 inserts the intersection check before the hint is consumed
- **CDR rejection-reason field** — already used by Phase 4 hangup paths; reuse for trunk_capacity_exhausted, trunk_acl_blocked, codec_mismatch_488

### Established Patterns
- **Sub-resource → drop-column** — Phase 3 D-02 (drop `trunk_group.credentials` JSON when `supersip_trunk_credentials` table lands). Phase 5 repeats for `trunk_group.acl`.
- **403 for ACL deny** — global ACL convention; per-trunk reuses
- **488 for codec mismatch** — already the right code in `src/proxy/call.rs`; Phase 5 makes it pre-dispatch instead of mid-call
- **Test file per sub-router** — IT-01 convention (Phase 3 D-22 explicitly enumerated 4 test files)

### Integration Points
- `src/proxy/routing/matcher.rs` — single insertion point for the 3 enforcement gates (after trunk resolution, before gateway selection)
- `src/handler/api_v1/mod.rs` — router merge for two new sub-routers
- `src/models/migration.rs` — append 3 migrations (create capacity, create acl_entries, drop old acl column)
- `tests/common/mod.rs` — extend `seed_*` helpers with capacity/acl fixtures
</code_context>

<specifics>
## Specific Ideas

- Capacity exhaustion **distinguishes** `max_calls` exhaustion from CPS exhaustion in CDR (`reason: trunk_capacity_exhausted` vs `trunk_cps_exhausted`) — both 503 on the wire, but observability separates them
- 503 includes `Retry-After: 5` header — carrier-friendly hint
- Codec normalization is a standalone module so Phase 6+ routing can reuse the same lookup table
- Test fixture `seed_trunk_with_enforcement` is the singular pattern for IT-03 — minimizes test-helper drift across capacity/ACL/codec tests
- F2 confirmed: GET /capacity returns both `current_active` AND `current_cps_rate` for full backpressure observability
</specifics>

<deferred>
## Deferred Ideas

- **Hot-reload of capacity/ACL** mid-call — out of scope; changes apply at next INVITE
- **Persistent active-count across restarts** — registry is in-memory; restart resets gate. Operator-tolerable.
- **Capacity rollover/burst credit** beyond max_cps — token bucket already smooths bursts up to bucket size; further burst credit is a v2.1 concern
- **Sub-account isolation on capacity/ACL** — Phase 13 (sub-accounts) revisits
- **Codec transcoding** as a fallback (instead of 488) — out of v2.0; carrier MUST match
- **Per-gateway ACL** (vs per-trunk-group ACL) — TSUB-05 is trunk-group-scoped; gateway-scoped ACL is a roadmap candidate but not in v2.0
- **Caching of per-trunk ACL** in `match_invite_with_trace` — fresh DB read on each INVITE for v2.0; cache is v2.1 hardening
- **Carrier overload feedback** (RFC 5390 — Overload Control) — out of v2.0
</deferred>

---

*Phase: 05-trunk-enforcement-capacity-acl-codec-filter*
*Context gathered: 2026-04-25*
