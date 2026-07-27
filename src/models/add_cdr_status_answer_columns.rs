use sea_orm_migration::prelude::*;

/// Phase 1 (CDR completeness): add `status_code`, `hangup_reason`, and
/// `answer_time` to `rustpbx_call_records`. These three fields already exist
/// on the in-memory `CallRecord` but were dropped before SQL; persisting them
/// makes SIP outcome, release reason, and PDD/billable-duration queryable.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table_name = "rustpbx_call_records";

        if !manager.has_column(table_name, "status_code").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(crate::models::call_record::Entity)
                        .add_column(
                            ColumnDef::new(crate::models::call_record::Column::StatusCode)
                                .small_integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !manager.has_column(table_name, "hangup_reason").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(crate::models::call_record::Entity)
                        .add_column(
                            ColumnDef::new(crate::models::call_record::Column::HangupReason)
                                // 64, not 32: known tokens are ≤18 chars, but
                                // `Other(s)` passes arbitrary strings through
                                // `as_db_str` — headroom avoids a CDR-insert
                                // failure on an over-long custom reason.
                                .string_len(64)
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !manager.has_column(table_name, "answer_time").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(crate::models::call_record::Entity)
                        .add_column(
                            ColumnDef::new(crate::models::call_record::Column::AnswerTime)
                                .timestamp()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // No-op down, matching the convention of the other add_*_column
        // migrations in this codebase (additive, never reversed).
        Ok(())
    }
}
