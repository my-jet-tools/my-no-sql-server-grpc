use std::sync::Arc;

use my_no_sql_grpc_core::db::{
    DbRow, DbTable, GetRowStatisticsResult, GetRowsFilter, RowStatistics,
};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::db_operations::DbOperationError;

pub fn get_row(db_table: &DbTable, partition_key: &str, row_key: &str) -> Option<Arc<DbRow>> {
    let db_row = db_table.get_row(partition_key, row_key)?;
    db_row.update_last_read_access(DateTimeAsMicroseconds::now());
    Some(db_row)
}

/// Unlike its neighbours, this one **does not** move the row's last-read mark -
/// see [`DbTable::get_row_statistics`]. Reading a statistic must not change it.
pub fn get_row_statistics(
    db_table: &DbTable,
    partition_key: &str,
    row_key: &str,
) -> Result<RowStatistics, DbOperationError> {
    match db_table.get_row_statistics(partition_key, row_key) {
        GetRowStatisticsResult::Found(statistics) => Ok(statistics),
        GetRowStatisticsResult::PartitionNotFound => Err(DbOperationError::PartitionNotFound(
            partition_key.to_string(),
        )),
        GetRowStatisticsResult::RowNotFound => Err(DbOperationError::RowNotFound),
    }
}

pub fn get_rows(db_table: &DbTable, filter: &GetRowsFilter) -> Vec<Arc<DbRow>> {
    let result = db_table.get_rows(filter);

    let now = DateTimeAsMicroseconds::now();
    for db_row in result.iter() {
        db_row.update_last_read_access(now);
    }

    result
}
