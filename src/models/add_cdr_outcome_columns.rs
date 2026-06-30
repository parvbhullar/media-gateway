//! enrich-cdr-api — denormalized outcome columns on `rustpbx_call_records`.
//!
//! Adds the OK/USR/SYS class and parsed Q.850 cause/text plus a sip-flow
//! availability flag, so the carrier summary can `GROUP BY` them instead of
//! re-parsing `metadata.hangup_messages` on every read. Written at call end by
//! `persist_call_record` via `crate::callrecord::outcome` (the canonical
//! taxonomy, incl. the 480+cause102→SYS override).
//!
//! Existing rows are backfilled with the *base* (code-only) classification — a
//! portable `CASE` that is correct for the overwhelming majority. The Q.850
//! override and `q850_cause`/`q850_text` are populated only on new writes;
//! historical rows keep `q850_*` NULL and a handful of 480-cause-102 rows read
//! `USR` rather than `SYS`. `sipflow_available` defaults to `false` for old
//! rows. Additive, idempotent (`has_column` + `IS NULL` guards), no-op `down`.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

use crate::models::call_record::{Column, Entity};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table = "rustpbx_call_records";

        if !manager.has_column(table, "outcome_kind").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Entity)
                        .add_column(ColumnDef::new(Column::OutcomeKind).string().null())
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column(table, "q850_cause").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Entity)
                        .add_column(ColumnDef::new(Column::Q850Cause).small_integer().null())
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column(table, "q850_text").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Entity)
                        .add_column(ColumnDef::new(Column::Q850Text).text().null())
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column(table, "sipflow_available").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Entity)
                        .add_column(
                            ColumnDef::new(Column::SipflowAvailable)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // Composite index for the grouped summary's common path:
        // WHERE started_at range  GROUP BY <bucket>, outcome_kind, status_code.
        if !manager
            .has_index(table, "idx_rustpbx_call_records_outcome")
            .await?
        {
            manager
                .create_index(
                    Index::create()
                        .if_not_exists()
                        .name("idx_rustpbx_call_records_outcome")
                        .table(Entity)
                        .col(Column::StartedAt)
                        .col(Column::OutcomeKind)
                        .col(Column::StatusCode)
                        .to_owned(),
                )
                .await?;
        }

        // Backfill the base (code-only) class for existing rows. Portable CASE;
        // the Q.850 override and q850_* are left to new writes (see module doc).
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
        conn.execute(Statement::from_string(
            backend,
            r#"
            UPDATE rustpbx_call_records SET outcome_kind = CASE
              WHEN status_code BETWEEN 200 AND 299 THEN 'OK'
              WHEN status IN ('completed','answered')
                   AND (status_code IS NULL OR status_code = 0) THEN 'OK'
              WHEN status_code IN (480,486,487,600,603) THEN 'USR'
              WHEN status_code IN (401,403,404,407,408) THEN 'SYS'
              WHEN status_code BETWEEN 500 AND 599 THEN 'SYS'
              WHEN status_code BETWEEN 600 AND 699 THEN 'USR'
              ELSE 'SYS'
            END
            WHERE outcome_kind IS NULL
            "#
            .to_owned(),
        ))
        .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // No-op down, matching the other add_*_column migrations.
        Ok(())
    }
}
