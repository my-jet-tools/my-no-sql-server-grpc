use my_no_sql_grpc_core::db::GetRowsFilter;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};
use crate::persist::markers::PersistTask;

/// Writes one queued task per namespace. Returns whether anything was written,
/// so the timer can come straight back for more instead of sleeping with a
/// non-empty queue.
///
/// `now: None` drains everything regardless of its due moment - that is the
/// shutdown path.
pub async fn persist(app: &AppContext, now: Option<DateTimeAsMicroseconds>) -> bool {
    // One pass at a time: the flush call writes through the same page-files, and
    // two passes interleaved could put a partition on disk before the metadata
    // naming its table.
    let _single_flight = app.persist_lock.lock().await;

    let mut did_work = false;

    for db_namespace in app.namespaces.get_all() {
        let Some(task) = db_namespace.persist_markers.get_task(now) else {
            continue;
        };

        execute(&db_namespace, task).await;
        db_namespace.persisted(DateTimeAsMicroseconds::now(), 1);
        did_work = true;
    }

    did_work
}

/// Writes everything the queue holds right now, whatever sync period each change
/// asked for, and answers how much that was.
///
/// The queue is **taken in one go when the call starts**: a change written while
/// the flush is running belongs to the next flush. Draining until the queue is
/// empty would be the same call under a live writer that never returns - and
/// what an operator wants to know is that everything they had written before
/// they asked is on disk, which a snapshot answers exactly.
pub async fn flush(app: &AppContext) -> usize {
    let _single_flight = app.persist_lock.lock().await;

    let mut written = 0;

    for db_namespace in app.namespaces.get_all() {
        let tasks = db_namespace.persist_markers.take_all();

        if tasks.is_empty() {
            continue;
        }

        let of_this_namespace = tasks.len();

        for task in tasks {
            execute(&db_namespace, task).await;
            written += 1;
        }

        db_namespace.persisted(DateTimeAsMicroseconds::now(), of_this_namespace);
    }

    written
}

async fn execute(db_namespace: &DbNamespace, task: PersistTask) {
    match task {
        PersistTask::TableMetadata { table_name } => {
            let Some(db_table) = db_namespace.tables.get_table(&table_name) else {
                db_namespace
                    .persist_repo
                    .delete_table_metadata(&table_name)
                    .await;
                return;
            };

            db_namespace
                .persist_repo
                .save_table_metadata(&table_name, &db_table.get_attributes())
                .await;
        }

        PersistTask::Partition {
            table_name,
            partition_key,
        } => {
            let Some(db_table) = db_namespace.tables.get_table(&table_name) else {
                db_namespace
                    .persist_repo
                    .delete_partition(&table_name, &partition_key)
                    .await;
                return;
            };

            // A table which is not persisted must hold nothing on disk -
            // including the slots it filled while the attribute was still on.
            // Freeing them is what makes a delete or a clean of such a table
            // reach the disk at all: the load path does not consult the
            // attribute, so a slot left here comes back as data on the next
            // start.
            if !db_table.get_attributes().persist {
                db_namespace
                    .persist_repo
                    .delete_partition(&table_name, &partition_key)
                    .await;
                return;
            }

            // The rows are cloned out of the table under its lock and written
            // afterwards, so the disk I/O never holds the table.
            let db_rows = db_table.get_rows(&GetRowsFilter {
                partition_key: Some(&partition_key),
                row_key: None,
                skip: None,
                limit: None,
            });

            // No rows means the partition is gone - deleting a row empties its
            // partition and the partition itself is dropped with it.
            if db_rows.is_empty() {
                db_namespace
                    .persist_repo
                    .delete_partition(&table_name, &partition_key)
                    .await;
                return;
            }

            db_namespace
                .persist_repo
                .save_partition(&table_name, &partition_key, &db_rows)
                .await;
        }
    }
}
