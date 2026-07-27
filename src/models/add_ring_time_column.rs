use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let table_name = "rustpbx_call_records";

        if !manager.has_column(table_name, "ring_time").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(crate::models::call_record::Entity)
                        .add_column(
                            ColumnDef::new(crate::models::call_record::Column::RingTime)
                                .timestamp_with_time_zone()
                                .null(),
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
