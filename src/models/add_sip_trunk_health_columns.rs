use sea_orm_migration::prelude::*;

/// Adds health-monitor configuration and runtime counter columns to the
/// `rustpbx_sip_trunks` table for databases created before Plan 1
/// (gateway OPTIONS health monitor) introduced these fields.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table_name = "rustpbx_sip_trunks";

        if !manager
            .has_column(table_name, "health_check_interval_secs")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(super::sip_trunk::legacy::Entity)
                        .add_column(
                            ColumnDef::new(super::sip_trunk::legacy::Column::HealthCheckIntervalSecs)
                                .integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !manager.has_column(table_name, "failure_threshold").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(super::sip_trunk::legacy::Entity)
                        .add_column(
                            ColumnDef::new(super::sip_trunk::legacy::Column::FailureThreshold)
                                .integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !manager.has_column(table_name, "recovery_threshold").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(super::sip_trunk::legacy::Entity)
                        .add_column(
                            ColumnDef::new(super::sip_trunk::legacy::Column::RecoveryThreshold)
                                .integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !manager
            .has_column(table_name, "consecutive_failures")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(super::sip_trunk::legacy::Entity)
                        .add_column(
                            ColumnDef::new(super::sip_trunk::legacy::Column::ConsecutiveFailures)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !manager
            .has_column(table_name, "consecutive_successes")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(super::sip_trunk::legacy::Entity)
                        .add_column(
                            ColumnDef::new(super::sip_trunk::legacy::Column::ConsecutiveSuccesses)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
