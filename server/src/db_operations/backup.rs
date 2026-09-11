use std::sync::Arc;

use my_no_sql_grpc_abstractions::db_entity::ParsedEntity;
use my_no_sql_grpc_core::db::{BulkWriteMode, DbRow, DbTable, DbTableAttributes, GetRowsFilter};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};
use crate::persist::backup::{BackupContent, BackupOnDisk, BackupZipBuilder};
use crate::persist::partition_blob;

use super::DbOperationError;
use super::write::{mark_partitions_to_persist, mark_table_metadata_to_persist};

/// One backup was taken.
pub struct TakenBackup {
    pub name_space: String,
    pub name: String,
}

/// Backs every namespace up, one zip each.
///
/// One archive per namespace and not one for the whole server, because that is
/// the unit everything else about a namespace is: its tables and its folder. Restoring one namespace is then one file, and how many backups to
/// keep is counted per namespace.
pub async fn make(
    app: &AppContext,
    now: DateTimeAsMicroseconds,
) -> Result<Vec<TakenBackup>, String> {
    let mut result = Vec::new();

    for db_namespace in app.namespaces.get_all() {
        // A namespace with no tables makes an empty archive, and an empty
        // archive taken every interval would push a real backup out of whatever
        // MaxBackups allows. Nothing to back up means no file at all.
        if db_namespace.tables.get_tables().is_empty() {
            continue;
        }

        let content = build(&db_namespace)?;
        let name = app.backups.save(&db_namespace.name, &content, now).await?;

        result.push(TakenBackup {
            name_space: db_namespace.name.clone(),
            name,
        });
    }

    Ok(result)
}

/// The snapshot is taken from **memory**, not from the page-files: what is on
/// disk lags by up to a sync period, and the backup worth having is of what the
/// server currently answers with.
fn build(db_namespace: &DbNamespace) -> Result<Vec<u8>, String> {
    let mut builder = BackupZipBuilder::new();

    for db_table in db_namespace.tables.get_tables().iter() {
        builder.add_table(&db_table.name, &db_table.get_attributes())?;

        for partition_key in db_table.get_partition_keys() {
            let db_rows = db_table.get_rows(&GetRowsFilter {
                partition_key: Some(&partition_key),
                row_key: None,
                skip: None,
                limit: None,
            });

            // A partition emptied between listing the keys and reading it is no
            // longer a partition, and a backup should not carry one.
            if db_rows.is_empty() {
                continue;
            }

            // Uncompressed: the zip deflates the entry, and zstd-ing it first
            // would pay for the compression twice and save it once.
            builder.add_partition(
                &db_table.name,
                &partition_key,
                &partition_blob::serialize(&db_rows, false),
            )?;
        }
    }

    builder.build()
}

pub async fn get_all(app: &AppContext, name_space: &str) -> Result<Vec<BackupOnDisk>, String> {
    app.backups.get_all(name_space).await
}

/// The bytes of a backup, to be handed to whoever asked to download it.
pub async fn download(app: &AppContext, name_space: &str, name: &str) -> Result<Vec<u8>, String> {
    app.backups.read(name_space, name).await
}

/// Puts a zip somebody handed back into the backups folder, under a name of this
/// server's own. It is not restored by it - restoring is a separate call, and an
/// archive worth keeping is worth looking inside first.
pub async fn upload(
    app: &AppContext,
    name_space: &str,
    content: &[u8],
    now: DateTimeAsMicroseconds,
) -> Result<String, String> {
    // Refused here rather than at restore: a file which is not a backup should
    // not be sitting in the backups folder looking like one.
    crate::persist::backup::read(content)?;

    app.backups.save(name_space, content, now).await
}

/// What is inside a backup, without restoring any of it.
pub async fn inspect(
    app: &AppContext,
    name_space: &str,
    name: &str,
) -> Result<BackupContent, String> {
    crate::persist::backup::read(&app.backups.read(name_space, name).await?)
}

/// The rows of one backed up partition, as they were stored.
pub async fn get_rows(
    app: &AppContext,
    name_space: &str,
    name: &str,
    table_name: &str,
    partition_key: &str,
) -> Result<Vec<Vec<u8>>, String> {
    let content = inspect(app, name_space, name).await?;

    let Some(table) = content
        .tables
        .iter()
        .find(|itm| itm.table_name == table_name)
    else {
        return Err(format!("the backup {name} has no table '{table_name}'"));
    };

    let Some(partition) = table
        .partitions
        .iter()
        .find(|itm| itm.partition_key == partition_key)
    else {
        return Err(format!(
            "the backup {name} has no partition '{partition_key}' of table '{table_name}'"
        ));
    };

    Ok(partition_blob::deserialize(&partition.blob)?
        .into_iter()
        .map(|itm| itm.row)
        .collect())
}

/// The one partition a restore was asked to put back.
pub struct RestoreOne {
    pub table_name: String,
    pub partition_key: String,
}

/// Puts a backup back into its namespace.
///
/// What is restored **replaces** what is there: a restored partition holds the
/// backup's partition and nothing else. Merging would leave rows nobody can
/// account for.
///
/// The schemas come back with the table's attributes, whatever is being
/// restored, one partition or all of them: a partition without the schema it was
/// written under is a partition nobody can show. `set_attributes` merges them,
/// so restoring into a table which is already there adds what the archive knew
/// rather than replacing what the table has learned since.
pub async fn restore(
    app: &AppContext,
    name_space: &str,
    name: &str,
    only: Option<RestoreOne>,
    persist_moment: DateTimeAsMicroseconds,
) -> Result<usize, DbOperationError> {
    let content = inspect(app, name_space, name)
        .await
        .map_err(DbOperationError::BackupFailed)?;

    let db_namespace = app
        .namespaces
        .get_or_create(name_space, &app.settings)
        .await?;

    let mut restored = 0;

    for table in content.tables {
        if let Some(only) = only.as_ref()
            && only.table_name != table.table_name
        {
            continue;
        }

        // A partition of a table the backup did not carry the attributes for is
        // still data - it lands in a table with default attributes rather than
        // being dropped, the same way the start up load treats one.
        let attributes = table
            .attributes
            .unwrap_or_else(DbTableAttributes::create_default);

        let db_table = db_namespace
            .tables
            .get_or_create(&table.table_name, || {
                Arc::new(DbTable::new(table.table_name.clone(), attributes.clone()))
            })
            .table;

        db_table.set_attributes(attributes);
        mark_table_metadata_to_persist(&db_namespace, &db_table, persist_moment);

        let mut db_rows = Vec::new();

        for partition in table.partitions {
            if let Some(only) = only.as_ref()
                && only.partition_key != partition.partition_key
            {
                continue;
            }

            db_rows.extend(
                to_db_rows(&partition.blob, &partition.partition_key)
                    .map_err(DbOperationError::BackupFailed)?,
            );

            restored += 1;
        }

        if db_rows.is_empty() {
            continue;
        }

        // One entry into the table for the whole archive's worth of it, so a
        // reader never sees the restore half way through.
        let result = db_table.bulk_write(BulkWriteMode::CleanPartitionsAndInsert, db_rows);

        mark_partitions_to_persist(
            &db_namespace,
            &db_table,
            &result.partitions_to_persist,
            persist_moment,
        );

        crate::db_operations::sync::bulk_written(
            app,
            &db_namespace,
            &db_table.name,
            BulkWriteMode::CleanPartitionsAndInsert,
            result.written,
        );
    }

    Ok(restored)
}

fn to_db_rows(blob: &[u8], partition_key: &str) -> Result<Vec<Arc<DbRow>>, String> {
    let mut result = Vec::new();

    for persisted in partition_blob::deserialize(blob)? {
        let parsed = ParsedEntity::parse(&persisted.row).map_err(|err| {
            format!("a row of the backed up partition '{partition_key}' does not parse: {err}")
        })?;

        // A stored row is in emit form, so it carries its TimeStamp.
        let time_stamp = match parsed.time_stamp {
            Some(time_stamp) => DateTimeAsMicroseconds::new(time_stamp),
            None => DateTimeAsMicroseconds::now(),
        };

        result.push(Arc::new(DbRow::new(
            parsed,
            persisted.schema_id,
            time_stamp,
        )));
    }

    Ok(result)
}
