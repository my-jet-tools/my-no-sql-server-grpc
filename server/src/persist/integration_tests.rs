//! Round-trips through the real slotted-page backend: every test opens a fresh
//! folder, writes through `PersistRepo`, then opens a **second** repo over the
//! same folder so what it reads comes from the recovery scan and not from any
//! in-memory state of the writer.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ahash::AHashMap;
use my_no_sql_grpc_abstractions::db_entity::{ParsedEntity, consts, write_varint};
use my_no_sql_grpc_abstractions::schemas::EntitySchema;
use my_no_sql_grpc_core::db::{DbRow, DbTableAttributes};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::{PersistRepo, partition_blob};

static FOLDER_NO: AtomicU64 = AtomicU64::new(0);

fn new_test_folder() -> String {
    let folder = std::env::temp_dir().join(format!(
        "my-no-sql-grpc-persist-{}-{}",
        std::process::id(),
        FOLDER_NO.fetch_add(1, Ordering::SeqCst)
    ));

    let _ = std::fs::remove_dir_all(&folder);
    folder.to_string_lossy().to_string()
}

fn build_row(partition_key: &str, row_key: &str, payload: &str, schema_id: u64) -> Arc<DbRow> {
    let mut src = Vec::new();

    for (field_no, value) in [(1u32, partition_key), (2, row_key), (5, payload)] {
        write_varint(
            &mut src,
            u64::from(field_no) << 3 | u64::from(consts::WIRE_TYPE_LEN),
        );
        write_varint(&mut src, value.len() as u64);
        src.extend_from_slice(value.as_bytes());
    }

    let parsed = ParsedEntity::parse(&src).unwrap();

    Arc::new(DbRow::new(
        parsed,
        schema_id,
        DateTimeAsMicroseconds::new(1_700_000_000_000_000),
    ))
}

async fn open(folder: &str, compress: bool) -> PersistRepo {
    PersistRepo::open(folder.to_string(), false, compress).await
}

/// Reads every partition back through a freshly opened repo, decoded into
/// (table, partition_key, row_keys).
async fn reload(folder: &str, compress: bool) -> Vec<(String, String, Vec<String>)> {
    let repo = open(folder, compress).await;

    let mut result = Vec::new();

    for loaded in repo.load_all_partitions(false).await {
        let row_keys = partition_blob::deserialize(&loaded.payload)
            .unwrap()
            .into_iter()
            .map(|persisted| {
                ParsedEntity::parse(&persisted.row)
                    .unwrap()
                    .get_row_key()
                    .to_string()
            })
            .collect();

        result.push((loaded.table_name, loaded.partition_key, row_keys));
    }

    result.sort();
    result
}

#[tokio::test]
async fn partitions_survive_a_reopen() {
    for compress in [false, true] {
        let folder = new_test_folder();

        {
            let repo = open(&folder, compress).await;
            repo.save_partition(
                "traders",
                "pk-1",
                &[
                    build_row("pk-1", "rk-1", "one", 7),
                    build_row("pk-1", "rk-2", "two", 7),
                ],
            )
            .await;
            repo.save_partition("traders", "pk-2", &[build_row("pk-2", "rk-1", "three", 7)])
                .await;
        }

        assert_eq!(
            reload(&folder, compress).await,
            vec![
                (
                    "traders".to_string(),
                    "pk-1".to_string(),
                    vec!["rk-1".to_string(), "rk-2".to_string()]
                ),
                (
                    "traders".to_string(),
                    "pk-2".to_string(),
                    vec!["rk-1".to_string()]
                ),
            ]
        );
    }
}

#[tokio::test]
async fn a_restored_row_keeps_its_schema_id_and_time_stamp() {
    let folder = new_test_folder();

    {
        let repo = open(&folder, true).await;
        repo.save_partition("t", "pk", &[build_row("pk", "rk", "payload", 4242)])
            .await;
    }

    let repo = open(&folder, true).await;
    let loaded = repo.load_all_partitions(false).await;
    let rows = partition_blob::deserialize(&loaded[0].payload).unwrap();

    assert_eq!(rows[0].schema_id, 4242);

    let parsed = ParsedEntity::parse(&rows[0].row).unwrap();
    assert_eq!(parsed.get_partition_key(), "pk");
    assert_eq!(parsed.time_stamp, Some(1_700_000_000_000_000));
}

#[tokio::test]
async fn a_deleted_partition_does_not_come_back() {
    let folder = new_test_folder();

    {
        let repo = open(&folder, false).await;
        repo.save_partition("t", "keep", &[build_row("keep", "rk", "x", 1)])
            .await;
        repo.save_partition("t", "drop", &[build_row("drop", "rk", "x", 1)])
            .await;
        repo.delete_partition("t", "drop").await;
    }

    let reloaded = reload(&folder, false).await;

    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded[0].1, "keep");
}

/// A partition which outgrows its size class moves to another page-file. The
/// old slot has to be freed, and the reopen must see exactly one copy - the new
/// one.
#[tokio::test]
async fn a_partition_that_changes_size_class_leaves_no_duplicate() {
    let folder = new_test_folder();

    {
        let repo = open(&folder, false).await;

        repo.save_partition("t", "pk", &[build_row("pk", "rk", "small", 1)])
            .await;

        let big_payload = "x".repeat(4096);
        repo.save_partition("t", "pk", &[build_row("pk", "rk-big", &big_payload, 1)])
            .await;
    }

    let reloaded = reload(&folder, false).await;

    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded[0].2, vec!["rk-big".to_string()]);
}

#[tokio::test]
async fn table_metadata_and_its_schemas_survive_a_reopen() {
    let folder = new_test_folder();

    let mut schemas = AHashMap::new();
    schemas.insert(7, Arc::new(EntitySchema::new(7, vec![1, 2, 3])));
    schemas.insert(9, Arc::new(EntitySchema::new(9, vec![4, 5])));

    let attributes = DbTableAttributes {
        persist: true,
        max_partitions_amount: Some(100),
        max_rows_per_partition_amount: None,
        created: DateTimeAsMicroseconds::new(1_700_000_000_000_000),
        schemas: Arc::new(schemas),
    };

    {
        let repo = open(&folder, false).await;
        repo.save_table_metadata("traders", &attributes).await;
    }

    let repo = open(&folder, false).await;
    let tables = repo.get_tables().await;

    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].table_name, "traders");
    assert_eq!(tables[0].attr.max_partitions_amount, Some(100));
    assert_eq!(tables[0].attr.max_rows_per_partition_amount, None);

    // The schemas come back with them, out of the same file and the same task:
    // that is what makes "rows on disk whose schema is not" unreachable.
    assert_eq!(tables[0].attr.schemas.len(), 2);
    assert_eq!(
        tables[0].attr.schemas.get(&7).unwrap().schema,
        vec![1, 2, 3]
    );
    assert_eq!(tables[0].attr.schemas.get(&9).unwrap().schema, vec![4, 5]);
}

/// The recovery scan must reuse the slots a vacuum freed rather than growing the
/// page-file forever, and the surviving partitions must still be readable.
#[tokio::test]
async fn vacuum_keeps_the_live_partitions() {
    let folder = new_test_folder();

    {
        let repo = open(&folder, false).await;

        for no in 0..10 {
            repo.save_partition(
                "t",
                &format!("pk-{no}"),
                &[build_row(&format!("pk-{no}"), "rk", "x", 1)],
            )
            .await;
        }

        for no in 0..8 {
            repo.delete_partition("t", &format!("pk-{no}")).await;
        }

        repo.vacuum().await;
    }

    let reloaded = reload(&folder, false).await;

    assert_eq!(reloaded.len(), 2);
    assert_eq!(reloaded[0].1, "pk-8");
    assert_eq!(reloaded[1].1, "pk-9");
}

/// A namespace created while the server is already running gets a folder which
/// may well have been written into before - the same namespace in a previous
/// life of the process, or one that was deleted and named again. Only the scan
/// says which versions are already in there; without it the fresh namespace
/// starts at version 0, below what is on disk, and the higher-version-wins dedup
/// of the next start hands back the old partition, reverting everything written
/// since.
#[tokio::test]
async fn a_namespace_opened_at_runtime_writes_above_what_its_folder_already_holds() {
    let root = new_test_folder();
    let folder = format!("{root}/archive");

    // The folder as a previous life of this namespace left it. Two writes, so
    // the highest version on disk is not zero.
    {
        let repo = open(&folder, false).await;

        repo.save_partition("traders", "pk-1", &[build_row("pk-1", "one", "x", 1)])
            .await;
        repo.save_partition("traders", "pk-1", &[build_row("pk-1", "two", "x", 1)])
            .await;
    }

    let settings = crate::settings_reader::SettingsModel {
        persistence_dest: root.clone(),
        location: "test".to_string(),
        compress_data: false,
        skip_broken_partitions: false,
        backups_dest: None,
        backup_interval_secs: None,
        max_backups: None,
        api_key: None,
    };

    let namespaces = crate::app::DbNamespaces::new();
    let db_namespace = namespaces
        .get_or_create("archive", &settings)
        .await
        .unwrap();

    db_namespace
        .persist_repo
        .save_partition("traders", "pk-1", &[build_row("pk-1", "three", "x", 1)])
        .await;

    assert_eq!(
        reload(&folder, false).await,
        vec![(
            "traders".to_string(),
            "pk-1".to_string(),
            vec!["three".to_string()]
        )]
    );

    let _ = std::fs::remove_dir_all(&root);
}
