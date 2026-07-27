//! Phase 6 Plan 06-04 — orchestrator that consults `supersip_routing_tables`
//! per INVITE (D-29 fresh DB read), filters by direction (D-21), sorts by
//! priority ASC (D-22), evaluates records in position order (D-23), falls
//! back to `is_default: true` records (D-19), and chains `next_table`
//! targets up to depth 3 with visited-set loop detection (D-25).
//!
//! Used by:
//!   - `match_invite_with_trace` (production matcher entry; D-06)
//!   - `/api/v1/routing/resolve` handler (dry-run; D-30) — same function
//!     so the dry-run cannot drift from production dispatch.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use anyhow::Result;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use serde_json::json;

use crate::call::DialDirection;
use crate::models::routing_tables::{
    Column as TableColumn, Entity as TableEntity, Model as TableModel,
};
use crate::proxy::routing::match_types::{
    CompareValue, HttpQueryBody, MatchOutcome, RegexCache, RoutingMatch, RoutingRecord,
    RoutingTarget, eval_compare, eval_exact, eval_http_query, eval_regex,
    lpm_match_length,
};

/// Hard cap on `next_table` chain depth (D-25, T-06-04-05).
const MAX_NEXT_TABLE_DEPTH: usize = 3;

/// Information about a matched record, returned to the matcher and to the
/// `/resolve` handler for D-30 wiring.
#[derive(Debug, Clone)]
pub struct MatchedRecordInfo {
    pub table_name: String,
    pub record_id: String,
    pub position: i32,
    pub target: RoutingTarget,
    pub used_default: bool,
    /// Trace events captured during evaluation (D-31). Each entry is a
    /// JSON object with shape `{event: "<name>", ...fields}`.
    pub events: Vec<serde_json::Value>,
}

/// Special outcome surfaced when chain depth or loop is detected.
#[derive(Debug)]
pub enum TableMatchError {
    LoopDetected(String),
    DepthExceeded,
    MissingTarget(String),
}

impl std::fmt::Display for TableMatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoopDetected(name) => write!(f, "routing_loop_detected:{}", name),
            Self::DepthExceeded => write!(f, "routing_chain_depth_exceeded"),
            Self::MissingTarget(name) => write!(f, "routing_missing_target:{}", name),
        }
    }
}

impl std::error::Error for TableMatchError {}

/// Process-wide reqwest client (T-06-04-12 — single shared connection pool).
fn shared_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("failed to build shared reqwest client for routing HttpQuery")
    })
}

/// Direction filter helper — matches D-21.
fn direction_allows(table_dir: &str, direction: &DialDirection) -> bool {
    match (table_dir, direction) {
        ("both", _) => true,
        ("inbound", DialDirection::Inbound) => true,
        ("outbound", DialDirection::Outbound) => true,
        ("inbound", _) | ("outbound", _) => false,
        // Unknown direction string in DB — be conservative, skip.
        _ => false,
    }
}

/// Top-level entry. Loads candidate tables fresh from DB per call (D-29),
/// honors direction filter and priority ordering, recursively resolves
/// `next_table` chains, and returns the first match (or None when nothing
/// matches and no default is set).
pub async fn match_against_supersip_tables(
    db: &DatabaseConnection,
    direction: &DialDirection,
    caller_number: &str,
    destination_number: &str,
    src_ip: Option<&str>,
    headers: &HashMap<String, String>,
) -> Result<Option<Result<MatchedRecordInfo, TableMatchError>>> {
    // Fetch all tables (no DB-side direction filter — sqlite test envs
    // don't have an enum); filter in memory.
    let tables: Vec<TableModel> = TableEntity::find()
        .filter(TableColumn::IsActive.eq(true))
        .order_by_asc(TableColumn::Priority)
        .all(db)
        .await?;

    let candidate: Vec<TableModel> = tables
        .into_iter()
        .filter(|t| direction_allows(&t.direction, direction))
        .collect();

    if candidate.is_empty() {
        return Ok(None);
    }

    let mut cache = RegexCache::new();
    let mut events: Vec<serde_json::Value> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();

    for table in &candidate {
        match evaluate_one_table(
            db,
            table,
            direction,
            caller_number,
            destination_number,
            src_ip,
            headers,
            &mut cache,
            &mut events,
            &mut visited,
            0,
        )
        .await?
        {
            TableEval::Hit(info) => return Ok(Some(Ok(info))),
            TableEval::DefaultHit(info) => return Ok(Some(Ok(info))),
            TableEval::Error(e) => return Ok(Some(Err(e))),
            TableEval::NoMatch => continue,
        }
    }

    Ok(None)
}

enum TableEval {
    Hit(MatchedRecordInfo),
    DefaultHit(MatchedRecordInfo),
    NoMatch,
    Error(TableMatchError),
}

#[allow(clippy::too_many_arguments)]
async fn evaluate_one_table(
    db: &DatabaseConnection,
    table: &TableModel,
    direction: &DialDirection,
    caller_number: &str,
    destination_number: &str,
    src_ip: Option<&str>,
    headers: &HashMap<String, String>,
    cache: &mut RegexCache,
    events: &mut Vec<serde_json::Value>,
    visited: &mut HashSet<String>,
    depth: usize,
) -> Result<TableEval> {
    if depth >= MAX_NEXT_TABLE_DEPTH {
        events.push(json!({"event": "ChainDepthExceeded", "table": table.name}));
        return Ok(TableEval::Error(TableMatchError::DepthExceeded));
    }
    if !visited.insert(table.name.clone()) {
        events.push(json!({"event": "RoutingLoopDetected", "table": table.name}));
        return Ok(TableEval::Error(TableMatchError::LoopDetected(
            table.name.clone(),
        )));
    }

    // Parse records.
    let records: Vec<RoutingRecord> = match serde_json::from_value::<Vec<RoutingRecord>>(
        table.records.clone(),
    ) {
        Ok(v) => v,
        Err(_) => Vec::new(),
    };

    // Filter active, non-default records into evaluation pool; default
    // record kept aside (D-19, D-20).
    let mut active: Vec<RoutingRecord> =
        records.iter().filter(|r| r.is_active).cloned().collect();
    active.sort_by_key(|r| r.position);

    // ── Pass 1: Lpm cross-record longest-wins (D-23) ──
    let mut best_lpm: Option<(usize, RoutingRecord)> = None;
    for rec in active.iter() {
        if rec.is_default {
            continue;
        }
        if let RoutingMatch::Lpm { prefix } = &rec.match_ {
            if let Some(len) = lpm_match_length(prefix, destination_number) {
                let take = match &best_lpm {
                    Some((blen, _)) => len > *blen,
                    None => true,
                };
                if take {
                    best_lpm = Some((len, rec.clone()));
                }
            }
        }
    }

    if let Some((_len, rec)) = best_lpm {
        if let RoutingMatch::Lpm { prefix } = &rec.match_ {
            events.push(json!({
                "event": "LpmMatch",
                "prefix": prefix,
                "table": table.name,
            }));
        }
        return resolve_record_target(
            db,
            table,
            &rec,
            direction,
            caller_number,
            destination_number,
            src_ip,
            headers,
            cache,
            events,
            visited,
            depth,
            false,
        )
        .await;
    }

    // ── Pass 2: non-Lpm records in position order (D-23) ──
    for rec in active.iter() {
        if rec.is_default {
            continue;
        }
        let outcome = match &rec.match_ {
            RoutingMatch::Lpm { .. } => continue,
            RoutingMatch::ExactMatch { value } => {
                let o = eval_exact(value, destination_number);
                if matches!(o, MatchOutcome::Hit { .. }) {
                    events.push(json!({
                        "event": "ExactMatch",
                        "value": value,
                        "table": table.name,
                    }));
                }
                o
            }
            RoutingMatch::Regex { pattern } => {
                let o = eval_regex(pattern, destination_number, cache);
                if matches!(o, MatchOutcome::Hit { .. }) {
                    events.push(json!({
                        "event": "RegexMatch",
                        "pattern": pattern,
                        "table": table.name,
                    }));
                }
                o
            }
            RoutingMatch::Compare { op, value } => {
                let o = eval_compare(op, value, destination_number);
                if matches!(o, MatchOutcome::Hit { .. }) {
                    let value_repr = match value {
                        CompareValue::Single(n) => json!(n),
                        CompareValue::Range([lo, hi]) => json!([lo, hi]),
                    };
                    events.push(json!({
                        "event": "CompareMatch",
                        "op": format!("{:?}", op).to_lowercase(),
                        "value": value_repr,
                        "table": table.name,
                    }));
                }
                o
            }
            RoutingMatch::HttpQuery {
                url,
                timeout_ms,
                headers: rec_headers,
            } => {
                let body = HttpQueryBody {
                    caller_number: caller_number.to_string(),
                    destination_number: destination_number.to_string(),
                    src_ip: src_ip.map(|s| s.to_string()),
                    headers: headers.clone(),
                };
                let res = eval_http_query(
                    shared_http_client(),
                    url,
                    *timeout_ms,
                    rec_headers.as_ref(),
                    &body,
                )
                .await;
                match (&res.outcome, &res.failure_reason) {
                    (MatchOutcome::Hit { .. }, _) => {
                        events.push(json!({
                            "event": "HttpQueryMatch",
                            "url": url,
                            "latency_ms": res.latency_ms,
                            "table": table.name,
                        }));
                        // For HttpQuery, the operator's response can override
                        // the record's static target. If they returned one,
                        // build a synthetic record carrying that target.
                        if let Some(t) = res.target {
                            let mut synthetic = rec.clone();
                            synthetic.target = t;
                            return resolve_record_target(
                                db,
                                table,
                                &synthetic,
                                direction,
                                caller_number,
                                destination_number,
                                src_ip,
                                headers,
                                cache,
                                events,
                                visited,
                                depth,
                                false,
                            )
                            .await;
                        }
                    }
                    (MatchOutcome::Miss, Some(reason)) => {
                        events.push(json!({
                            "event": "HttpQueryFailed",
                            "url": url,
                            "error": reason,
                            "table": table.name,
                        }));
                    }
                    _ => {}
                }
                res.outcome
            }
        };

        if matches!(outcome, MatchOutcome::Hit { .. }) {
            return resolve_record_target(
                db,
                table,
                rec,
                direction,
                caller_number,
                destination_number,
                src_ip,
                headers,
                cache,
                events,
                visited,
                depth,
                false,
            )
            .await;
        }
    }

    // ── Pass 3: default record (D-19) ──
    if let Some(default_rec) = records.iter().find(|r| r.is_default && r.is_active) {
        events.push(json!({
            "event": "DefaultRecordUsed",
            "table": table.name,
        }));
        return resolve_record_target(
            db,
            table,
            default_rec,
            direction,
            caller_number,
            destination_number,
            src_ip,
            headers,
            cache,
            events,
            visited,
            depth,
            true,
        )
        .await;
    }

    events.push(json!({"event": "NoMatch", "table": table.name}));
    Ok(TableEval::NoMatch)
}

#[allow(clippy::too_many_arguments)]
async fn resolve_record_target(
    db: &DatabaseConnection,
    table: &TableModel,
    rec: &RoutingRecord,
    direction: &DialDirection,
    caller_number: &str,
    destination_number: &str,
    src_ip: Option<&str>,
    headers: &HashMap<String, String>,
    cache: &mut RegexCache,
    events: &mut Vec<serde_json::Value>,
    visited: &mut HashSet<String>,
    depth: usize,
    used_default: bool,
) -> Result<TableEval> {
    match &rec.target {
        RoutingTarget::NextTable { name } => {
            // Recurse into named table.
            let next: Option<TableModel> = TableEntity::find()
                .filter(TableColumn::Name.eq(name.clone()))
                .filter(TableColumn::IsActive.eq(true))
                .one(db)
                .await?;
            let Some(next_table) = next else {
                events.push(json!({
                    "event": "NextTableMissing",
                    "name": name,
                }));
                return Ok(TableEval::Error(TableMatchError::MissingTarget(
                    name.clone(),
                )));
            };
            // Honor direction filter on chained table too.
            if !direction_allows(&next_table.direction, direction) {
                events.push(json!({
                    "event": "NextTableDirectionFiltered",
                    "name": name,
                }));
                return Ok(TableEval::NoMatch);
            }
            // Recurse with depth + 1.
            Box::pin(evaluate_one_table(
                db,
                &next_table,
                direction,
                caller_number,
                destination_number,
                src_ip,
                headers,
                cache,
                events,
                visited,
                depth + 1,
            ))
            .await
        }
        _ => {
            let info = MatchedRecordInfo {
                table_name: table.name.clone(),
                record_id: rec.record_id.clone(),
                position: rec.position,
                target: rec.target.clone(),
                used_default,
                events: events.clone(),
            };
            if used_default {
                Ok(TableEval::DefaultHit(info))
            } else {
                Ok(TableEval::Hit(info))
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, Database, Set};
    use sea_orm_migration::MigratorTrait;

    use crate::models::migration::Migrator;
    use crate::models::routing_tables;

    async fn fresh_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.expect("connect");
        Migrator::up(&db, None).await.expect("migrate");
        db
    }

    fn rec(id: &str, pos: i32, m: RoutingMatch, t: RoutingTarget) -> RoutingRecord {
        RoutingRecord {
            record_id: id.to_string(),
            position: pos,
            match_: m,
            target: t,
            is_default: false,
            is_active: true,
        }
    }

    async fn seed(
        db: &DatabaseConnection,
        name: &str,
        direction: &str,
        priority: i32,
        records: Vec<RoutingRecord>,
    ) {
        let now = Utc::now();
        let am = routing_tables::ActiveModel {
            name: Set(name.to_string()),
            description: Set(None),
            direction: Set(direction.to_string()),
            priority: Set(priority),
            is_active: Set(true),
            records: Set(serde_json::to_value(&records).unwrap()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        am.insert(db).await.expect("insert table");
    }

    fn empty_headers() -> HashMap<String, String> {
        HashMap::new()
    }

    #[tokio::test]
    async fn inbound_table_skipped_for_outbound_direction() {
        let db = fresh_db().await;
        seed(
            &db,
            "in-only",
            "inbound",
            10,
            vec![rec(
                "r1",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::TrunkGroup {
                    name: "us".into(),
                },
            )],
        )
        .await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1555",
            "+1999",
            None,
            &empty_headers(),
        )
        .await
        .unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn tables_sorted_by_priority_asc_first_match_wins() {
        let db = fresh_db().await;
        seed(
            &db,
            "low-priority",
            "both",
            100,
            vec![rec(
                "r1",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::TrunkGroup {
                    name: "us-low".into(),
                },
            )],
        )
        .await;
        seed(
            &db,
            "high-priority",
            "both",
            10,
            vec![rec(
                "r2",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::TrunkGroup {
                    name: "us-high".into(),
                },
            )],
        )
        .await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1555",
            "+1999",
            None,
            &empty_headers(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(res.table_name, "high-priority");
        match res.target {
            RoutingTarget::TrunkGroup { name } => assert_eq!(name, "us-high"),
            _ => panic!("wrong target"),
        }
    }

    #[tokio::test]
    async fn lpm_records_compete_within_table_longest_wins() {
        let db = fresh_db().await;
        seed(
            &db,
            "lpm-table",
            "both",
            10,
            vec![
                rec(
                    "r-short",
                    0,
                    RoutingMatch::Lpm {
                        prefix: "+1".into(),
                    },
                    RoutingTarget::TrunkGroup {
                        name: "short".into(),
                    },
                ),
                rec(
                    "r-long",
                    1,
                    RoutingMatch::Lpm {
                        prefix: "+1415".into(),
                    },
                    RoutingTarget::TrunkGroup {
                        name: "long".into(),
                    },
                ),
            ],
        )
        .await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1",
            "+14155551234",
            None,
            &empty_headers(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(res.record_id, "r-long");
    }

    #[tokio::test]
    async fn inactive_record_skipped() {
        let db = fresh_db().await;
        let mut r = rec(
            "r1",
            0,
            RoutingMatch::Lpm {
                prefix: "+1".into(),
            },
            RoutingTarget::TrunkGroup {
                name: "us".into(),
            },
        );
        r.is_active = false;
        seed(&db, "t", "both", 10, vec![r]).await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1",
            "+1234",
            None,
            &empty_headers(),
        )
        .await
        .unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn default_record_returned_when_no_match() {
        let db = fresh_db().await;
        let mut def = rec(
            "r-def",
            99,
            RoutingMatch::Lpm {
                prefix: "doesnt-matter".into(),
            },
            RoutingTarget::TrunkGroup {
                name: "fallback".into(),
            },
        );
        def.is_default = true;
        seed(
            &db,
            "t",
            "both",
            10,
            vec![
                rec(
                    "r1",
                    0,
                    RoutingMatch::Lpm {
                        prefix: "+44".into(),
                    },
                    RoutingTarget::TrunkGroup {
                        name: "uk".into(),
                    },
                ),
                def,
            ],
        )
        .await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1",
            "+1555",
            None,
            &empty_headers(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert!(res.used_default);
        assert_eq!(res.record_id, "r-def");
    }

    #[tokio::test]
    async fn no_match_no_default_returns_none() {
        let db = fresh_db().await;
        seed(
            &db,
            "t",
            "both",
            10,
            vec![rec(
                "r1",
                0,
                RoutingMatch::Lpm {
                    prefix: "+44".into(),
                },
                RoutingTarget::TrunkGroup {
                    name: "uk".into(),
                },
            )],
        )
        .await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1",
            "+1555",
            None,
            &empty_headers(),
        )
        .await
        .unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn next_table_chain_depth_2_resolves_target() {
        let db = fresh_db().await;
        seed(
            &db,
            "table-a",
            "both",
            10,
            vec![rec(
                "ra",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::NextTable {
                    name: "table-b".into(),
                },
            )],
        )
        .await;
        seed(
            &db,
            "table-b",
            "both",
            20,
            vec![rec(
                "rb",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::TrunkGroup {
                    name: "us".into(),
                },
            )],
        )
        .await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1",
            "+1555",
            None,
            &empty_headers(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(res.record_id, "rb");
        assert_eq!(res.table_name, "table-b");
    }

    #[tokio::test]
    async fn next_table_chain_depth_3_caps() {
        let db = fresh_db().await;
        // A -> B -> C -> D (depth 3 reaches D, but cap is 3 so D is rejected)
        seed(
            &db,
            "ta",
            "both",
            10,
            vec![rec(
                "ra",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::NextTable { name: "tb".into() },
            )],
        )
        .await;
        seed(
            &db,
            "tb",
            "both",
            20,
            vec![rec(
                "rb",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::NextTable { name: "tc".into() },
            )],
        )
        .await;
        seed(
            &db,
            "tc",
            "both",
            30,
            vec![rec(
                "rc",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::NextTable { name: "td".into() },
            )],
        )
        .await;
        seed(
            &db,
            "td",
            "both",
            40,
            vec![rec(
                "rd",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::TrunkGroup {
                    name: "us".into(),
                },
            )],
        )
        .await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1",
            "+1555",
            None,
            &empty_headers(),
        )
        .await
        .unwrap();
        // Should see DepthExceeded
        let err = res.unwrap();
        match err {
            Err(TableMatchError::DepthExceeded) => {}
            other => panic!("expected DepthExceeded, got {:?}", other.is_ok()),
        }
    }

    #[tokio::test]
    async fn next_table_chain_loop_detected() {
        let db = fresh_db().await;
        seed(
            &db,
            "ta",
            "both",
            10,
            vec![rec(
                "ra",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::NextTable { name: "tb".into() },
            )],
        )
        .await;
        seed(
            &db,
            "tb",
            "both",
            20,
            vec![rec(
                "rb",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::NextTable { name: "ta".into() },
            )],
        )
        .await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1",
            "+1555",
            None,
            &empty_headers(),
        )
        .await
        .unwrap()
        .unwrap();
        match res {
            Err(TableMatchError::LoopDetected(name)) => {
                assert!(name == "ta" || name == "tb");
            }
            other => panic!("expected LoopDetected, got {:?}", other.is_ok()),
        }
    }

    #[tokio::test]
    async fn next_table_target_missing_logs_and_aborts() {
        let db = fresh_db().await;
        seed(
            &db,
            "ta",
            "both",
            10,
            vec![rec(
                "ra",
                0,
                RoutingMatch::Lpm {
                    prefix: "+1".into(),
                },
                RoutingTarget::NextTable {
                    name: "ghost".into(),
                },
            )],
        )
        .await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1",
            "+1555",
            None,
            &empty_headers(),
        )
        .await
        .unwrap()
        .unwrap();
        match res {
            Err(TableMatchError::MissingTarget(name)) => assert_eq!(name, "ghost"),
            other => panic!("expected MissingTarget, got {:?}", other.is_ok()),
        }
    }

    #[tokio::test]
    async fn exact_match_hit() {
        let db = fresh_db().await;
        seed(
            &db,
            "t",
            "both",
            10,
            vec![rec(
                "r1",
                0,
                RoutingMatch::ExactMatch {
                    value: "8005551234".into(),
                },
                RoutingTarget::TrunkGroup {
                    name: "tf".into(),
                },
            )],
        )
        .await;
        let res = match_against_supersip_tables(
            &db,
            &DialDirection::Outbound,
            "+1",
            "8005551234",
            None,
            &empty_headers(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(res.record_id, "r1");
    }
}
