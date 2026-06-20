use sea_orm::Set;
use sea_orm::entity::prelude::*;
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{
    boolean, integer, integer_null, json_null, string, string_null, text_null, timestamp,
    timestamp_null,
};
use sea_orm_migration::sea_query::{ColumnDef, ForeignKeyAction as MigrationForeignKeyAction};
use sea_query::Expr;
use serde::{Deserialize, Serialize};

use crate::callrecord::{CallRecord, CallRecordHook};

// CallRecordPersistArgs removed

pub struct DatabaseHook {
    pub db: DatabaseConnection,
}

#[async_trait::async_trait]
impl CallRecordHook for DatabaseHook {
    async fn on_record_completed(&self, record: &mut CallRecord) -> anyhow::Result<()> {
        persist_call_record(&self.db, record).await
    }
}

pub async fn persist_call_record(
    db: &DatabaseConnection,
    record: &mut CallRecord,
) -> anyhow::Result<()> {
    let details = &record.details;

    let direction = details.direction.trim().to_ascii_lowercase();
    let status = details.status.trim().to_ascii_lowercase();
    let from_number = details.from_number.clone();
    let to_number = details.to_number.clone();
    let caller_name = details.caller_name.clone();
    let agent_name = details.agent_name.clone();
    let queue = details.queue.clone();
    let department_id = details.department_id;
    let extension_id = details.extension_id;
    let sip_trunk_id = details.sip_trunk_id;
    let route_id = details.route_id;
    let sip_gateway = details.sip_gateway.clone();

    // Derived from trace info if not explicit (but here we just use what's in details.rewrite)
    // Note: older PersistArgs had these as Option<String>, but Rewrite struct has them as String.
    // If they are empty strings, we might want to store None or Some("")?
    // Let's assume "" is valid or check if empty.
    let rewrite_original_from = if !details.rewrite.caller_original.is_empty() {
        Some(details.rewrite.caller_original.clone())
    } else {
        None
    };
    let rewrite_original_to = if !details.rewrite.callee_original.is_empty() {
        Some(details.rewrite.callee_original.clone())
    } else {
        None
    };

    let recording_url = details
        .recording_url
        .clone()
        .or_else(|| record.recorder.first().map(|media| media.path.clone()));
    let recording_duration_secs = details.recording_duration_secs;
    let has_transcript = details.has_transcript;
    let transcript_status = details.transcript_status.clone();
    let transcript_language = details.transcript_language.clone();
    let tags = details.tags.clone();
    let duration_secs = (record.end_time - record.start_time).num_seconds().max(0) as i32;
    // task 2.3: billable (answered) seconds — NULL when the call never answered.
    let billable_duration_secs = record
        .answer_time
        .map(|ans| (record.end_time - ans).num_seconds().max(0) as i32);

    let caller_uri = normalize_endpoint_uri(&record.caller);
    let callee_uri = normalize_endpoint_uri(&record.callee);

    let transcript_status_str = transcript_status
        .clone()
        .unwrap_or_else(|| "none".to_string());

    let leg_timeline_json = if record.leg_timeline.is_empty() {
        None
    } else {
        serde_json::to_value(&record.leg_timeline).ok()
    };

    // Build the metadata JSON object: start from the string map in
    // `details.metadata`, then fold in `sip_leg_roles` and `hangup_messages`
    // under reserved keys (CDR-07 / CDR-02b). `From<Model>` splits these back
    // out, so the reserved keys must not collide with user metadata keys.
    let metadata_json = {
        let mut map = details
            .metadata
            .as_ref()
            .and_then(|m| serde_json::to_value(m).ok())
            .and_then(|v| match v {
                serde_json::Value::Object(o) => Some(o),
                _ => None,
            })
            .unwrap_or_default();
        if !record.sip_leg_roles.is_empty()
            && let Ok(v) = serde_json::to_value(&record.sip_leg_roles)
        {
            map.insert("sip_leg_roles".to_string(), v);
        }
        if !record.hangup_messages.is_empty()
            && let Ok(v) = serde_json::to_value(&record.hangup_messages)
        {
            map.insert("hangup_messages".to_string(), v);
        }
        if map.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(map))
        }
    };

    let active = ActiveModel {
        call_id: Set(record.call_id.clone()),
        display_id: Set(None),
        direction: Set(direction.clone()),
        status: Set(status.clone()),
        // status_code 0 means "no SIP code captured" → store NULL.
        status_code: Set((record.status_code != 0).then_some(record.status_code as i16)),
        hangup_reason: Set(record.hangup_reason.as_ref().map(|r| r.as_db_str())),
        started_at: Set(record.start_time),
        ended_at: Set(Some(record.end_time)),
        answer_time: Set(record.answer_time),
        duration_secs: Set(duration_secs),
        billable_duration_secs: Set(billable_duration_secs),
        from_number: Set(from_number.clone()),
        to_number: Set(to_number.clone()),
        caller_name: Set(caller_name.clone()),
        agent_name: Set(agent_name.clone()),
        queue: Set(queue.clone()),
        department_id: Set(department_id),
        extension_id: Set(extension_id),
        sip_trunk_id: Set(sip_trunk_id),
        route_id: Set(route_id),
        sip_gateway: Set(sip_gateway.clone()),
        rewrite_original_from: Set(rewrite_original_from),
        rewrite_original_to: Set(rewrite_original_to),
        caller_uri: Set(caller_uri.clone()),
        callee_uri: Set(callee_uri.clone()),
        recording_url: Set(recording_url.clone()),
        recording_duration_secs: Set(recording_duration_secs),
        has_transcript: Set(has_transcript),
        transcript_status: Set(transcript_status_str),
        transcript_language: Set(transcript_language.clone()),
        tags: Set(tags.clone()),
        leg_timeline: Set(leg_timeline_json),
        metadata: Set(metadata_json),
        created_at: Set(record.start_time),
        updated_at: Set(record.end_time),
        archived_at: Set(None),
        ..Default::default()
    };

    Entity::insert(active)
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(Column::CallId)
                .update_columns([
                    Column::DisplayId,
                    Column::Direction,
                    Column::Status,
                    Column::StatusCode,
                    Column::HangupReason,
                    Column::StartedAt,
                    Column::EndedAt,
                    Column::AnswerTime,
                    Column::DurationSecs,
                    Column::FromNumber,
                    Column::ToNumber,
                    Column::CallerName,
                    Column::AgentName,
                    Column::Queue,
                    Column::DepartmentId,
                    Column::ExtensionId,
                    Column::SipTrunkId,
                    Column::RouteId,
                    Column::SipGateway,
                    Column::RewriteOriginalFrom,
                    Column::RewriteOriginalTo,
                    Column::CallerUri,
                    Column::CalleeUri,
                    Column::RecordingUrl,
                    Column::RecordingDurationSecs,
                    Column::HasTranscript,
                    Column::TranscriptStatus,
                    Column::TranscriptLanguage,
                    Column::Tags,
                    Column::LegTimeline,
                    Column::Metadata,
                    Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec(db)
        .await?;

    Ok(())
}

/// Set `recording_url` on the row matching `call_id`. Used by the
/// upload-retry scheduler after a successful re-upload, so the playback
/// handler can find the file in S3 instead of looking for the (possibly
/// deleted) local copy.
pub async fn update_recording_url_by_call_id(
    db: &sea_orm::DatabaseConnection,
    call_id: &str,
    recording_url: &str,
) -> Result<u64, sea_orm::DbErr> {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    let res = Entity::update_many()
        .col_expr(Column::RecordingUrl, Expr::value(recording_url.to_string()))
        .col_expr(Column::UpdatedAt, Expr::value(chrono::Utc::now()))
        .filter(Column::CallId.eq(call_id))
        .exec(db)
        .await?;
    Ok(res.rows_affected)
}

pub fn extract_sip_username(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(uri) = rsipstack::sip::Uri::try_from(trimmed) {
        if let Some(user) = uri.user() {
            let value = user.to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }

        if trimmed.starts_with("sip:") || trimmed.starts_with("sips:") {
            let without_scheme = trimmed
                .split_once(':')
                .map(|(_, rest)| rest)
                .unwrap_or(trimmed);
            let candidate = without_scheme.split('@').next().unwrap_or_default().trim();
            if !candidate.is_empty() {
                return Some(candidate.to_string());
            }
        }
    }

    let mut candidate = trimmed.split('@').next().unwrap_or(trimmed).trim();
    if let Some(stripped) = candidate.strip_prefix("tel:") {
        candidate = stripped;
    }
    if let Some(stripped) = candidate.strip_prefix("sip:") {
        candidate = stripped;
    }
    if let Some(stripped) = candidate.strip_prefix("sips:") {
        candidate = stripped;
    }
    if candidate.is_empty() {
        None
    } else {
        Some(candidate.to_string())
    }
}

fn normalize_endpoint_uri(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "rustpbx_call_records")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    #[sea_orm(unique)]
    pub call_id: String,
    pub display_id: Option<String>,
    pub direction: String,
    pub status: String,
    /// Final SIP response code for the call (e.g. 200, 486, 503). Nullable so
    /// rows written before this column existed deserialize cleanly.
    pub status_code: Option<i16>,
    /// Stable hangup-reason token (e.g. "by_caller", "no_answer"). See
    /// `CallRecordHangupReason::as_db_str`.
    pub hangup_reason: Option<String>,
    pub started_at: DateTimeUtc,
    pub ended_at: Option<DateTimeUtc>,
    /// When the call was answered (200 OK). NULL for unanswered calls.
    /// Billable duration = ended_at - answer_time; PDD = answer_time - started_at.
    pub answer_time: Option<DateTimeUtc>,
    pub duration_secs: i32,
    /// Answered (billable) seconds: `ended_at − answer_time`, clamped ≥ 0; NULL
    /// for unanswered calls (task 2.3). `duration_secs` remains wall-clock
    /// (`ended_at − started_at`), so billing reads this column, not that one.
    pub billable_duration_secs: Option<i32>,
    pub from_number: Option<String>,
    pub to_number: Option<String>,
    pub caller_name: Option<String>,
    pub agent_name: Option<String>,
    pub queue: Option<String>,
    pub department_id: Option<i64>,
    pub extension_id: Option<i64>,
    pub sip_trunk_id: Option<i64>,
    pub route_id: Option<i64>,
    pub sip_gateway: Option<String>,
    pub rewrite_original_from: Option<String>,
    pub rewrite_original_to: Option<String>,
    pub caller_uri: Option<String>,
    pub callee_uri: Option<String>,
    pub recording_url: Option<String>,
    pub recording_duration_secs: Option<i32>,
    pub has_transcript: bool,
    pub transcript_status: String,
    pub transcript_language: Option<String>,
    pub tags: Option<Json>,
    pub leg_timeline: Option<Json>,
    pub metadata: Option<Json>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub archived_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::department::Entity",
        from = "Column::DepartmentId",
        to = "super::department::Column::Id",
        on_delete = "SetNull",
        on_update = "Cascade"
    )]
    Department,
    #[sea_orm(
        belongs_to = "super::extension::Entity",
        from = "Column::ExtensionId",
        to = "super::extension::Column::Id",
        on_delete = "SetNull",
        on_update = "Cascade"
    )]
    Extension,
    #[sea_orm(
        belongs_to = "super::sip_trunk::Entity",
        from = "Column::SipTrunkId",
        to = "super::sip_trunk::Column::Id",
        on_delete = "SetNull",
        on_update = "Cascade"
    )]
    SipTrunk,
    #[sea_orm(
        belongs_to = "super::routing::Entity",
        from = "Column::RouteId",
        to = "super::routing::Column::Id",
        on_delete = "SetNull",
        on_update = "Cascade"
    )]
    Route,
}

impl ActiveModelBehavior for ActiveModel {}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Entity)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Column::Id)
                            .big_integer()
                            .primary_key()
                            .auto_increment(),
                    )
                    .col(string(Column::CallId).char_len(120))
                    .col(string_null(Column::DisplayId).char_len(120))
                    .col(string(Column::Direction).char_len(16))
                    .col(string(Column::Status).char_len(32))
                    .col(timestamp(Column::StartedAt).default(Expr::current_timestamp()))
                    .col(timestamp_null(Column::EndedAt))
                    .col(integer(Column::DurationSecs).not_null().default(0))
                    .col(string_null(Column::FromNumber).char_len(64))
                    .col(string_null(Column::ToNumber).char_len(64))
                    .col(string_null(Column::CallerName).char_len(160))
                    .col(string_null(Column::AgentName).char_len(160))
                    .col(string_null(Column::Queue).char_len(120))
                    .col(ColumnDef::new(Column::DepartmentId).big_integer().null())
                    .col(ColumnDef::new(Column::ExtensionId).big_integer().null())
                    .col(ColumnDef::new(Column::SipTrunkId).big_integer().null())
                    .col(ColumnDef::new(Column::RouteId).big_integer().null())
                    .col(string_null(Column::SipGateway).char_len(160))
                    .col(string_null(Column::RewriteOriginalFrom).char_len(64))
                    .col(string_null(Column::RewriteOriginalTo).char_len(64))
                    .col(text_null(Column::CallerUri))
                    .col(text_null(Column::CalleeUri))
                    .col(string_null(Column::RecordingUrl).char_len(255))
                    .col(integer_null(Column::RecordingDurationSecs))
                    .col(boolean(Column::HasTranscript).default(false))
                    .col(
                        string(Column::TranscriptStatus)
                            .char_len(32)
                            .default("pending"),
                    )
                    .col(string_null(Column::TranscriptLanguage).char_len(16))
                    .col(json_null(Column::Tags))
                    .col(json_null(Column::LegTimeline))
                    .col(json_null(Column::Metadata))
                    .col(timestamp(Column::CreatedAt).default(Expr::current_timestamp()))
                    .col(timestamp(Column::UpdatedAt).default(Expr::current_timestamp()))
                    .col(timestamp_null(Column::ArchivedAt))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_call_records_department")
                            .from(Entity, Column::DepartmentId)
                            .to(super::department::Entity, super::department::Column::Id)
                            .on_delete(MigrationForeignKeyAction::SetNull)
                            .on_update(MigrationForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_call_records_extension")
                            .from(Entity, Column::ExtensionId)
                            .to(super::extension::Entity, super::extension::Column::Id)
                            .on_delete(MigrationForeignKeyAction::SetNull)
                            .on_update(MigrationForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_call_records_sip_trunk")
                            .from(Entity, Column::SipTrunkId)
                            .to(super::sip_trunk::Entity, super::sip_trunk::Column::Id)
                            .on_delete(MigrationForeignKeyAction::SetNull)
                            .on_update(MigrationForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_call_records_route")
                            .from(Entity, Column::RouteId)
                            .to(super::routing::Entity, super::routing::Column::Id)
                            .on_delete(MigrationForeignKeyAction::SetNull)
                            .on_update(MigrationForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_rustpbx_call_records_call_id")
                    .table(Entity)
                    .col(Column::CallId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_call_records_started_at")
                    .table(Entity)
                    .col(Column::StartedAt)
                    .col(Column::Direction)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_call_records_status")
                    .table(Entity)
                    .col(Column::Status)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_rustpbx_call_records_extension")
                    .table(Entity)
                    .col(Column::ExtensionId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Entity).to_owned())
            .await
    }
}

impl From<Model> for CallRecord {
    fn from(val: Model) -> Self {
        // Split the metadata JSON: reserved keys (CDR-07 / CDR-02b) become typed
        // fields; remaining string-valued keys go back into details.metadata.
        // This must not panic when nested (non-string) reserved values are present.
        let (details_metadata, sip_leg_roles, hangup_messages) = match val.metadata {
            Some(serde_json::Value::Object(mut map)) => {
                let sip_leg_roles = map
                    .remove("sip_leg_roles")
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                let hangup_messages = map
                    .remove("hangup_messages")
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                let string_map: std::collections::HashMap<String, String> = map
                    .into_iter()
                    .filter_map(|(k, v)| match v {
                        serde_json::Value::String(s) => Some((k, s)),
                        _ => None,
                    })
                    .collect();
                let details_metadata = (!string_map.is_empty()).then_some(string_map);
                (details_metadata, sip_leg_roles, hangup_messages)
            }
            _ => (None, std::collections::HashMap::new(), Vec::new()),
        };

        let details = crate::callrecord::CallDetails {
            direction: val.direction,
            status: val.status,
            from_number: val.from_number.clone(),
            to_number: val.to_number.clone(),
            caller_name: val.caller_name,
            agent_name: val.agent_name,
            queue: val.queue,
            department_id: val.department_id,
            extension_id: val.extension_id,
            sip_trunk_id: val.sip_trunk_id,
            route_id: val.route_id,
            sip_gateway: val.sip_gateway,
            recording_url: val.recording_url,
            recording_duration_secs: val.recording_duration_secs,
            has_transcript: val.has_transcript,
            transcript_status: Some(val.transcript_status),
            transcript_language: val.transcript_language,
            tags: val.tags,
            metadata: details_metadata,
            rewrite: crate::callrecord::CallRecordRewrite {
                caller_original: val.rewrite_original_from.unwrap_or_default(),
                caller_final: String::new(),
                callee_original: val.rewrite_original_to.unwrap_or_default(),
                callee_final: String::new(),
                contact: None,
                destination: None,
            },
            last_error: None,
        };

        let leg_timeline = val
            .leg_timeline
            .and_then(|json| serde_json::from_value(json).ok())
            .unwrap_or_default();

        CallRecord {
            call_id: val.call_id,
            start_time: val.started_at,
            ring_time: None, // ring_time not persisted (out of Phase-1 scope)
            answer_time: val.answer_time,
            end_time: val.ended_at.unwrap_or(val.started_at),
            caller: val
                .caller_uri
                .unwrap_or_else(|| val.from_number.unwrap_or_default()),
            callee: val
                .callee_uri
                .unwrap_or_else(|| val.to_number.unwrap_or_default()),
            status_code: val.status_code.map(|c| c as u16).unwrap_or(0),
            hangup_reason: val
                .hangup_reason
                .as_deref()
                .map(crate::callrecord::CallRecordHangupReason::from_db_str),
            hangup_messages,
            recorder: Vec::new(), // recorder list not persisted (single stereo file)
            sip_leg_roles,
            leg_timeline,
            details,
            extensions: http::Extensions::new(),
        }
    }
}

#[cfg(test)]
mod cdr_phase1_tests {
    use super::*;
    use crate::callrecord::{CallRecord, CallRecordHangupReason};

    fn sample_model() -> Model {
        let now = chrono::Utc::now();
        Model {
            id: 1,
            call_id: "call-1".to_string(),
            display_id: None,
            direction: "inbound".to_string(),
            status: "completed".to_string(),
            status_code: Some(200),
            hangup_reason: Some("by_callee".to_string()),
            started_at: now,
            ended_at: Some(now),
            answer_time: Some(now),
            duration_secs: 10,
            billable_duration_secs: Some(10),
            from_number: Some("1001".to_string()),
            to_number: Some("2002".to_string()),
            caller_name: None,
            agent_name: None,
            queue: None,
            department_id: None,
            extension_id: None,
            sip_trunk_id: None,
            route_id: None,
            sip_gateway: None,
            rewrite_original_from: None,
            rewrite_original_to: None,
            caller_uri: Some("sip:1001@host".to_string()),
            callee_uri: Some("sip:2002@host".to_string()),
            recording_url: None,
            recording_duration_secs: None,
            has_transcript: false,
            transcript_status: "none".to_string(),
            transcript_language: None,
            tags: None,
            leg_timeline: None,
            metadata: None,
            created_at: now,
            updated_at: now,
            archived_at: None,
        }
    }

    #[test]
    fn hangup_reason_db_str_roundtrips() {
        for r in [
            CallRecordHangupReason::ByCaller,
            CallRecordHangupReason::ByCallee,
            CallRecordHangupReason::NoAnswer,
            CallRecordHangupReason::Rejected,
            CallRecordHangupReason::Failed,
            CallRecordHangupReason::ServerUnavailable,
        ] {
            assert_eq!(CallRecordHangupReason::from_db_str(&r.as_db_str()), r);
        }
        let other = CallRecordHangupReason::Other("custom".to_string());
        assert_eq!(CallRecordHangupReason::from_db_str(&other.as_db_str()), other);
    }

    #[test]
    fn from_model_maps_new_columns() {
        let rec: CallRecord = sample_model().into();
        assert_eq!(rec.status_code, 200);
        assert_eq!(rec.hangup_reason, Some(CallRecordHangupReason::ByCallee));
        assert!(rec.answer_time.is_some());
    }

    #[test]
    fn from_model_null_status_code_becomes_zero_and_answer_none() {
        let mut m = sample_model();
        m.status_code = None;
        m.answer_time = None;
        let rec: CallRecord = m.into();
        assert_eq!(rec.status_code, 0);
        assert_eq!(rec.answer_time, None);
    }

    #[test]
    fn metadata_reserved_keys_split_out_and_strings_kept() {
        let mut m = sample_model();
        m.metadata = Some(serde_json::json!({
            "tenant": "acme",
            "sip_leg_roles": { "call-1": "caller" },
            "hangup_messages": [ { "code": 503, "reason": "busy" } ],
        }));
        let rec: CallRecord = m.into();
        assert_eq!(
            rec.sip_leg_roles.get("call-1").map(|s| s.as_str()),
            Some("caller")
        );
        assert_eq!(rec.hangup_messages.len(), 1);
        assert_eq!(rec.hangup_messages[0].code, 503);
        let md = rec.details.metadata.expect("string metadata preserved");
        assert_eq!(md.get("tenant").map(|s| s.as_str()), Some("acme"));
        assert!(!md.contains_key("sip_leg_roles"));
        assert!(!md.contains_key("hangup_messages"));
    }
}
