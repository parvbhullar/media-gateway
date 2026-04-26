# Phase 5: Trunk Enforcement — Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-25
**Phase:** 05-trunk-enforcement-capacity-acl-codec-filter
**Areas discussed:** Capacity storage & tracking, CPS algorithm, exhaustion response, ACL CRUD, ACL enforcement, codec rejection, test fixtures + 2 follow-ups

Discussion run as 3 batched groups. User picked Q1/Q2/Q6/Q7 explicitly; remaining areas defaulted to "all recommended".

---

## Q1: Capacity storage shape (TSUB-04)

| Option | Description | Selected |
|---|---|---|
| A | New sub-resource table `supersip_trunk_capacity` (1 row per trunk_group, FK CASCADE) — clean, follows D-00 prefix | ✓ |
| B | Two columns on `rustpbx_trunk_groups` (max_calls, max_cps) — additive | |

**User's choice:** A
**Notes:** Consistent with TSUB-01..03 sub-resource pattern; keeps trunk_group lean.

---

## Q2: Active-count tracking source (TSUB-07)

| Option | Description | Selected |
|---|---|---|
| A | `ActiveProxyCallRegistry` snapshot for read-side, in-memory atomic gate for enforcement (hybrid) | ✓ |
| B | In-memory atomic counter only (DashMap<trunk_id, AtomicU32>) | |
| C | DB session-counter row | |

**User's choice:** A (hybrid: registry snapshot for GET observability + atomic gate at dispatch entry for enforcement).
**Notes:** Read-side reuses existing state; enforcement adds a small new state piece.

---

## Q3: ACL CRUD shape (TSUB-05)

| Option | Description | Selected |
|---|---|---|
| A | Multi-row sub-resource table `supersip_trunk_acl_entries` (POST/DELETE per entry) | ✓ |
| B | JSON column edits — PUT replaces full array, reuses existing `trunk_group.acl: Option<Json>` | |

**User's choice:** A (default to recommended)
**Notes:** Mirrors Phase 3 credentials/origination_uris promotion; drops the JSON column same way.

---

## Q4: ACL enforcement integration (TSUB-05)

| Option | Description | Selected |
|---|---|---|
| A | Extend existing global ACL handler — sequential check after trunk identification | ✓ |
| B | New `enforce_trunk_acl` step before dispatch — separate concern from global firewall | |

**User's choice:** A (default to recommended)
**Notes:** Single allow/deny decision point; reuses CIDR parser.

---

## Q5: Codec mismatch rejection point (TSUB-06)

| Option | Description | Selected |
|---|---|---|
| A | Pre-dispatch SDP parse + early 488 in matcher — fail fast, never reaches gateway | ✓ |
| B | In offer/answer SDP processing during call setup | |
| C | Both — defense in depth | |

**User's choice:** A (default to recommended)
**Notes:** Codec list is on trunk; once trunk resolved, reject before dispatch.

---

## Q6: Capacity exhaustion SIP response (TSUB-04)

| Option | Description | Selected |
|---|---|---|
| A | 503 Service Unavailable with `Retry-After` — carrier convention | ✓ |
| B | 486 Busy Here — call-state semantic | |
| C | 480 Temporarily Unavailable — vague | |

**User's choice:** A
**Notes:** Matches Twilio/Telnyx behavior; Retry-After: 5.

---

## Q7: CPS rate-limiting algorithm (TSUB-04)

| Option | Description | Selected |
|---|---|---|
| A | Token bucket in-memory per trunk — smoothest, handles bursts | ✓ |
| B | Fixed 1-second window counter — simpler, can spike at boundary | |
| C | Sliding window — most accurate, more memory | |

**User's choice:** A
**Notes:** Capacity = max_cps, refill rate = max_cps tokens/sec.

---

## Q8: Test fixture strategy (IT-03)

| Option | Description | Selected |
|---|---|---|
| A | Existing-pattern mock — simulated INVITE through `match_invite_with_trace`, assert `MatchOutcome::Reject{code}` | ✓ |
| B | Real proxy startup with carrier loopback | |

**User's choice:** A (default to recommended)
**Notes:** Mirrors Phase 4 `seed_active_call` and Phase 3 routing-resolve test pattern.

---

## Follow-ups

### F1: Codec normalization location
- **Selected:** Standalone `src/proxy/routing/codec_normalize.rs` helper
- **Notes:** Better testability; Phase 6+ routing can reuse the same lookup table.

### F2: Capacity observability shape
- **Selected:** GET /capacity returns `{max_calls, max_cps, current_active, current_cps_rate}`
- **Notes:** Both observable backpressure metrics for clients.

---

## Claude's Discretion

- `governor` crate vs hand-rolled token bucket — research phase decides
- `current_cps_rate` source (token bucket direct read vs rolling 1s window)
- Permit lifecycle (RAII vs explicit release)
- ACL keyword case sensitivity
- CDR rejection-reason field name consistency
- Codec normalization lookup table caching
- Race condition test approach for capacity gate

## Deferred Ideas

- Hot-reload of capacity/ACL mid-call
- Persistent active-count across restarts
- Capacity burst credit beyond max_cps
- Sub-account isolation on enforcement (Phase 13)
- Codec transcoding fallback
- Per-gateway ACL (vs per-trunk-group)
- ACL caching in matcher
- Carrier overload feedback (RFC 5390)
