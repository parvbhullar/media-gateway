# Phase 1 Deferred Items

Items from `01-VERIFICATION.md` gaps[] that Plan 01-06 intentionally
does not close, with rationale and tracking target.

## DIAG-05 — diagnostics/summary missing flood/auth fields

**Gap:** `GET /api/v1/diagnostics/summary` does not return the
`recent_flood_events` or `recent_auth_failures` slots the locked
CONTEXT.md shape specified.

**Rationale for deferral:** Flood tracking and brute-force auth
failure tracking are Phase 10 (Security Suite) work. The shape slots
cannot be populated with meaningful data until Phase 10 lands the
trackers themselves. Stubbing them out with empty arrays now would
lock a schema that Phase 10 will likely want to change.

**Target phase:** Phase 10 — Security Suite. When Phase 10 lands the
flood and brute-force trackers, update `diagnostics/summary` to
surface their stats and update CONTEXT.md / this deferred item.

## SHELL-05 — console handler pure-fn extraction not performed

**Gap:** Plan 01-01 called for extracting `pub(crate) async fn`
helpers from `console/handlers/{did,sip_trunk,call_record,
diagnostics}.rs` so the api-v1 handlers could reuse them without
duplicating business logic.

**What shipped instead:** The executor deliberately routed reuse
through the SeaORM model layer — api-v1 handlers call the model
entities directly rather than going through the console helpers.
This is documented at `src/handler/api_v1/dids.rs:1-12`.

**Rationale for accepting the deviation:** Model-layer sharing is
arguably cleaner because it avoids coupling the api-v1 handlers to
the console handler signatures, which were shaped for HTML form
responses. Duplication is the wrong word — both layers call the
same model methods directly. The original truth statement was
based on an incorrect assumption that business logic lived in the
console handlers themselves, when in fact it lives in the model.

**Target phase:** None — this is closed as ADR-style deviation.
The truth statement in Plans 01-01 through 01-05 is semantically
satisfied even though the literal contract wasn't followed.

## MIG-03 — console render-parity not documented

**Gap:** No commit recorded a manual render-parity check after the
Phase 1 refactor of the console handlers.

**How Plan 01-06 addresses this:** The executor of Plan 01-06 SHALL
add a render-parity checklist to `01-06-SUMMARY.md` — pick one URL
per console page (sip_trunks, dids, call-records, routing,
diagnostics, settings), load it with curl or a browser, and record
HTTP 200 + non-empty body. This is a retroactive audit, not new
code.

**Target phase:** Closed in Plan 01-06's SUMMARY.

## reload/app (4th reload step) — deferred from SYS-02

**Gap:** Plan 01-06 implements 3 of 4 reload steps: trunks, routes,
acl. The 4th step (reload_app, which reloads the configuration
file and runs preflight validation) is deferred because it has
fundamentally different semantics — it takes query parameters
(`?check_only=true`, `?mode=validate`), supports a dry-run mode,
and returns a config-validation error shape that does not fit the
`ReloadStepOutcome { step, elapsed_ms, changed_count }` pattern.

**Target phase:** Phase 11 — System Polish & CDR Export. Phase 11
already owns `/system/info|config|stats|cluster` polish; extending
`/system/reload` to accept `?include_app=true` and return the
dry-run error shape fits naturally.
