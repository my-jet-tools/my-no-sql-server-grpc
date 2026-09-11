use std::sync::Arc;

use my_no_sql_grpc_core::db::{AppliedChange, BulkWriteMode, DbRow, PartitionRowKeys};

use crate::app::{AppContext, DbNamespace};
use crate::reader::{SyncChunk, split_partition_row_keys, split_partition_rows, split_rows};

/// Tells every subscriber of this table about rows that were written.
///
/// Called **after** the write itself has committed - that order is what keeps a
/// reader which is subscribing right now from missing it, see
/// `DbTable::register_and_snapshot`.
pub fn rows_updated(
    app: &AppContext,
    db_namespace: &DbNamespace,
    table_name: &str,
    rows: &[Arc<DbRow>],
) {
    enqueue(app, db_namespace, table_name, || {
        let mut result = Vec::new();
        push_updated_rows(&mut result, table_name, rows.to_vec());
        result
    });
}

pub fn rows_deleted(
    app: &AppContext,
    db_namespace: &DbNamespace,
    table_name: &str,
    partitions: Vec<PartitionRowKeys>,
) {
    enqueue(app, db_namespace, table_name, || {
        let mut result = Vec::new();
        push_deleted_rows(&mut result, table_name, partitions);
        result
    });
}

/// What a batch looks like from the outside. Which chunks it becomes is decided
/// by the mode alone, because that is what the mode says: whether the subscriber
/// has to throw something away before taking the rows, and whether the rows are
/// an update of what it holds or the new content of whole partitions.
pub fn bulk_written(
    app: &AppContext,
    db_namespace: &DbNamespace,
    table_name: &str,
    mode: BulkWriteMode,
    rows: Vec<Arc<DbRow>>,
) {
    enqueue(app, db_namespace, table_name, || {
        let mut result = Vec::new();

        match mode {
            BulkWriteMode::InsertOrReplace | BulkWriteMode::InsertOrReplaceIfNew => {
                push_updated_rows(&mut result, table_name, rows);
            }

            // The rows are not an update of these partitions - they are what the
            // partitions now consist of, which is a different instruction.
            BulkWriteMode::CleanPartitionsAndInsert => {
                let chunks = split_partition_rows(rows);

                if chunks.is_empty() {
                    return result;
                }

                for partitions in chunks {
                    result.push(SyncChunk::InitPartitions {
                        table_name: table_name.to_string(),
                        partitions,
                    });
                }

                result.push(SyncChunk::InitPartitionsEnd {
                    table_name: table_name.to_string(),
                });
            }

            // Emptying the table is an instruction of its own, and it is queued
            // even when the batch brought no rows - such a batch is a CleanTable.
            BulkWriteMode::CleanTableAndInsert => {
                result.push(SyncChunk::CleanTable {
                    table_name: table_name.to_string(),
                });

                push_updated_rows(&mut result, table_name, rows);
            }
        }

        result
    });
}

pub fn table_cleaned(app: &AppContext, db_namespace: &DbNamespace, table_name: &str) {
    enqueue(app, db_namespace, table_name, || {
        vec![SyncChunk::CleanTable {
            table_name: table_name.to_string(),
        }]
    });
}

pub fn table_attributes_updated(
    app: &AppContext,
    db_namespace: &DbNamespace,
    table_name: &str,
    attributes: std::sync::Arc<my_no_sql_grpc_core::db::DbTableAttributes>,
) {
    enqueue(app, db_namespace, table_name, || {
        vec![SyncChunk::UpdateTableAttributes {
            table_name: table_name.to_string(),
            attributes,
        }]
    });
}

pub fn table_deleted(app: &AppContext, db_namespace: &DbNamespace, table_name: &str) {
    enqueue(app, db_namespace, table_name, || {
        vec![SyncChunk::DeleteTable {
            table_name: table_name.to_string(),
        }]
    });
}

pub fn partitions_deleted(
    app: &AppContext,
    db_namespace: &DbNamespace,
    table_name: &str,
    partition_keys: Vec<String>,
) {
    enqueue(app, db_namespace, table_name, || {
        vec![SyncChunk::CleanPartitions {
            table_name: table_name.to_string(),
            partition_keys,
        }]
    });
}

/// A list of changes, as the subscribers have to see it - what a transaction
/// did, or what a garbage collection pass took out.
///
/// The order is the caller's own and is not cosmetic - a row deleted and then
/// written again is not the same as a row written and then deleted - and all of
/// it goes into one `enqueue`, so the whole thing reaches a session as one
/// contiguous run with nobody else's chunks in the middle of it.
pub fn changes_applied(
    app: &AppContext,
    db_namespace: &DbNamespace,
    table_name: &str,
    changes: Vec<AppliedChange>,
) {
    enqueue(app, db_namespace, table_name, || {
        let mut result = Vec::new();

        for change in changes {
            match change {
                AppliedChange::TableCleaned => result.push(SyncChunk::CleanTable {
                    table_name: table_name.to_string(),
                }),
                AppliedChange::PartitionsDeleted(partition_keys) => {
                    result.push(SyncChunk::CleanPartitions {
                        table_name: table_name.to_string(),
                        partition_keys,
                    })
                }
                AppliedChange::RowsDeleted(partitions) => {
                    push_deleted_rows(&mut result, table_name, partitions)
                }
                AppliedChange::RowsWritten(rows) => {
                    push_updated_rows(&mut result, table_name, rows)
                }
            }
        }

        result
    });
}

/// Rows always travel as a cut batch closed by its `End`, so the reader applies
/// them in one go. An empty batch produces neither - there is nothing to close.
fn push_updated_rows(dest: &mut Vec<SyncChunk>, table_name: &str, rows: Vec<Arc<DbRow>>) {
    let chunks = split_rows(rows);

    if chunks.is_empty() {
        return;
    }

    for rows in chunks {
        dest.push(SyncChunk::UpdateRows {
            table_name: table_name.to_string(),
            rows,
        });
    }

    dest.push(SyncChunk::UpdateRowsEnd {
        table_name: table_name.to_string(),
    });
}

/// The same for a delete: cut, then closed by its `End`. A transaction can name
/// far more keys than fit into one message, so this is cut too even though it
/// carries no rows.
fn push_deleted_rows(
    dest: &mut Vec<SyncChunk>,
    table_name: &str,
    partitions: Vec<PartitionRowKeys>,
) {
    let chunks = split_partition_row_keys(partitions);

    if chunks.is_empty() {
        return;
    }

    for partitions in chunks {
        dest.push(SyncChunk::DeleteRows {
            table_name: table_name.to_string(),
            partitions,
        });
    }

    dest.push(SyncChunk::DeleteRowsEnd {
        table_name: table_name.to_string(),
    });
}

/// One `enqueue` call per session, carrying everything the operation produced:
/// the queue is what keeps the order, and a chunk left for a second call could
/// end up behind another writer's.
///
/// The chunks are built only when somebody is listening - cutting a batch nobody
/// subscribed to would be the whole cost of the write for none of its use.
fn enqueue(
    app: &AppContext,
    db_namespace: &DbNamespace,
    table_name: &str,
    build_chunks: impl FnOnce() -> Vec<SyncChunk>,
) {
    let sessions = app
        .reader_sessions
        .get_subscribed(&db_namespace.name, table_name);

    if sessions.is_empty() {
        return;
    }

    let chunks = build_chunks();

    if chunks.is_empty() {
        return;
    }

    for session in sessions {
        session.enqueue(chunks.iter().cloned());
    }
}
