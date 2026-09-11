use std::sync::Arc;

use my_no_sql_grpc_core::db::{DbPartition, DbRow, DbTable, DbTableAttributes};
use my_no_sql_grpc_core::db_entity::ParsedEntity;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};
use crate::persist::{layout, partition_blob};

/// Loads every namespace back from disk and only then lets requests in - a table
/// served half-loaded would look like a table that lost its data.
pub async fn load_from_disk(app: Arc<AppContext>) {
    let root = app.settings.get_persistence_dest();

    let mut namespaces = layout::get_namespaces_on_disk(&root).await;

    // The default namespace always exists, even on a first ever start: a client
    // which names no namespace has to land somewhere.
    if !namespaces
        .iter()
        .any(|itm| itm == crate::consts::DEFAULT_NAMESPACE)
    {
        namespaces.push(crate::consts::DEFAULT_NAMESPACE.to_string());
    }

    for name in namespaces {
        let db_namespace = app.namespaces.open_on_start_up(&name, &app.settings).await;
        load_namespace(&app, &db_namespace).await;
    }

    app.states.set_initialized();

    println!(
        "Initialized. Namespaces loaded: {}",
        app.namespaces.get_all().len()
    );
}

async fn load_namespace(app: &AppContext, db_namespace: &DbNamespace) {
    let skip_errors = app.settings.skip_broken_partitions;

    for loaded in db_namespace.persist_repo.get_tables().await {
        db_namespace.tables.get_or_create(&loaded.table_name, || {
            Arc::new(DbTable::new(loaded.table_name.clone(), loaded.attr.clone()))
        });
    }

    let mut partitions_loaded = 0;
    let mut rows_loaded = 0;

    for loaded in db_namespace
        .persist_repo
        .load_all_partitions(skip_errors)
        .await
    {
        let rows = match partition_blob::deserialize(&loaded.payload) {
            Ok(rows) => rows,
            Err(err) => {
                let msg = format!(
                    "Can not read partition '{}' of table '{}' in namespace '{}': {}",
                    loaded.partition_key, loaded.table_name, db_namespace.name, err
                );

                if skip_errors {
                    println!("{msg}. The partition is skipped.");
                    continue;
                }

                panic!("{msg}");
            }
        };

        // A partition whose table is missing from tables.meta is still data - it
        // is loaded into a table with default attributes rather than dropped.
        let db_table = db_namespace
            .tables
            .get_or_create(&loaded.table_name, || {
                println!(
                    "Table '{}' of namespace '{}' has partitions but no entry in tables.meta - restoring it with default attributes",
                    loaded.table_name, db_namespace.name
                );
                Arc::new(DbTable::new(
                    loaded.table_name.clone(),
                    DbTableAttributes::create_default(),
                ))
            })
            .table;

        let mut db_partition = DbPartition::new(loaded.partition_key.clone());

        for persisted in rows {
            let parsed = match ParsedEntity::parse(&persisted.row) {
                Ok(parsed) => parsed,
                Err(err) => {
                    let msg = format!(
                        "Can not parse a row of partition '{}' of table '{}' in namespace '{}': {}",
                        loaded.partition_key, loaded.table_name, db_namespace.name, err
                    );

                    if skip_errors {
                        println!("{msg}. The row is skipped.");
                        continue;
                    }

                    panic!("{msg}");
                }
            };

            // A stored row is in emit form, so it always carries its TimeStamp.
            // Falling back to `now` for one that somehow does not keeps the row
            // rather than losing it.
            let time_stamp = match parsed.time_stamp {
                Some(time_stamp) => DateTimeAsMicroseconds::new(time_stamp),
                None => DateTimeAsMicroseconds::now(),
            };

            db_partition.insert_or_replace(Arc::new(DbRow::new(
                parsed,
                persisted.schema_id,
                time_stamp,
            )));
            rows_loaded += 1;
        }

        db_table.restore_partition(db_partition);
        partitions_loaded += 1;
    }

    let schemas_loaded: usize = db_namespace
        .tables
        .get_tables()
        .iter()
        .map(|db_table| db_table.get_attributes().schemas.len())
        .sum();

    println!(
        "Namespace '{}' loaded: {} tables, {} partitions, {} rows, {} schemas",
        db_namespace.name,
        db_namespace.tables.get_tables().len(),
        partitions_loaded,
        rows_loaded,
        schemas_loaded
    );
}
