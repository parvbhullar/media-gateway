# DID / Numbers Configuration — Design Spec

**Date:** 2026-04-13
**Status:** Draft for review
**Scope:** rustpbx / media-gateway

## 1. Problem

Today, DIDs (Direct Inward Dialing numbers) live on `sip_trunks.did_numbers` as an unstructured JSON blob. They are:

- **Not normalized** — `+15551234567`, `15551234567`, and `(555) 123-4567` are distinct strings.
- **Not validated** — any text is accepted at the console form.
- **Not unique** — the same number can appear on multiple trunks with no warning.
- **Not used for routing** — `src/proxy/routing/matcher.rs` matches inbound calls by regex on `from.user` / `to.user` / `request_uri`. The `did_numbers` field is never read during matching.

Result: a call can arrive on any trunk and be routed to any extension with no DID-ownership check. Configured DIDs are documentation, not policy.

## 2. Goals

1. **Correctness** — inbound routing is deterministic: a call to a known DID resolves to the owning trunk, and optionally directly to an extension.
2. **Hygiene** — DIDs are normalized to E.164 on write, unique across the system, and validated in the UI.
3. **Compatibility** — existing trunks and regex routing rules continue to work. DID resolution is additive: if a DID is known, it short-circuits; otherwise existing rules apply.

Non-goals: multi-tenant DID ownership, number-porting workflows, DID reservation / provisioning against upstream carriers.

## 3. Data Model

New table, no foreign keys (per decision — uniqueness and referential integrity handled in app code to keep migration surface small).

```sql
CREATE TABLE dids (
    number           TEXT NOT NULL PRIMARY KEY, -- normalized E.164, e.g. "+15551234567"
    trunk_name       TEXT NOT NULL,             -- owning trunk (sip_trunks.name)
    extension_number TEXT NULL,                 -- optional direct-dial target
    failover_trunk   TEXT NULL,                 -- optional redundancy trunk
    label            TEXT NULL,                 -- human-readable tag ("Main line", "Sales")
    enabled          BOOLEAN NOT NULL DEFAULT 1,
    created_at       DATETIME NOT NULL,
    updated_at       DATETIME NOT NULL
);

CREATE INDEX idx_dids_trunk_name ON dids(trunk_name);
CREATE INDEX idx_dids_extension_number ON dids(extension_number);
```

**Why no FKs:**
- Keeps the migration a single `CREATE TABLE`; no cascade semantics to reason about.
- Matches the pragmatic style of the existing codebase (routing already references trunks by name).
- Trunk / extension deletes will be handled in the delete handler: block or null-out DIDs that reference the removed entity.

**Sea-ORM entity:** `src/models/did.rs` with `Migration` struct appended to `src/models/migration.rs`.

**Deprecation of `sip_trunks.did_numbers`:** the JSON column stays for one release as a read-only fallback; a one-shot data migration (in the same `Migration`) copies existing values into the new table (normalizing, skipping duplicates, logging collisions). After the release, a follow-up migration drops the column. Not part of this spec.

## 4. Normalization

Single helper in `src/models/did.rs`:

```rust
pub fn normalize_did(raw: &str, default_region: Option<&str>) -> Result<String, DidError>
```

- Uses the already-vendored `phonenumber = "0.3.9"` crate.
- Returns canonical E.164 (`format().mode(E164)`).
- Rejects: empty, non-parseable, non-valid numbers. Error variant feeds UI validation messages.

**Default region comes from settings.** A new field `routing.default_country` (ISO 3166-1 alpha-2, e.g. `"US"`, `"IN"`) is added to the existing system settings (the `system_config` / Database Config surface shipped in commit `0a03df2`). Behaviour:

- If set → passed as the default region to `phonenumber::parse`, so operators can enter local-format numbers (e.g. `5551234567`) and they normalize to full E.164.
- If empty → `phonenumber::parse` is called with `None`, meaning input **must** already be in `+<country><national>` form or it's rejected.

The setting is read once at startup, cached in the routing/DID config snapshot, and refreshed through the same broadcast channel used for trunk/route reloads. A settings change never rewrites existing rows — normalization is applied on write only. Operators changing the default country later will see newly entered numbers normalized against the new region, while stored DIDs remain in their canonical E.164 form (which is region-independent).

All write paths (console form, API, data migration, seeding) go through this helper. No other place is allowed to insert into `dids`.

## 5. Inbound Routing

Modify `src/proxy/routing/matcher.rs` (and its call site in `src/proxy/call.rs:148`).

**Current flow:** iterate rules, regex-match against SIP fields, pick first match.

**New flow (DID-first):**

```
extract to_user from request
try normalize_did(to_user, default_region)
  -> if Ok(number) && dids[number].enabled:
       - verify inbound trunk == dids[number].trunk_name
           * strict mode: reject (503) on mismatch
           * loose mode: log warning, continue
       - if dids[number].extension_number.is_some():
           -> route directly to that extension (short-circuit)
       - else:
           -> fall through to existing rule engine, but tag the call
              with resolved DID for downstream use (CDR, policy)
  -> else: existing rule engine unchanged
```

**DID cache:** loaded at startup and refreshed on DID CRUD via the same broadcast channel that already refreshes trunks/routes. Lookup is an `Arc<HashMap<String, DidEntry>>` read — O(1), lock-free via `ArcSwap` (already used elsewhere in the project; verify during implementation).

**Strict vs loose mode:** global setting `routing.did_strict_mode` (bool, default `false` for backwards compat). Surfaced in the Settings UI alongside existing routing config.

**Failover trunk:** if the owning trunk is currently unregistered / marked down, and `failover_trunk` is set, outbound presentation uses the failover. (Inbound direction is determined by which trunk the call arrived on, so failover only affects outbound CallerID selection — covered in §7.)

## 6. Outbound CallerID

Outbound calls today pick a trunk via routing rules and present whatever CallerID the policy says. With owned DIDs we can additionally:

- When the call's `from_number` (or the extension's assigned DID) matches a row in `dids`, prefer the owning `trunk_name` for egress unless a routing rule explicitly overrides.
- If the owning trunk is down and `failover_trunk` is set, use the failover and log the substitution.

This is a small hook in the outbound trunk-selection path; exact file TBD during planning (likely `src/proxy/call.rs` outbound branch).

## 7. Console / API

**New endpoints** (`src/console/handlers/did.rs`, new file):

| Method | Path                  | Purpose                               |
|--------|-----------------------|---------------------------------------|
| GET    | `/console/dids`       | List (filter by trunk, extension, q)  |
| POST   | `/console/dids`       | Create one DID                        |
| POST   | `/console/dids/bulk`  | Create many (line-delimited, with per-row errors) |
| GET    | `/console/dids/:num`  | Fetch one                             |
| PUT    | `/console/dids/:num`  | Update label / extension / failover / enabled |
| DELETE | `/console/dids/:num`  | Remove                                |

All writes run `normalize_did` first, then insert/upsert. Uniqueness violation → 409 with the offending number.

**Form validation:** the trunk form's existing `did_numbers` textarea becomes read-only display ("managed on the Numbers page"). A new `Numbers` nav entry opens the list/edit UI. Bulk-add supports pasting a block of numbers; each line is parsed, validated, and either accepted or returned with an inline error.

**Trunk delete:** blocks with 409 if any DID still references the trunk (`trunk_name` or `failover_trunk`). User must reassign or delete the DIDs first. (Rationale: silent orphaning is worse than a one-line error.)

**Extension delete:** nulls `extension_number` on matching DIDs and logs the change. (Rationale: orphaned extension pointers are recoverable; blocking would create user friction for a common operation.)

## 8. Seeding & Migration

- **Schema migration:** new `Migration` struct in `src/models/did.rs`, registered in `src/models/migration.rs`.
- **Data backfill:** same migration walks existing `sip_trunks.did_numbers`, normalizes each entry, inserts into `dids` with `trunk_name = sip_trunks.name`, `extension_number = NULL`, `label = NULL`. Collisions (same normalized number on two trunks) are logged and the later one is skipped — a post-migration report is written to `system_notifications` so operators can resolve it.
- `config.toml` seeding (per existing pattern in the recent `Database Config` work) gains an optional `[[dids]]` block for fresh installs.

## 9. Testing

- **Unit:** `normalize_did` — valid, invalid, default-region, round-trip.
- **Unit:** matcher DID short-circuit — known DID + extension, known DID + no extension (falls through), unknown DID (falls through), strict-mode mismatch (reject).
- **Integration:** POST→GET→PUT→DELETE lifecycle; uniqueness violation; trunk-delete guard; extension-delete null-out.
- **Migration test:** seed a DB with duplicate/mixed-format `did_numbers` across trunks, run migration, assert correct normalization, single ownership, and notification entry for collisions.

## 10. Rollout

1. Ship migration + model + API + UI behind default `did_strict_mode = false`.
2. Operators review the collision report, clean up, then enable strict mode per deployment.
3. Next release: drop the legacy `sip_trunks.did_numbers` column.

## 11. Open Questions

- Should `label` be indexed for search? **Proposal:** no — small tables, `LIKE` scan is fine until >10k rows.
- Do we want per-DID inbound policies (time-of-day, IVR entry)? **Out of scope for v1** — can hang off the DID row later without schema upheaval.
