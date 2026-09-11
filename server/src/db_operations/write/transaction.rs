use my_no_sql_grpc_core::db::{DbTable, TransactionAction};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};

use super::mark_partitions_to_persist;

/// Applies everything a transaction accumulated, in the order it was posted.
///
/// By the time this runs the transaction is already out of the registry and the
/// stream that carried its actions is long gone: what is left is a list, and the
/// table is entered once to apply all of it. That single visit is the whole
/// guarantee - no reader can take a snapshot between the delete and the insert,
/// and the subscribers get the whole transaction as one contiguous run of
/// instructions.
pub fn commit_transaction(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    actions: Vec<TransactionAction>,
    persist_moment: DateTimeAsMicroseconds,
) {
    let result = db_table.apply_transaction(actions);

    mark_partitions_to_persist(
        db_namespace,
        db_table,
        &result.partitions_to_persist,
        persist_moment,
    );

    crate::db_operations::sync::changes_applied(app, db_namespace, &db_table.name, result.changes);
}
