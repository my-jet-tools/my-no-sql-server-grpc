use my_no_sql_grpc_core::db::DbTable;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};

use super::mark_partitions_to_persist;

/// Drops whole partitions. Only the ones which were actually there are marked
/// and announced: a key the table never held changes nothing on the disk, and a
/// subscriber mirrors the table, so it does not hold it either.
pub fn delete_partitions(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    partition_keys: &[String],
    persist_moment: DateTimeAsMicroseconds,
) {
    let removed = db_table.delete_partitions(partition_keys);

    if removed.is_empty() {
        return;
    }

    mark_partitions_to_persist(db_namespace, db_table, &removed, persist_moment);

    crate::db_operations::sync::partitions_deleted(app, db_namespace, &db_table.name, removed);
}
