use my_no_sql_grpc_core::db::DbTable;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::DbNamespace;

/// Marked whatever the table's `persist` attribute says right now, and that is
/// deliberate: the queue records **what changed**, and what the disk is supposed
/// to hold because of it is decided when the task is drained, where the table can
/// still be looked at.
///
/// Gating the mark instead would make a delete queue nothing at all for a table
/// whose persistence is off - and such a table can perfectly well own slots and a
/// `tables.meta` entry, written while the attribute was still on. The load path
/// does not consult the attribute either, so those slots come back on the next
/// start: a table deleted before the restart is there afterwards, with every row.
pub(crate) fn mark_partition_to_persist(
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    partition_key: &str,
    persist_moment: DateTimeAsMicroseconds,
) {
    db_namespace
        .persist_markers
        .persist_partition(&db_table.name, partition_key, persist_moment);
}

/// A partition is the smallest thing the persist loop writes, so an operation
/// which touched many of them owes a mark for each: the loop looks every one of
/// them up and writes it, or frees its slot when the table no longer holds it.
pub fn mark_partitions_to_persist(
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    partition_keys: &[String],
    persist_moment: DateTimeAsMicroseconds,
) {
    for partition_key in partition_keys {
        db_namespace.persist_markers.persist_partition(
            &db_table.name,
            partition_key,
            persist_moment,
        );
    }
}

pub(crate) fn mark_table_metadata_to_persist(
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    persist_moment: DateTimeAsMicroseconds,
) {
    db_namespace
        .persist_markers
        .persist_table_metadata(&db_table.name, persist_moment);
}
