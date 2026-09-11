use std::sync::Arc;

use my_no_sql_grpc_core::db::{BulkWriteMode, DbTable, GetRowsFilter};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};
use crate::db_operations::DbOperationError;

use super::{mark_partitions_to_persist, mark_table_metadata_to_persist};

/// Drops a whole namespace: every table it holds, and its folder on disk.
///
/// The default namespace is refused. A client which names no namespace has to
/// land somewhere, so it is not a namespace like the others - it is where
/// everything without a name goes, and it always exists.
pub async fn delete_namespace(
    app: &AppContext,
    name: &str,
    persist_moment: DateTimeAsMicroseconds,
) -> Result<(), DbOperationError> {
    let name = crate::app::DbNamespaces::resolve_name(name);

    if name == crate::consts::DEFAULT_NAMESPACE {
        return Err(DbOperationError::DefaultNamespaceCanNotBeDeleted);
    }

    let Some(db_namespace) = app.namespaces.remove(name).await else {
        return Err(DbOperationError::NamespaceNotFound(name.to_string()));
    };

    // Under the persist lock from here on: a persist pass which snapshotted the
    // namespaces before the removal may be inside `execute` right now, and every
    // file it opens is an `.expect(..)` - it would panic on the folder this call
    // is about to take away.
    let _persist_guard = app.persist_lock.lock().await;

    // Out of the map first: from here on nothing can resolve it by name, so no
    // write can land in what is about to go.
    for db_table in db_namespace.tables.get_tables().iter() {
        super::delete_table(app, &db_namespace, &db_table.name, persist_moment)?;
    }

    // The tables are gone from the namespace, so the persist loop would free
    // their slots - but nothing drains a namespace nobody can reach any more.
    // The folder goes as a whole instead.
    //
    // A folder which stays is the caller's answer, not a line in the log: the
    // namespace has left memory whatever happens here, nothing retries what is
    // left, and the leftover is a full copy which the next start loads back - so
    // "deleted" would be the one answer that is untrue.
    db_namespace
        .persist_repo
        .delete_everything()
        .await
        .map_err(|err| {
            DbOperationError::NamespaceFolderNotDeleted(format!(
                "Namespace '{}' is removed from the server but {err}",
                db_namespace.name
            ))
        })?;

    Ok(())
}

/// Moves a table between namespaces, data and all.
///
/// The rows are re-inserted rather than handed over: a namespace owns its folder,
/// so the data has to be written into the destination's own files. The schemas
/// need no move of their own - they are the table's attributes, and the
/// attributes are copied into the table the destination publishes.
pub async fn move_table_to_namespace(
    app: &AppContext,
    db_namespace: &DbNamespace,
    table_name: &str,
    destination: &Arc<DbNamespace>,
    persist_moment: DateTimeAsMicroseconds,
) -> Result<(), DbOperationError> {
    if db_namespace.name == destination.name {
        return Err(DbOperationError::TableAlreadyExists(table_name.to_string()));
    }

    if destination.tables.has_table(table_name) {
        return Err(DbOperationError::TableAlreadyExists(table_name.to_string()));
    }

    // The source leaves its namespace FIRST, and the rows are read out of the
    // table which is no longer in it. While it stays resolvable by name another
    // worker can be inside a write on it - every RPC is a task of its own - and
    // that row would be acknowledged, announced to the subscribers, and then
    // destroyed together with the source. It is the same argument `delete_table`
    // already stands on.
    let Some(source) = db_namespace.tables.remove(table_name) else {
        return Err(DbOperationError::TableNotFound(table_name.to_string()));
    };

    let attributes = source.get_attributes().as_ref().clone();
    let db_rows = source.get_rows(&GetRowsFilter::all());

    let db_table = Arc::new(DbTable::new(table_name.to_string(), attributes));
    let result = db_table.bulk_write(BulkWriteMode::CleanTableAndInsert, db_rows);

    // Published only once it holds the rows: an empty table which is still being
    // filled is resolvable, so a write could land in it - and the
    // `CleanTableAndInsert` above would be what wipes that write.
    destination.tables.insert(db_table.clone());

    mark_table_metadata_to_persist(destination, &db_table, persist_moment);
    mark_partitions_to_persist(
        destination,
        &db_table,
        &result.partitions_to_persist,
        persist_moment,
    );

    // Whoever is subscribed in the destination gets the table as it now is.
    crate::db_operations::sync::bulk_written(
        app,
        destination,
        &db_table.name,
        BulkWriteMode::CleanTableAndInsert,
        result.written,
    );

    // ...and only then do the subscribers of the source lose it: a reader which
    // follows both would otherwise be told it is nowhere. This is the rest of
    // what `delete_table` does - emptying the table is what hands over the
    // partition keys whose slots the source folder has to free.
    let partition_keys = source.clean();

    mark_partitions_to_persist(db_namespace, &source, &partition_keys, persist_moment);
    mark_table_metadata_to_persist(db_namespace, &source, persist_moment);

    crate::db_operations::sync::table_deleted(app, db_namespace, table_name);

    Ok(())
}
