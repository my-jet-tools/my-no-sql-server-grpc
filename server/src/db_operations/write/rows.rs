use std::sync::Arc;

use my_no_sql_grpc_core::db::{
    BulkWriteMode, DbRow, DbTable, PartitionRowKeys, ReplaceIfVersionMatchesResult,
};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};
use crate::db_operations::DbOperationError;

use super::mark_partition_to_persist;

pub fn insert(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    db_row: Arc<DbRow>,
    persist_moment: DateTimeAsMicroseconds,
) -> Result<(), DbOperationError> {
    if !db_table.insert_if_not_exists(db_row.clone()) {
        return Err(DbOperationError::RowAlreadyExists);
    }

    written(app, db_namespace, db_table, db_row, persist_moment);
    Ok(())
}

pub fn insert_or_replace(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    db_row: Arc<DbRow>,
    persist_moment: DateTimeAsMicroseconds,
) {
    db_table.insert_or_replace(db_row.clone());
    written(app, db_namespace, db_table, db_row, persist_moment);
}

/// Keeps the stored row unless the incoming one is strictly newer, and says
/// which of the two happened.
///
/// What is compared is the `TimeStamp` the entity carries, so the caller has to
/// have built the row from the client's own value - the server's clock would
/// make every row newer than the stored one and the whole call an
/// `InsertOrReplace`.
///
/// A row that was not taken is not a failure and nothing is told about it: the
/// stored row is at least as new, so a subscriber handed the incoming one would
/// have its cache pushed backwards.
pub fn insert_or_replace_if_new(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    db_row: Arc<DbRow>,
    persist_moment: DateTimeAsMicroseconds,
) -> bool {
    let result = db_table.bulk_write(BulkWriteMode::InsertOrReplaceIfNew, vec![db_row.clone()]);

    if result.written.is_empty() {
        return false;
    }

    written(app, db_namespace, db_table, db_row, persist_moment);
    true
}

/// Overwrites a stored row, and refuses when somebody has rewritten it since the
/// caller read it.
///
/// `expected_time_stamp` is the version the client read the row at - the
/// TimeStamp its entity carries - and it is a different moment from the one the
/// row being written will carry: that one is stamped by the server like on every
/// other write, which is what makes the next reader see a new version and the
/// read-modify-write loop terminate. Without the check both of two processes
/// editing the same row are told `Ok`, and one of the two edits is gone with
/// nobody anywhere hearing about it.
pub fn replace(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    db_row: Arc<DbRow>,
    expected_time_stamp: DateTimeAsMicroseconds,
    persist_moment: DateTimeAsMicroseconds,
) -> Result<(), DbOperationError> {
    match db_table.replace_if_version_matches(db_row.clone(), expected_time_stamp) {
        ReplaceIfVersionMatchesResult::Replaced => {}
        ReplaceIfVersionMatchesResult::RowNotFound => return Err(DbOperationError::RowNotFound),
        ReplaceIfVersionMatchesResult::VersionMismatch => {
            return Err(DbOperationError::OptimisticConcurrencyUpdateFails);
        }
    }

    written(app, db_namespace, db_table, db_row, persist_moment);
    Ok(())
}

/// Deletes one row and says whether there was one.
///
/// A key which is not there is **not** a failure: that is what `BulkDelete`
/// already answers to the same input, and two ways of deleting one key must not
/// disagree about it. Ported cleanup which loops over keys - and by then half of
/// them have expired - would otherwise stop on the first of them.
pub fn delete_row(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    partition_key: &str,
    row_key: &str,
    persist_moment: DateTimeAsMicroseconds,
) -> bool {
    if db_table.delete_row(partition_key, row_key).is_none() {
        return false;
    }

    // The partition is marked even when the delete emptied it: the persist loop
    // finds no rows and takes that as "free the slot".
    mark_partition_to_persist(db_namespace, db_table, partition_key, persist_moment);

    crate::db_operations::sync::rows_deleted(
        app,
        db_namespace,
        &db_table.name,
        vec![PartitionRowKeys {
            partition_key: partition_key.to_string(),
            row_keys: vec![row_key.to_string()],
        }],
    );

    true
}

/// The two things every successful write owes the rest of the server: the disk
/// has to learn about it, and so do the readers.
fn written(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    db_row: Arc<DbRow>,
    persist_moment: DateTimeAsMicroseconds,
) {
    mark_partition_to_persist(
        db_namespace,
        db_table,
        db_row.get_partition_key(),
        persist_moment,
    );

    crate::db_operations::sync::rows_updated(
        app,
        db_namespace,
        &db_table.name,
        std::slice::from_ref(&db_row),
    );
}
