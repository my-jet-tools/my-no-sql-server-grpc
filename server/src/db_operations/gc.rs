use my_no_sql_grpc_core::db::{AppliedChange, DbTable, GcResult};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};

use super::write::mark_partitions_to_persist;

/// One garbage collection pass over one table.
///
/// Returns whether it took anything out, so the timer can say so once instead of
/// per table.
pub fn collect(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    now: DateTimeAsMicroseconds,
    persist_moment: DateTimeAsMicroseconds,
) -> bool {
    applied(
        app,
        db_namespace,
        db_table,
        db_table.gc(now),
        persist_moment,
    )
}

/// Drops the schemas of one table no row of it names any more.
///
/// It runs on the same timer as the row collector because it is the same kind of
/// question, and the reasoning behind each of its rules is on
/// [`DbTable::gc_schemas`]. Nothing is told to the subscribers: a reader's cache
/// is rows and table attributes, and the schemas are not among the attributes it
/// is sent - showing a row as JSON is this server's own job. What the disk is
/// owed is the table's metadata, which is where a schema lives.
pub fn collect_schemas(
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    persist_moment: DateTimeAsMicroseconds,
) -> bool {
    if !db_table.gc_schemas() {
        return false;
    }

    super::write::mark_table_metadata_to_persist(db_namespace, db_table, persist_moment);

    true
}

/// The table's own limit, applied right now with a number of its own. What the
/// timer does on a schedule, an operator can ask for with a different figure.
pub fn keep_max_partitions_amount(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    max: usize,
    persist_moment: DateTimeAsMicroseconds,
) {
    let result = db_table.keep_max_partitions_amount(max);
    applied(app, db_namespace, db_table, result, persist_moment);
}

pub fn keep_max_rows_in_partition(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    partition_key: &str,
    max: usize,
    persist_moment: DateTimeAsMicroseconds,
) {
    let result = db_table.keep_max_rows_in_partition(partition_key, max);
    applied(app, db_namespace, db_table, result, persist_moment);
}

/// What a pass owes the rest of the server. The partitions go first and the rows
/// after them, all in one queue entry: a pass is one thing that happened, and a
/// subscriber should not have another writer's chunks in the middle of it.
fn applied(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    result: GcResult,
    persist_moment: DateTimeAsMicroseconds,
) -> bool {
    if result.is_empty() {
        return false;
    }

    mark_partitions_to_persist(
        db_namespace,
        db_table,
        &result.partitions_to_persist,
        persist_moment,
    );

    let mut changes = Vec::new();

    if !result.partitions_removed.is_empty() {
        changes.push(AppliedChange::PartitionsDeleted(result.partitions_removed));
    }

    if !result.rows_removed.is_empty() {
        changes.push(AppliedChange::RowsDeleted(result.rows_removed));
    }

    crate::db_operations::sync::changes_applied(app, db_namespace, &db_table.name, changes);

    true
}
