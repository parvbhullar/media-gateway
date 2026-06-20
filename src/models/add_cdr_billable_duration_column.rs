//! task 2.3 — add `billable_duration_secs` to `rustpbx_call_records`.
//!
//! `duration_secs` is wall-clock (`ended_at − started_at`); billing needs the
//! answered span (`ended_at − answer_time`). This adds a nullable column for
//! it (NULL when the call never answered), persisted by `persist_call_record`
//! and exposed on the REST CdrView + the `call.completed` webhook. Additive,
//! idempotent (`has_column` guard), no-op `down` per the codebase convention.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table_name = "rustpbx_call_records";

        if !manager
            .has_column(table_name, "billable_duration_secs")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(crate::models::call_record::Entity)
                        .add_column(
                            ColumnDef::new(
                                crate::models::call_record::Column::BillableDurationSecs,
                            )
                            .integer()
                            .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // No-op down, matching the other add_*_column migrations (additive,
        // never reversed).
        Ok(())
    }
}
