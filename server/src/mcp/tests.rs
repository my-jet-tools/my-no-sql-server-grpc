//! The tools driven against a real `AppContext`, which is what the middleware
//! does with them - it only takes the JSON apart and hands over the input.
//!
//! What is worth holding here is what the tools decide rather than what the
//! database does: the gate, the schema a write goes through, and the answers a
//! caller is meant to act on.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mcp_server_middleware::McpToolCall;
use my_no_sql_grpc_core::db::{DbTable, DbTableAttributes, GetRowStatisticsResult};
use my_no_sql_grpc_core::schemas::{
    DeclaredField, EntitySchema, Scalar, SchemaBuilder, get_schema_id,
};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::*;
use crate::settings_reader::SettingsModel;

static FOLDER_NO: AtomicU64 = AtomicU64::new(0);

const TABLE: &str = "traders";

fn new_test_folder() -> String {
    let folder = std::env::temp_dir().join(format!(
        "my-no-sql-grpc-mcp-{}-{}",
        std::process::id(),
        FOLDER_NO.fetch_add(1, Ordering::SeqCst)
    ));

    let _ = std::fs::remove_dir_all(&folder);
    folder.to_string_lossy().to_string()
}

fn settings(folder: &str) -> Arc<SettingsModel> {
    settings_with_backups(folder, None)
}

fn settings_with_backups(folder: &str, backups_dest: Option<String>) -> Arc<SettingsModel> {
    Arc::new(SettingsModel {
        persistence_dest: folder.to_string(),
        location: "test".to_string(),
        compress_data: false,
        skip_broken_partitions: false,
        backups_dest,
        backup_interval_secs: None,
        max_backups: None,
        api_key: None,
    })
}

/// The shape a client's macro would declare for this entity.
fn schema() -> EntitySchema {
    schema_of(&[
        DeclaredField::scalar("PartitionKey", 1, Scalar::String, false),
        DeclaredField::scalar("RowKey", 2, Scalar::String, false),
        DeclaredField::scalar("Amount", 5, Scalar::F64, false),
    ])
}

/// A second version of the same entity - one more field, so a different shape
/// and, through the hash, a different id.
fn schema_v2() -> EntitySchema {
    schema_of(&[
        DeclaredField::scalar("PartitionKey", 1, Scalar::String, false),
        DeclaredField::scalar("RowKey", 2, Scalar::String, false),
        DeclaredField::scalar("Amount", 5, Scalar::F64, false),
        DeclaredField::scalar("Level", 6, Scalar::I32, false),
    ])
}

fn schema_of(fields: &[DeclaredField]) -> EntitySchema {
    let mut builder = SchemaBuilder::new("TraderEntity");

    for field in fields {
        builder = builder.add_field(field.clone());
    }

    let bytes = builder.build().serialize();

    // The id a client's macro folds out of its own type is a hash of exactly
    // these bytes, so this is the same number it would send.
    EntitySchema::new(get_schema_id(&bytes), bytes)
}

/// A server which is up, loaded, and holds one table with one schema.
async fn app_with_table() -> Arc<AppContext> {
    app_with_table_and_settings(settings(&new_test_folder())).await
}

async fn app_with_table_and_settings(settings: Arc<SettingsModel>) -> Arc<AppContext> {
    let app = Arc::new(AppContext::new(settings));
    crate::operations::load_from_disk(app.clone()).await;

    let db_namespace = app
        .namespaces
        .get_or_create("", &app.settings)
        .await
        .unwrap();

    let db_table = crate::db_operations::write::create_table(
        &db_namespace,
        TABLE,
        DbTableAttributes::create_default(),
        DateTimeAsMicroseconds::now(),
    )
    .unwrap();

    db_table.register_schema(schema());

    app
}

fn open_writes(app: &AppContext) {
    app.open_mcp_writes(DateTimeAsMicroseconds::now());
}

async fn insert(app: &Arc<AppContext>, entity_json: &str) -> Result<String, String> {
    InsertOrReplaceRowToolCallHandler::new(app.clone())
        .execute_tool_call(InsertOrReplaceRowInputData {
            namespace: None,
            table_name: TABLE.to_string(),
            entity_json: entity_json.to_string(),
            schema_id: None,
        })
        .await
        .map(|result| result.status)
}

async fn rows(app: &Arc<AppContext>) -> Vec<String> {
    GetRowsToolCallHandler::new(app.clone())
        .execute_tool_call(GetRowsInputData {
            namespace: None,
            table_name: TABLE.to_string(),
            partition_key: None,
            row_key: None,
            skip: None,
            limit: None,
        })
        .await
        .unwrap()
        .rows
}

/// The gate is the whole of the protection here, so it is the first thing worth
/// holding: shut by default, and every write bounces off it.
#[tokio::test]
async fn every_write_is_refused_until_the_window_is_open() {
    let app = app_with_table().await;

    let err = insert(&app, r#"{"PartitionKey":"acc-1","RowKey":"rk-1"}"#)
        .await
        .unwrap_err();

    assert!(err.contains("SHUT"), "{err}");

    let err = DeleteRowToolCallHandler::new(app.clone())
        .execute_tool_call(DeleteRowInputData {
            namespace: None,
            table_name: TABLE.to_string(),
            partition_key: "acc-1".to_string(),
            row_key: "rk-1".to_string(),
        })
        .await
        .unwrap_err();

    assert!(err.contains("SHUT"), "{err}");

    let err = CleanTableToolCallHandler::new(app.clone())
        .execute_tool_call(CleanTableInputData {
            namespace: None,
            table_name: TABLE.to_string(),
        })
        .await
        .unwrap_err();

    assert!(err.contains("SHUT"), "{err}");

    // ...while reading is never gated.
    assert!(rows(&app).await.is_empty());
}

/// The round trip the whole surface is built around: JSON in, protobuf stored,
/// JSON back out.
#[tokio::test]
async fn a_row_written_as_json_comes_back_as_the_same_json() {
    let app = app_with_table().await;
    open_writes(&app);

    insert(
        &app,
        r#"{"PartitionKey":"acc-1","RowKey":"rk-1","Amount":12.5}"#,
    )
    .await
    .unwrap();

    let rows = rows(&app).await;

    assert_eq!(rows.len(), 1);
    // The server's own TimeStamp is added on the way out, so the row is matched
    // on the parts the caller sent.
    assert!(rows[0].contains(r#""PartitionKey":"acc-1""#), "{}", rows[0]);
    assert!(rows[0].contains(r#""RowKey":"rk-1""#), "{}", rows[0]);
    assert!(rows[0].contains(r#""Amount":12.5"#), "{}", rows[0]);
    assert!(rows[0].contains(r#""TimeStamp":"#), "{}", rows[0]);
}

/// A misspelled field must not become a row stored without that value.
#[tokio::test]
async fn a_field_the_schema_does_not_have_refuses_the_write() {
    let app = app_with_table().await;
    open_writes(&app);

    let err = insert(
        &app,
        r#"{"PartitionKey":"acc-1","RowKey":"rk-1","Amonut":12.5}"#,
    )
    .await
    .unwrap_err();

    assert!(err.contains("Amonut"), "{err}");
    assert!(rows(&app).await.is_empty(), "the row was written anyway");
}

/// A table nobody has written to has no shape to write through, and saying so
/// is the only honest answer: inventing one would put a schema under an id the
/// owning client folded out of its own type.
#[tokio::test]
async fn a_table_without_a_schema_can_not_be_written_to_from_here() {
    let app = Arc::new(AppContext::new(settings(&new_test_folder())));
    crate::operations::load_from_disk(app.clone()).await;

    let db_namespace = app
        .namespaces
        .get_or_create("", &app.settings)
        .await
        .unwrap();

    crate::db_operations::write::create_table(
        &db_namespace,
        TABLE,
        DbTableAttributes::create_default(),
        DateTimeAsMicroseconds::now(),
    )
    .unwrap();

    open_writes(&app);

    let err = insert(&app, r#"{"PartitionKey":"acc-1","RowKey":"rk-1"}"#)
        .await
        .unwrap_err();

    assert!(err.contains("never been written to"), "{err}");
}

/// Two versions in one table is a deploy in flight. Picking one would store the
/// row under a version nobody meant, and the row outlives the guess.
#[tokio::test]
async fn two_schemas_make_the_write_ask_which_one() {
    let app = app_with_table().await;
    open_writes(&app);

    let db_namespace = app.namespaces.get("").unwrap();
    let db_table = db_namespace.tables.get_table(TABLE).unwrap();
    db_table.register_schema(schema_v2());

    let err = insert(&app, r#"{"PartitionKey":"acc-1","RowKey":"rk-1"}"#)
        .await
        .unwrap_err();

    assert!(err.contains("more than one entity version"), "{err}");

    // Naming one gets the write through, and the answer says which was used.
    let written = InsertOrReplaceRowToolCallHandler::new(app.clone())
        .execute_tool_call(InsertOrReplaceRowInputData {
            namespace: None,
            table_name: TABLE.to_string(),
            entity_json: r#"{"PartitionKey":"acc-1","RowKey":"rk-1","Level":3}"#.to_string(),
            schema_id: Some(schema_v2().id),
        })
        .await
        .unwrap();

    assert_eq!(written.schema_id, schema_v2().id);
    assert!(rows(&app).await[0].contains(r#""Level":3"#));
}

/// The window is what makes a write possible, so a wide delete is one call and
/// the count is how many rows were actually there - not how many were named.
#[tokio::test]
async fn a_bulk_delete_spans_partitions_and_counts_what_was_there() {
    let app = app_with_table().await;
    open_writes(&app);

    for (partition_key, row_key) in [("acc-1", "rk-1"), ("acc-1", "rk-2"), ("acc-2", "rk-3")] {
        insert(
            &app,
            &format!(r#"{{"PartitionKey":"{partition_key}","RowKey":"{row_key}"}}"#),
        )
        .await
        .unwrap();
    }

    let deleted = BulkDeleteRowsToolCallHandler::new(app.clone())
        .execute_tool_call(BulkDeleteRowsInputData {
            namespace: None,
            table_name: TABLE.to_string(),
            rows_json: r#"{"acc-1":["rk-1","rk-9"],"acc-2":["rk-3"]}"#.to_string(),
        })
        .await
        .unwrap();

    // Three keys named, two of them were there.
    assert_eq!(deleted.rows_deleted, 2);
    assert_eq!(rows(&app).await.len(), 1);
}

/// Paging is what keeps a large table out of a context window, and `has_more` is
/// the only way a caller knows to ask again.
#[tokio::test]
async fn rows_come_back_a_page_at_a_time() {
    let app = app_with_table().await;
    open_writes(&app);

    for no in 0..5 {
        insert(
            &app,
            &format!(r#"{{"PartitionKey":"acc-1","RowKey":"rk-{no}"}}"#),
        )
        .await
        .unwrap();
    }

    let page = GetRowsToolCallHandler::new(app.clone())
        .execute_tool_call(GetRowsInputData {
            namespace: None,
            table_name: TABLE.to_string(),
            partition_key: None,
            row_key: None,
            skip: None,
            limit: Some(2),
        })
        .await
        .unwrap();

    assert_eq!(page.count, 2);
    assert!(page.has_more);

    let last = GetRowsToolCallHandler::new(app.clone())
        .execute_tool_call(GetRowsInputData {
            namespace: None,
            table_name: TABLE.to_string(),
            partition_key: None,
            row_key: None,
            skip: Some(3),
            limit: Some(2),
        })
        .await
        .unwrap();

    assert_eq!(last.count, 2);
    assert!(!last.has_more);
}

/// Looking at a row must not rescue it from eviction: an agent browsing a table
/// would otherwise keep alive exactly the cold rows somebody went to look at.
#[tokio::test]
async fn reading_rows_does_not_move_the_last_read_mark() {
    let app = app_with_table().await;
    open_writes(&app);

    insert(&app, r#"{"PartitionKey":"acc-1","RowKey":"rk-1"}"#)
        .await
        .unwrap();

    let db_namespace = app.namespaces.get("").unwrap();
    let db_table = db_namespace.tables.get_table(TABLE).unwrap();

    let before = last_read(&db_table);
    rows(&app).await;
    let after = last_read(&db_table);

    assert_eq!(before, after);
}

fn last_read(db_table: &DbTable) -> DateTimeAsMicroseconds {
    match db_table.get_row_statistics("acc-1", "rk-1") {
        GetRowStatisticsResult::Found(statistics) => statistics.row_last_read_access,
        _ => panic!("the row is not there"),
    }
}

/// The four backup tools are one path - list, tables, partitions, rows - and the
/// last of them is the one worth proving: a snapshot carries the schemas with the
/// table, so a partition inside an archive is readable even when no live table
/// holds that shape any more.
#[tokio::test]
async fn a_backup_is_walked_into_and_its_rows_are_shown_through_the_snapshot_schema() {
    let folder = new_test_folder();
    let backups = format!("{folder}-backups");
    let _ = std::fs::remove_dir_all(&backups);

    let app =
        app_with_table_and_settings(settings_with_backups(&folder, Some(backups.clone()))).await;
    open_writes(&app);

    insert(
        &app,
        r#"{"PartitionKey":"acc-1","RowKey":"rk-1","Amount":12.5}"#,
    )
    .await
    .unwrap();

    crate::db_operations::backup::make(&app, DateTimeAsMicroseconds::now())
        .await
        .unwrap();

    let listed = GetListOfBackupsToolCallHandler::new(app.clone())
        .execute_tool_call(GetListOfBackupsInputData { namespace: None })
        .await
        .unwrap();

    assert_eq!(listed.count, 1);
    let file_name = listed.files[0].file_name.clone();

    let tables = GetBackupTablesToolCallHandler::new(app.clone())
        .execute_tool_call(GetBackupTablesInputData {
            namespace: None,
            file_name: file_name.clone(),
        })
        .await
        .unwrap();

    assert_eq!(tables.tables[0].name, TABLE);
    assert_eq!(tables.tables[0].partitions_count, 1);
    // The schema rode along with the table's attributes; without it the rows
    // below would come back numbered.
    assert_eq!(tables.tables[0].schemas_count, 1);

    let partitions = GetBackupPartitionsToolCallHandler::new(app.clone())
        .execute_tool_call(GetBackupPartitionsInputData {
            namespace: None,
            file_name: file_name.clone(),
            table_name: TABLE.to_string(),
        })
        .await
        .unwrap();

    assert_eq!(partitions.partitions, vec!["acc-1".to_string()]);

    let backed_up = GetBackupRowsToolCallHandler::new(app.clone())
        .execute_tool_call(GetBackupRowsInputData {
            namespace: None,
            file_name: file_name.clone(),
            table_name: TABLE.to_string(),
            partition_key: "acc-1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(backed_up.count, 1);
    assert!(
        backed_up.rows[0].contains(r#""Amount":12.5"#),
        "{:?}",
        backed_up.rows
    );

    // ...and the archive puts back what the table has since lost.
    CleanTableToolCallHandler::new(app.clone())
        .execute_tool_call(CleanTableInputData {
            namespace: None,
            table_name: TABLE.to_string(),
        })
        .await
        .unwrap();

    assert!(rows(&app).await.is_empty());

    let restored = RestoreBackupToolCallHandler::new(app.clone())
        .execute_tool_call(RestoreBackupInputData {
            namespace: None,
            file_name,
            table_name: None,
            partition_key: None,
        })
        .await
        .unwrap();

    assert_eq!(restored.partitions_restored, 1);
    assert_eq!(rows(&app).await.len(), 1);

    let _ = std::fs::remove_dir_all(&backups);
    let _ = std::fs::remove_dir_all(&folder);
}

/// Half a name is a request nobody can act on: naming a table without a
/// partition reads as "restore this table", which is not what the call does.
#[tokio::test]
async fn restoring_one_partition_takes_both_halves_of_its_name() {
    let app = app_with_table().await;
    open_writes(&app);

    let err = RestoreBackupToolCallHandler::new(app.clone())
        .execute_tool_call(RestoreBackupInputData {
            namespace: None,
            file_name: "whatever.zip".to_string(),
            table_name: Some(TABLE.to_string()),
            partition_key: None,
        })
        .await
        .unwrap_err();

    assert!(err.contains("partition_key"), "{err}");
}

/// A namespace nobody has written to is a mistake worth hearing about, and
/// resolving it into being would leave a folder behind every typo.
#[tokio::test]
async fn a_namespace_which_is_not_there_is_named_in_the_answer() {
    let app = app_with_table().await;

    let err = GetListOfTablesToolCallHandler::new(app.clone())
        .execute_tool_call(GetListOfTablesInputData {
            namespace: Some("prod".to_string()),
        })
        .await
        .unwrap_err();

    assert!(err.contains("prod"), "{err}");
    assert!(err.contains("get_namespaces"), "{err}");
}

/// Every tool the middleware is handed, listed once. Nothing but this holds the
/// names together: they live in a `const` on each handler, and a rename is
/// invisible until a client asks for a tool which is no longer there.
#[test]
fn the_registered_tools_are_the_ones_that_were_declared() {
    use mcp_server_middleware::ToolDefinition;

    let names = [
        GetNamespacesToolCallHandler::FUNC_NAME,
        GetListOfTablesToolCallHandler::FUNC_NAME,
        GetRowsToolCallHandler::FUNC_NAME,
        GetListOfBackupsToolCallHandler::FUNC_NAME,
        GetBackupTablesToolCallHandler::FUNC_NAME,
        GetBackupPartitionsToolCallHandler::FUNC_NAME,
        GetBackupRowsToolCallHandler::FUNC_NAME,
        InsertOrReplaceRowToolCallHandler::FUNC_NAME,
        BulkInsertOrReplaceRowsToolCallHandler::FUNC_NAME,
        DeleteRowToolCallHandler::FUNC_NAME,
        BulkDeleteRowsToolCallHandler::FUNC_NAME,
        DeletePartitionsToolCallHandler::FUNC_NAME,
        CleanTableToolCallHandler::FUNC_NAME,
        MoveTableToNamespaceToolCallHandler::FUNC_NAME,
        RestoreBackupToolCallHandler::FUNC_NAME,
    ];

    assert_eq!(
        names.to_vec(),
        vec![
            "get_namespaces",
            "get_list_of_tables",
            "get_rows",
            "get_list_of_backups",
            "get_backup_tables",
            "get_backup_partitions",
            "get_backup_rows",
            "insert_or_replace_row",
            "bulk_insert_or_replace_rows",
            "delete_row",
            "bulk_delete_rows",
            "delete_partitions",
            "clean_table",
            "move_table_to_namespace",
            "restore_backup",
        ]
    );
}

/// The window shuts by itself, and `remaining` is what says so.
#[test]
fn the_write_window_closes_on_its_own() {
    let app = AppContext::new(settings("/tmp/mcp-window-test"));

    let now = DateTimeAsMicroseconds::now();
    assert!(!app.mcp_writes_are_open(now));

    app.open_mcp_writes(now);
    assert_eq!(
        app.mcp_writes_remaining_secs(now),
        Some(crate::app::MCP_WRITES_WINDOW_SECS)
    );

    let mut later = now;
    later.add_seconds(crate::app::MCP_WRITES_WINDOW_SECS - 1);
    assert!(app.mcp_writes_are_open(later));

    let mut past = now;
    past.add_seconds(crate::app::MCP_WRITES_WINDOW_SECS);
    assert!(!app.mcp_writes_are_open(past));

    // ...and shutting it is immediate.
    app.open_mcp_writes(now);
    app.close_mcp_writes();
    assert!(!app.mcp_writes_are_open(now));
}
