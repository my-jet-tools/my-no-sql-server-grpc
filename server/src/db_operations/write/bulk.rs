use std::sync::Arc;

use my_no_sql_grpc_core::db::{
    AppliedChange, BulkWriteMode, DbRow, DbTable, PartitionRowKeys, TransactionAction,
};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};

use super::mark_partitions_to_persist;

/// Applies a whole batch and tells the disk and the subscribers about it once.
///
/// The stream the batch arrived on is long gone by this point: it was transport
/// and nothing more. What matters here is that the table is entered exactly
/// once. A batch applied row by row would let a reader take a snapshot in the
/// middle of it, and would cost one queue entry per row instead of one per batch.
pub fn bulk_write(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    mode: BulkWriteMode,
    rows: Vec<Arc<DbRow>>,
    persist_moment: DateTimeAsMicroseconds,
) {
    let result = db_table.bulk_write(mode, rows);

    mark_partitions_to_persist(
        db_namespace,
        db_table,
        &result.partitions_to_persist,
        persist_moment,
    );

    crate::db_operations::sync::bulk_written(
        app,
        db_namespace,
        &db_table.name,
        mode,
        result.written,
    );
}

/// Deletes rows named by key, across as many partitions as it takes, in one
/// entry into the table. Answers how many of them were actually there.
///
/// It goes down the transaction path because that is what a list of deletes
/// applied under one lock is - not because a transaction was opened. Deleting is
/// the whole of this call, so there is nothing to open, and the caller does not
/// have to keep an id alive between two round trips to get the one visit.
pub fn bulk_delete(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    partitions: Vec<PartitionRowKeys>,
    persist_moment: DateTimeAsMicroseconds,
) -> usize {
    let result = db_table.apply_transaction(
        partitions
            .into_iter()
            .map(TransactionAction::DeleteRows)
            .collect(),
    );

    // Only the keys which were there come back, so this is how many rows went
    // rather than how many the caller named.
    let deleted = result
        .changes
        .iter()
        .map(|change| match change {
            AppliedChange::RowsDeleted(partitions) => {
                partitions.iter().map(|itm| itm.row_keys.len()).sum()
            }
            _ => 0,
        })
        .sum();

    mark_partitions_to_persist(
        db_namespace,
        db_table,
        &result.partitions_to_persist,
        persist_moment,
    );

    crate::db_operations::sync::changes_applied(app, db_namespace, &db_table.name, result.changes);

    deleted
}
