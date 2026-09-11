use my_no_sql_grpc_core::db::DbTableAttributes;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::models::TableAttributesInputContract;

pub fn to_attributes(
    src: &TableAttributesInputContract,
    now: DateTimeAsMicroseconds,
) -> DbTableAttributes {
    DbTableAttributes {
        persist: src.persist,
        max_partitions_amount: to_optional_limit(src.max_partitions_amount),
        max_rows_per_partition_amount: to_optional_limit(src.max_rows_per_partition_amount),
        // `created` is overwritten by `set_table_attributes` with the moment the
        // table actually came into being - setting a limit is not a new table.
        created: now,
        // Likewise the schemas: no request names one, and `DbTable` merges them
        // instead of taking them so that this call - which replaces rather than
        // patches - can not leave the stored rows without a shape.
        schemas: Default::default(),
    }
}

/// 0 is what a caller who does not want a limit writes when they would rather
/// pass a number than leave the parameter out. It is "no limit", not "no
/// partitions".
pub fn to_optional_limit(src: Option<i64>) -> Option<usize> {
    let value = src?;

    if value <= 0 {
        return None;
    }

    Some(value as usize)
}
