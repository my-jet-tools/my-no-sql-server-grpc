//! Drives the gRPC handlers themselves - mappers, db_operations, persist
//! markers and the slotted-page backend all included - and then throws the whole
//! `AppContext` away and builds a new one over the same folder. What the second
//! one answers came off the disk.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use my_no_sql_grpc_core::db_entity::{ParsedEntity, consts, write_varint};
use tonic::Request;

use crate::app::AppContext;
use crate::grpc_server::WriterGrpcService;
use crate::my_no_sql_writer_grpc::writer_server::Writer;
use crate::my_no_sql_writer_grpc::*;
use crate::settings_reader::SettingsModel;

static FOLDER_NO: AtomicU64 = AtomicU64::new(0);

const TABLE: &str = "traders";
const SCHEMA_ID: u64 = 0x0102_0304_0506_0708;

/// Most of these tests never show a row, but the schema they carry still has to
/// be one: the write path reads it back before it keeps it, because bytes that
/// can not be read back could never show anything and would sit in
/// `tables.meta` outliving every restart.
fn schema_bytes() -> Vec<u8> {
    schema_bytes_of("TraderEntity")
}

/// The same shape under another root name - a different schema by every measure
/// that matters here, and a valid one.
fn schema_bytes_of(root_message_name: &str) -> Vec<u8> {
    use my_no_sql_grpc_core::schemas::{Field, ItemType, Message, Scalar, Schema, Tp};

    let string = Tp::Item(ItemType::Scalar(Scalar::String));

    let field = |no: u32, name: &str| Field {
        no,
        name: name.to_string(),
        tp: string,
    };

    Schema {
        root: 0,
        messages: vec![Message {
            name: root_message_name.to_string(),
            fields: vec![
                field(1, "PartitionKey"),
                field(2, "RowKey"),
                field(5, "Payload"),
            ],
        }],
    }
    .serialize()
}

fn new_test_folder() -> String {
    let folder = std::env::temp_dir().join(format!(
        "my-no-sql-grpc-e2e-{}-{}",
        std::process::id(),
        FOLDER_NO.fetch_add(1, Ordering::SeqCst)
    ));

    let _ = std::fs::remove_dir_all(&folder);
    folder.to_string_lossy().to_string()
}

fn settings(folder: &str) -> Arc<SettingsModel> {
    Arc::new(SettingsModel {
        persistence_dest: folder.to_string(),
        location: "test".to_string(),
        compress_data: true,
        skip_broken_partitions: false,
        backups_dest: None,
        backup_interval_secs: None,
        max_backups: None,
        api_key: None,
    })
}

/// Starts a server over `folder` and loads whatever is already there.
async fn start(folder: &str) -> WriterGrpcService {
    let app = Arc::new(AppContext::new(settings(folder)));
    crate::operations::load_from_disk(app.clone()).await;
    WriterGrpcService::new(app)
}

/// Writes everything queued, exactly as the shutdown path does.
async fn drain_persist(service: &WriterGrpcService) {
    while crate::operations::persist(&service.app, None).await {}
}

fn entity(partition_key: &str, row_key: &str, payload: &str) -> Vec<u8> {
    let mut result = Vec::new();

    for (field_no, value) in [(1u32, partition_key), (2, row_key), (5, payload)] {
        write_varint(
            &mut result,
            u64::from(field_no) << 3 | u64::from(consts::WIRE_TYPE_LEN),
        );
        write_varint(&mut result, value.len() as u64);
        result.extend_from_slice(value.as_bytes());
    }

    result
}

/// The same entity, carrying the client's own TimeStamp - which is what
/// `InsertOrReplaceIfNew` compares.
fn entity_with_time_stamp(partition_key: &str, row_key: &str, time_stamp: i64) -> Vec<u8> {
    with_time_stamp(entity(partition_key, row_key, "x"), time_stamp)
}

fn with_time_stamp(mut row: Vec<u8>, time_stamp: i64) -> Vec<u8> {
    write_varint(&mut row, 3 << 3 | u64::from(consts::WIRE_TYPE_VARINT));
    write_varint(&mut row, time_stamp as u64);

    row
}

fn schema() -> Option<EntitySchemaGrpcModel> {
    Some(EntitySchemaGrpcModel {
        schema_id: SCHEMA_ID,
        schema: schema_bytes(),
    })
}

fn write_request(partition_key: &str, row_key: &str, payload: &str) -> WriteRowGrpcRequest {
    WriteRowGrpcRequest {
        name_space: String::new(),
        table_name: TABLE.to_string(),
        schema: schema(),
        row: entity(partition_key, row_key, payload),
        sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        use_client_time_stamp: false,
    }
}

/// A `Replace` as a client makes it: the entity it read back, changed, still
/// carrying the TimeStamp it came with - that is the version being replaced, and
/// it is not the TimeStamp the row will end up with.
fn replace_request(
    partition_key: &str,
    row_key: &str,
    payload: &str,
    version: i64,
) -> WriteRowGrpcRequest {
    let mut request = write_request(partition_key, row_key, payload);
    request.row = with_time_stamp(entity(partition_key, row_key, payload), version);
    request
}

/// The version a stored row has right now - what a client reads before it
/// replaces it.
async fn stored_version(service: &WriterGrpcService, partition_key: &str, row_key: &str) -> i64 {
    let row = get_row(service, partition_key, row_key)
        .await
        .expect("the row has to be there to have a version");

    ParsedEntity::parse(&row)
        .unwrap()
        .time_stamp
        .expect("a stored row is always handed out with its TimeStamp")
}

async fn create_table(service: &WriterGrpcService) {
    service
        .create_table_if_not_exists(Request::new(CreateTableGrpcRequest {
            name_space: String::new(),
            table_name: TABLE.to_string(),
            attributes: Some(TableAttributesGrpcModel {
                persist: true,
                max_partitions_amount: None,
                max_rows_per_partition_amount: None,
            }),
            sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        }))
        .await
        .unwrap();
}

/// Switches the table's persistence on or off, leaving its limits alone.
async fn set_persist(service: &WriterGrpcService, persist: bool) {
    service
        .set_table_attributes(Request::new(SetTableAttributesGrpcRequest {
            name_space: String::new(),
            table_name: TABLE.to_string(),
            attributes: Some(TableAttributesGrpcModel {
                persist,
                max_partitions_amount: None,
                max_rows_per_partition_amount: None,
            }),
            sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        }))
        .await
        .unwrap();
}

fn bulk_message(mode: BulkWriteModeGrpcModel, rows: Vec<Vec<u8>>) -> BulkWriteGrpcRequest {
    BulkWriteGrpcRequest {
        name_space: String::new(),
        table_name: TABLE.to_string(),
        mode: mode as i32,
        sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        use_client_time_stamp: false,
        schema: schema(),
        rows,
    }
}

/// Drives the handler over a stream of already-built messages: the transport is
/// the one thing a bulk write does not care about.
async fn bulk_write(
    service: &WriterGrpcService,
    messages: Vec<BulkWriteGrpcRequest>,
) -> Result<(), tonic::Status> {
    service
        .apply_bulk_write(tokio_stream::iter(messages.into_iter().map(Ok)))
        .await
        .map(|_| ())
}

async fn get_row(
    service: &WriterGrpcService,
    partition_key: &str,
    row_key: &str,
) -> Option<Vec<u8>> {
    service
        .get_row(Request::new(GetRowGrpcRequest {
            name_space: String::new(),
            table_name: TABLE.to_string(),
            partition_key: partition_key.to_string(),
            row_key: row_key.to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .row
}

#[tokio::test]
async fn a_written_row_is_still_there_after_a_restart() {
    let folder = new_test_folder();

    {
        let service = start(&folder).await;
        create_table(&service).await;

        service
            .insert_or_replace(Request::new(write_request("acc-1", "eur-usd", "hello")))
            .await
            .unwrap();

        drain_persist(&service).await;
    }

    let service = start(&folder).await;

    let row = get_row(&service, "acc-1", "eur-usd")
        .await
        .expect("the row must survive the restart");

    // What comes back has to be a complete entity: the user's own field plus the
    // TimeStamp the server stamped.
    let parsed = ParsedEntity::parse(&row).unwrap();
    assert_eq!(parsed.get_partition_key(), "acc-1");
    assert_eq!(parsed.get_row_key(), "eur-usd");
    assert!(parsed.time_stamp.is_some());
    assert!(String::from_utf8_lossy(&row).contains("hello"));
}

#[tokio::test]
async fn the_table_and_its_schema_come_back_too() {
    let folder = new_test_folder();

    {
        let service = start(&folder).await;
        create_table(&service).await;
        service
            .insert_or_replace(Request::new(write_request("acc-1", "eur-usd", "hello")))
            .await
            .unwrap();
        drain_persist(&service).await;
    }

    let service = start(&folder).await;

    let db_namespace = service.app.namespaces.get("").unwrap();

    assert!(db_namespace.tables.has_table(TABLE));
    assert!(
        db_namespace
            .tables
            .get_table(TABLE)
            .unwrap()
            .get_attributes()
            .persist
    );

    // The schema travelled with the write and came back inside the metadata of
    // the very table whose rows were written under it.
    let restored = db_namespace
        .tables
        .get_table(TABLE)
        .unwrap()
        .get_schema(SCHEMA_ID)
        .unwrap();
    assert_eq!(restored.schema, schema_bytes());
}

#[tokio::test]
async fn a_deleted_row_stays_deleted_after_a_restart() {
    let folder = new_test_folder();

    {
        let service = start(&folder).await;
        create_table(&service).await;

        service
            .insert_or_replace(Request::new(write_request("acc-1", "keep", "x")))
            .await
            .unwrap();
        service
            .insert_or_replace(Request::new(write_request("acc-1", "drop", "x")))
            .await
            .unwrap();
        drain_persist(&service).await;

        service
            .delete_row(Request::new(DeleteRowGrpcRequest {
                name_space: String::new(),
                table_name: TABLE.to_string(),
                partition_key: "acc-1".to_string(),
                row_key: "drop".to_string(),
                sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
            }))
            .await
            .unwrap();
        drain_persist(&service).await;
    }

    let service = start(&folder).await;

    assert!(get_row(&service, "acc-1", "keep").await.is_some());
    assert!(get_row(&service, "acc-1", "drop").await.is_none());
}

/// Emptying a partition has to free its slot, not leave an empty one behind that
/// the next start would load as a ghost partition.
#[tokio::test]
async fn emptying_a_partition_removes_it_from_disk() {
    let folder = new_test_folder();

    {
        let service = start(&folder).await;
        create_table(&service).await;

        service
            .insert_or_replace(Request::new(write_request("gone", "rk", "x")))
            .await
            .unwrap();
        drain_persist(&service).await;

        service
            .delete_row(Request::new(DeleteRowGrpcRequest {
                name_space: String::new(),
                table_name: TABLE.to_string(),
                partition_key: "gone".to_string(),
                row_key: "rk".to_string(),
                sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
            }))
            .await
            .unwrap();
        drain_persist(&service).await;
    }

    let service = start(&folder).await;
    let db_namespace = service.app.namespaces.get("").unwrap();
    let db_table = db_namespace.tables.get_table(TABLE).unwrap();

    assert_eq!(db_table.get_partitions_amount(), 0);
}

#[tokio::test]
async fn insert_refuses_to_overwrite_and_replace_refuses_to_create() {
    let folder = new_test_folder();
    let service = start(&folder).await;
    create_table(&service).await;

    // Nothing stored yet: replace has nothing to replace. It carries a version
    // all the same, because without one the call is refused before it gets as
    // far as looking.
    let status = service
        .replace(Request::new(replace_request("acc-1", "rk", "x", 1)))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);

    service
        .insert(Request::new(write_request("acc-1", "rk", "first")))
        .await
        .unwrap();

    // Now it is there: insert must not overwrite it.
    let status = service
        .insert(Request::new(write_request("acc-1", "rk", "second")))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::AlreadyExists);

    let version = stored_version(&service, "acc-1", "rk").await;

    service
        .replace(Request::new(replace_request(
            "acc-1", "rk", "second", version,
        )))
        .await
        .unwrap();

    let row = get_row(&service, "acc-1", "rk").await.unwrap();
    assert!(String::from_utf8_lossy(&row).contains("second"));
}

/// What `Replace` is for: two writers holding the same read do not both win, and
/// the one who lost is told so. Without the check both are answered `Ok` and one
/// of the two edits is gone with nobody anywhere hearing about it.
#[tokio::test]
async fn replace_refuses_a_row_which_was_rewritten_since_it_was_read() {
    let folder = new_test_folder();
    let service = start(&folder).await;
    create_table(&service).await;

    service
        .insert_or_replace(Request::new(write_request("acc-1", "rk", "first")))
        .await
        .unwrap();

    // The version both of them read.
    let version = stored_version(&service, "acc-1", "rk").await;

    service
        .replace(Request::new(replace_request(
            "acc-1", "rk", "mine", version,
        )))
        .await
        .unwrap();

    let status = service
        .replace(Request::new(replace_request(
            "acc-1", "rk", "theirs", version,
        )))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Aborted);

    let row = get_row(&service, "acc-1", "rk").await.unwrap();
    assert!(String::from_utf8_lossy(&row).contains("mine"));

    // And reading again is all it takes for the same write to land - which is
    // what makes the retry loop end rather than spin.
    let version = stored_version(&service, "acc-1", "rk").await;

    service
        .replace(Request::new(replace_request(
            "acc-1", "rk", "theirs", version,
        )))
        .await
        .unwrap();

    let row = get_row(&service, "acc-1", "rk").await.unwrap();
    assert!(String::from_utf8_lossy(&row).contains("theirs"));
}

/// An entity with no TimeStamp names no version, so there is nothing to check.
/// Answering `Ok` to it would say the row was replaced under a check nobody made.
#[tokio::test]
async fn replace_without_a_version_is_refused() {
    let folder = new_test_folder();
    let service = start(&folder).await;
    create_table(&service).await;

    service
        .insert_or_replace(Request::new(write_request("acc-1", "rk", "first")))
        .await
        .unwrap();

    let status = service
        .replace(Request::new(write_request("acc-1", "rk", "second")))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    let row = get_row(&service, "acc-1", "rk").await.unwrap();
    assert!(String::from_utf8_lossy(&row).contains("first"));
}

/// The row that lands carries the server's clock, not the version it was based
/// on: a row that kept the version it replaced would let the next writer holding
/// the same read through as well, and the check would pass twice for one version.
#[tokio::test]
async fn a_replaced_row_gets_a_version_of_its_own() {
    let folder = new_test_folder();
    let service = start(&folder).await;
    create_table(&service).await;

    service
        .insert_or_replace(Request::new(write_request("acc-1", "rk", "first")))
        .await
        .unwrap();

    let version = stored_version(&service, "acc-1", "rk").await;

    let mut request = replace_request("acc-1", "rk", "second", version);
    // Even asked for outright, the client's TimeStamp is not what is stored.
    request.use_client_time_stamp = true;

    service.replace(Request::new(request)).await.unwrap();

    assert_ne!(stored_version(&service, "acc-1", "rk").await, version);
}

/// Deleting a key which is not there is not a failure: `BulkDelete` answers the
/// same input the same way, and a cleanup loop over keys - half of which have
/// expired by then - must not stop on the first of them.
#[tokio::test]
async fn deleting_a_row_which_is_not_there_is_not_an_error() {
    let folder = new_test_folder();
    let service = start(&folder).await;
    create_table(&service).await;

    let response = service
        .delete_row(Request::new(DeleteRowGrpcRequest {
            name_space: String::new(),
            table_name: TABLE.to_string(),
            partition_key: "acc-1".to_string(),
            row_key: "never-was".to_string(),
            sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(!response.deleted);

    service
        .insert_or_replace(Request::new(write_request("acc-1", "rk", "x")))
        .await
        .unwrap();

    let response = service
        .delete_row(Request::new(DeleteRowGrpcRequest {
            name_space: String::new(),
            table_name: TABLE.to_string(),
            partition_key: "acc-1".to_string(),
            row_key: "rk".to_string(),
            sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(response.deleted);
}

#[tokio::test]
async fn a_write_to_a_table_which_does_not_exist_is_refused() {
    let folder = new_test_folder();
    let service = start(&folder).await;

    let status = service
        .insert_or_replace(Request::new(write_request("acc-1", "rk", "x")))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn a_row_without_a_partition_key_is_refused() {
    let folder = new_test_folder();
    let service = start(&folder).await;
    create_table(&service).await;

    let mut request = write_request("acc-1", "rk", "x");
    // Only RowKey and a user field - proto3 would not put an empty PartitionKey
    // on the wire either, so this is what a broken entity actually looks like.
    request.row = {
        let full = entity("acc-1", "rk", "x");
        let mut reader = my_no_sql_grpc_core::db_entity::ProtobufReader::new(&full);
        let mut without_pk = Vec::new();

        while let Some(field) = reader.get_next().unwrap() {
            if field.field_no != consts::FIELD_PARTITION_KEY {
                without_pk.extend_from_slice(field.field.get_slice(&full));
            }
        }

        without_pk
    };

    let status = service
        .insert_or_replace(Request::new(request))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

/// The schema travels with the write, reaches the disk, and after a restart the
/// row still renders under its own field names - which is the whole point of
/// keeping a schema per row.
#[tokio::test]
async fn a_row_renders_through_its_schema_after_a_restart() {
    let folder = new_test_folder();

    let schema = crate::json_view::tests::build_schema();
    let schema_id = 0x00de_ad00_beef_0000;

    {
        let service = start(&folder).await;
        create_table(&service).await;

        let mut request = write_request("acc-1", "eur-usd", "ignored");
        request.schema = Some(EntitySchemaGrpcModel {
            schema_id,
            schema: schema.clone(),
        });
        request.row = crate::json_view::tests::build_row();

        service
            .insert_or_replace(Request::new(request))
            .await
            .unwrap();

        drain_persist(&service).await;
    }

    let service = start(&folder).await;

    let db_namespace = service.app.namespaces.get("").unwrap();
    let stored_schema = db_namespace
        .tables
        .get_table(TABLE)
        .unwrap()
        .get_schema(schema_id)
        .expect("the schema must survive the restart");

    // Byte for byte what was written: the id names these bytes and nothing else,
    // and a schema that came back changed would show every row of this table
    // under names nobody declared.
    assert_eq!(stored_schema.schema, schema);

    let index = service
        .app
        .json_schemas
        .get_or_build(&stored_schema)
        .expect("the restored schema must still resolve");

    let db_row = db_namespace
        .tables
        .get_table(TABLE)
        .unwrap()
        .get_row("acc-1", "eur-usd")
        .unwrap();

    let json = crate::json_view::write_row_as_json(
        my_json::json_writer::JsonObjectWriter::new(),
        &db_row.to_vec(),
        Some(&index),
    )
    .build();

    assert!(json.contains(r#""PartitionKey":"acc-1""#), "{json}");
    assert!(json.contains(r#""Amount":1.5"#), "{json}");
    assert!(json.contains(r#""Limits":{"MaxLots":2.5"#), "{json}");
    // TimeStamp is rendered as a date rather than as raw microseconds, and the
    // schema never described it - the server owns that name.
    assert!(json.contains(r#""TimeStamp":"2"#), "{json}");
}

/// The one check the server does make about an id it is handed, end to end: the
/// bytes behind a known id are the bytes it is known by. It is what stops a
/// second client, written from the published proto, from putting its own number
/// on its own shape - which would show this table's rows through that shape for
/// as long as they live.
#[tokio::test]
async fn a_second_schema_under_a_known_id_is_refused() {
    let service = start(&new_test_folder()).await;
    create_table(&service).await;

    service
        .insert_or_replace(Request::new(write_request("acc-1", "eur-usd", "hello")))
        .await
        .unwrap();

    let mut request = write_request("acc-1", "gbp-usd", "hello");
    request.schema = Some(EntitySchemaGrpcModel {
        schema_id: SCHEMA_ID,
        schema: schema_bytes_of("SomebodyElsesEntity"),
    });

    let status = service
        .insert_or_replace(Request::new(request))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // And the row it came with is not there: the write was refused, not
    // half-applied.
    assert!(
        service
            .app
            .namespaces
            .get("")
            .unwrap()
            .tables
            .get_table(TABLE)
            .unwrap()
            .get_row("acc-1", "gbp-usd")
            .is_none()
    );
}

/// A table under a client which is being redeployed holds two entity versions
/// for as long as the old rows live, and one version after that. Without a
/// collector the second one would simply stay - and a table recreated a hundred
/// times would carry a hundred dead shapes, loaded at every start.
///
/// Driven through the real handlers and then through a restart, because the
/// point is that the drop reaches the disk: the schemas live in the table's
/// metadata, so removing one is a metadata write like any other.
#[tokio::test]
async fn a_schema_no_row_names_any_more_is_collected_and_stays_collected() {
    let folder = new_test_folder();

    const OLD_SCHEMA_ID: u64 = 0x1111_2222_3333_4444;

    {
        let service = start(&folder).await;
        create_table(&service).await;

        let mut old = write_request("acc-1", "old", "hello");
        old.schema = Some(EntitySchemaGrpcModel {
            schema_id: OLD_SCHEMA_ID,
            schema: schema_bytes_of("TraderEntityV1"),
        });

        service.insert_or_replace(Request::new(old)).await.unwrap();
        service
            .insert_or_replace(Request::new(write_request("acc-1", "new", "hello")))
            .await
            .unwrap();

        let db_namespace = service.app.namespaces.get("").unwrap();
        let db_table = db_namespace.tables.get_table(TABLE).unwrap();

        // Both versions are live, and that is the state the counter in
        // `/api/Status` exists to show.
        assert_eq!(db_table.get_attributes().schemas.len(), 2);
        assert!(!crate::db_operations::gc::collect_schemas(
            &db_namespace,
            &db_table,
            rust_extensions::date_time::DateTimeAsMicroseconds::now(),
        ));

        // The last row of the old version goes.
        service
            .delete_row(Request::new(DeleteRowGrpcRequest {
                name_space: String::new(),
                table_name: TABLE.to_string(),
                partition_key: "acc-1".to_string(),
                row_key: "old".to_string(),
                sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
            }))
            .await
            .unwrap();

        assert!(crate::db_operations::gc::collect_schemas(
            &db_namespace,
            &db_table,
            rust_extensions::date_time::DateTimeAsMicroseconds::now(),
        ));

        assert_eq!(db_table.get_attributes().schemas.len(), 1);
        assert!(db_table.get_schema(SCHEMA_ID).is_some());

        drain_persist(&service).await;
    }

    let service = start(&folder).await;
    let db_table = service
        .app
        .namespaces
        .get("")
        .unwrap()
        .tables
        .get_table(TABLE)
        .unwrap();

    // The one that is still used came back, and the dead one did not: a collect
    // which only happened in memory would show up here as two again.
    assert!(db_table.get_schema(OLD_SCHEMA_ID).is_none());
    assert_eq!(
        db_table.get_schema(SCHEMA_ID).unwrap().schema,
        schema_bytes()
    );
}

// ---- reader ----------------------------------------------------------------

use crate::grpc_server::ReaderGrpcService;
use crate::my_no_sql_reader_grpc::reader_server::Reader;
use crate::my_no_sql_reader_grpc::{
    GetChangeGrpcRequest, GreetingGrpcRequest, SubscribeGrpcRequest,
};

async fn greet(reader: &ReaderGrpcService) -> String {
    reader
        .greeting(Request::new(GreetingGrpcRequest {
            app_name: "test-reader".to_string(),
            version: "1.0.0".to_string(),
            name_space: String::new(),
        }))
        .await
        .unwrap()
        .into_inner()
        .session_id
}

/// Collects the snapshot `Subscribe` streams back, as row keys.
async fn subscribe(reader: &ReaderGrpcService, session_id: &str) -> Vec<String> {
    use tokio_stream::StreamExt;

    let mut stream = reader
        .subscribe(Request::new(SubscribeGrpcRequest {
            session_id: session_id.to_string(),
            table_name: TABLE.to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    let mut result = Vec::new();

    while let Some(chunk) = stream.next().await {
        for row in chunk.unwrap().rows {
            result.push(ParsedEntity::parse(&row).unwrap().get_row_key().to_string());
        }
    }

    result
}

async fn get_change(
    reader: &ReaderGrpcService,
    session_id: &str,
) -> crate::my_no_sql_reader_grpc::GetChangeGrpcResponse {
    reader
        .get_change(Request::new(GetChangeGrpcRequest {
            session_id: session_id.to_string(),
            read_statistics: Vec::new(),
        }))
        .await
        .unwrap()
        .into_inner()
}

#[tokio::test]
async fn a_reader_gets_the_snapshot_and_then_every_change() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    writer
        .insert_or_replace(Request::new(write_request("acc-1", "before", "x")))
        .await
        .unwrap();

    let reader = ReaderGrpcService::new(writer.app.clone());
    let session_id = greet(&reader).await;

    // The snapshot holds what was written before the subscription.
    assert_eq!(subscribe(&reader, &session_id).await, vec!["before"]);

    // ...and what is written after it arrives through the queue.
    writer
        .insert_or_replace(Request::new(write_request("acc-1", "after", "x")))
        .await
        .unwrap();

    let change = get_change(&reader, &session_id).await;
    let update = change.update_rows.expect("UpdateRows must be delivered");
    assert_eq!(update.table_name, TABLE);
    assert_eq!(
        ParsedEntity::parse(&update.rows[0]).unwrap().get_row_key(),
        "after"
    );

    // The batch is closed by its End - that is when the reader applies it.
    let change = get_change(&reader, &session_id).await;
    assert!(change.update_rows_end.is_some());
    assert!(change.update_rows.is_none());

    writer
        .delete_row(Request::new(DeleteRowGrpcRequest {
            name_space: String::new(),
            table_name: TABLE.to_string(),
            partition_key: "acc-1".to_string(),
            row_key: "after".to_string(),
            sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        }))
        .await
        .unwrap();

    let change = get_change(&reader, &session_id).await;
    let deleted = change.delete_rows.expect("DeleteRows must be delivered");
    assert_eq!(deleted.partitions[0].partition_key, "acc-1");
    assert_eq!(deleted.partitions[0].row_keys, vec!["after".to_string()]);

    let change = get_change(&reader, &session_id).await;
    assert!(change.delete_rows_end.is_some());
}

// ---- bulk write ------------------------------------------------------------

#[tokio::test]
async fn a_bulk_write_lands_whole_and_survives_a_restart() {
    let folder = new_test_folder();

    {
        let service = start(&folder).await;
        create_table(&service).await;

        bulk_write(
            &service,
            vec![
                bulk_message(
                    BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
                    vec![entity("acc-1", "a", "x"), entity("acc-1", "b", "x")],
                ),
                // The second message of the same batch, with the same header.
                bulk_message(
                    BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
                    vec![entity("acc-2", "a", "x")],
                ),
            ],
        )
        .await
        .unwrap();

        drain_persist(&service).await;
    }

    let service = start(&folder).await;

    assert!(get_row(&service, "acc-1", "a").await.is_some());
    assert!(get_row(&service, "acc-1", "b").await.is_some());
    assert!(get_row(&service, "acc-2", "a").await.is_some());
}

/// The whole point of accumulating the stream: however many messages carried the
/// batch, the subscriber is told about it once.
#[tokio::test]
async fn a_bulk_write_reaches_a_subscriber_as_one_batch() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    let reader = ReaderGrpcService::new(writer.app.clone());
    let session_id = greet(&reader).await;
    assert!(subscribe(&reader, &session_id).await.is_empty());

    bulk_write(
        &writer,
        vec![
            bulk_message(
                BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
                vec![entity("acc-1", "a", "x"), entity("acc-1", "b", "x")],
            ),
            bulk_message(
                BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
                vec![entity("acc-2", "a", "x")],
            ),
        ],
    )
    .await
    .unwrap();

    let change = get_change(&reader, &session_id).await;
    let update = change.update_rows.expect("UpdateRows must be delivered");
    assert_eq!(update.rows.len(), 3);

    assert!(
        get_change(&reader, &session_id)
            .await
            .update_rows_end
            .is_some()
    );

    // ...and nothing else: two messages did not become two batches. A write of
    // its own is what comes next out of the queue, which it could not if a
    // leftover chunk of the batch were still sitting in front of it.
    writer
        .insert_or_replace(Request::new(write_request("acc-3", "later", "x")))
        .await
        .unwrap();

    let change = get_change(&reader, &session_id).await;
    let update = change.update_rows.expect("UpdateRows must be delivered");
    assert_eq!(update.rows.len(), 1);
    assert_eq!(
        ParsedEntity::parse(&update.rows[0]).unwrap().get_row_key(),
        "later"
    );
}

#[tokio::test]
async fn clean_partitions_and_insert_replaces_the_named_partitions_only() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    bulk_write(
        &writer,
        vec![bulk_message(
            BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
            vec![entity("acc-1", "old", "x"), entity("acc-2", "keep", "x")],
        )],
    )
    .await
    .unwrap();

    let reader = ReaderGrpcService::new(writer.app.clone());
    let session_id = greet(&reader).await;
    assert_eq!(subscribe(&reader, &session_id).await, vec!["old", "keep"]);

    bulk_write(
        &writer,
        vec![bulk_message(
            BulkWriteModeGrpcModel::BulkWriteCleanPartitionsAndInsert,
            vec![entity("acc-1", "new", "x")],
        )],
    )
    .await
    .unwrap();

    assert!(get_row(&writer, "acc-1", "old").await.is_none());
    assert!(get_row(&writer, "acc-1", "new").await.is_some());
    assert!(get_row(&writer, "acc-2", "keep").await.is_some());

    // The reader is told what the partition now consists of, not what changed.
    let change = get_change(&reader, &session_id).await;
    let init = change
        .init_partitions
        .expect("InitPartitions must be delivered");
    assert_eq!(init.partitions.len(), 1);
    assert_eq!(init.partitions[0].partition_key, "acc-1");
    assert_eq!(
        ParsedEntity::parse(&init.partitions[0].rows[0])
            .unwrap()
            .get_row_key(),
        "new"
    );

    assert!(
        get_change(&reader, &session_id)
            .await
            .init_partitions_end
            .is_some()
    );
}

#[tokio::test]
async fn clean_table_and_insert_empties_the_table_before_it_writes() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    bulk_write(
        &writer,
        vec![bulk_message(
            BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
            vec![entity("acc-1", "old", "x")],
        )],
    )
    .await
    .unwrap();

    let reader = ReaderGrpcService::new(writer.app.clone());
    let session_id = greet(&reader).await;
    assert_eq!(subscribe(&reader, &session_id).await, vec!["old"]);

    bulk_write(
        &writer,
        vec![bulk_message(
            BulkWriteModeGrpcModel::BulkWriteCleanTableAndInsert,
            vec![entity("acc-2", "new", "x")],
        )],
    )
    .await
    .unwrap();

    assert!(get_row(&writer, "acc-1", "old").await.is_none());
    assert!(get_row(&writer, "acc-2", "new").await.is_some());

    // Cleaning first is an instruction of its own, and it has to arrive first.
    assert!(get_change(&reader, &session_id).await.clean_table.is_some());
    assert!(get_change(&reader, &session_id).await.update_rows.is_some());
    assert!(
        get_change(&reader, &session_id)
            .await
            .update_rows_end
            .is_some()
    );
}

/// A batch which brings no rows at all still means what its mode says.
#[tokio::test]
async fn a_clean_table_and_insert_with_no_rows_is_a_clean_table() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    bulk_write(
        &writer,
        vec![bulk_message(
            BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
            vec![entity("acc-1", "old", "x")],
        )],
    )
    .await
    .unwrap();

    bulk_write(
        &writer,
        vec![bulk_message(
            BulkWriteModeGrpcModel::BulkWriteCleanTableAndInsert,
            Vec::new(),
        )],
    )
    .await
    .unwrap();

    let db_table = writer
        .app
        .namespaces
        .get("")
        .unwrap()
        .tables
        .get_table(TABLE)
        .unwrap();

    assert_eq!(db_table.get_partitions_amount(), 0);
}

#[tokio::test]
async fn insert_or_replace_if_new_keeps_the_newer_stored_row() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    let mut message = bulk_message(
        BulkWriteModeGrpcModel::BulkWriteInsertOrReplaceIfNew,
        vec![entity_with_time_stamp("acc-1", "rk", 100)],
    );
    bulk_write(&writer, vec![message.clone()]).await.unwrap();

    let reader = ReaderGrpcService::new(writer.app.clone());
    let session_id = greet(&reader).await;
    assert_eq!(subscribe(&reader, &session_id).await, vec!["rk"]);

    // Older than what is stored: refused.
    message.rows = vec![entity_with_time_stamp("acc-1", "rk", 50)];
    bulk_write(&writer, vec![message.clone()]).await.unwrap();

    let stored = get_row(&writer, "acc-1", "rk").await.unwrap();
    assert_eq!(ParsedEntity::parse(&stored).unwrap().time_stamp, Some(100));

    // Newer: taken.
    message.rows = vec![entity_with_time_stamp("acc-1", "rk", 200)];
    bulk_write(&writer, vec![message]).await.unwrap();

    let stored = get_row(&writer, "acc-1", "rk").await.unwrap();
    assert_eq!(ParsedEntity::parse(&stored).unwrap().time_stamp, Some(200));

    // The refused row was never announced either - the first thing the queue
    // hands over is the row that won, not the one that lost. Announcing the
    // loser would push the subscriber's cache backwards in time.
    let change = get_change(&reader, &session_id).await;
    let update = change.update_rows.expect("UpdateRows must be delivered");
    assert_eq!(
        ParsedEntity::parse(&update.rows[0]).unwrap().time_stamp,
        Some(200)
    );
}

/// The header describes the batch, so a message which disagrees is a broken
/// client - and since nothing is written until the stream ends, refusing it
/// leaves the table exactly as it was.
#[tokio::test]
async fn a_message_which_disagrees_with_the_header_is_refused_and_writes_nothing() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    let mut second = bulk_message(
        BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
        vec![entity("acc-1", "b", "x")],
    );
    second.mode = BulkWriteModeGrpcModel::BulkWriteCleanTableAndInsert as i32;

    let status = bulk_write(
        &writer,
        vec![
            bulk_message(
                BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
                vec![entity("acc-1", "a", "x")],
            ),
            second,
        ],
    )
    .await
    .unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(get_row(&writer, "acc-1", "a").await.is_none());
}

#[tokio::test]
async fn a_bulk_write_with_a_broken_entity_writes_nothing() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    let status = bulk_write(
        &writer,
        vec![bulk_message(
            BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
            vec![entity("acc-1", "a", "x"), Vec::new()],
        )],
    )
    .await
    .unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(get_row(&writer, "acc-1", "a").await.is_none());
}

#[tokio::test]
async fn a_bulk_write_which_names_nothing_is_refused() {
    let folder = new_test_folder();
    let writer = start(&folder).await;

    let status = bulk_write(&writer, Vec::new()).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn a_bulk_write_to_a_table_which_does_not_exist_is_refused() {
    let folder = new_test_folder();
    let writer = start(&folder).await;

    let status = bulk_write(
        &writer,
        vec![bulk_message(
            BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
            vec![entity("acc-1", "a", "x")],
        )],
    )
    .await
    .unwrap_err();

    assert_eq!(status.code(), tonic::Code::NotFound);
}

// ---- transactions ----------------------------------------------------------

async fn start_transaction(service: &WriterGrpcService) -> String {
    service
        .start_transaction(Request::new(StartTransactionGrpcRequest {
            name_space: String::new(),
            table_name: TABLE.to_string(),
            sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        }))
        .await
        .unwrap()
        .into_inner()
        .transaction_id
}

fn action(transaction_id: &str) -> TransactionActionGrpcModel {
    TransactionActionGrpcModel {
        transaction_id: transaction_id.to_string(),
        clean_table: None,
        delete_partitions: None,
        delete_rows: None,
        insert_or_replace: None,
    }
}

fn clean_table_action(transaction_id: &str) -> TransactionActionGrpcModel {
    TransactionActionGrpcModel {
        clean_table: Some(TransactionCleanTableGrpcModel {}),
        ..action(transaction_id)
    }
}

fn delete_partitions_action(
    transaction_id: &str,
    partition_keys: &[&str],
) -> TransactionActionGrpcModel {
    TransactionActionGrpcModel {
        delete_partitions: Some(TransactionDeletePartitionsGrpcModel {
            partition_keys: partition_keys.iter().map(|itm| itm.to_string()).collect(),
        }),
        ..action(transaction_id)
    }
}

fn delete_rows_action(
    transaction_id: &str,
    partition_key: &str,
    row_keys: &[&str],
) -> TransactionActionGrpcModel {
    TransactionActionGrpcModel {
        delete_rows: Some(TransactionDeleteRowsGrpcModel {
            partition_key: partition_key.to_string(),
            row_keys: row_keys.iter().map(|itm| itm.to_string()).collect(),
        }),
        ..action(transaction_id)
    }
}

fn insert_action(transaction_id: &str, rows: Vec<Vec<u8>>) -> TransactionActionGrpcModel {
    TransactionActionGrpcModel {
        insert_or_replace: Some(TransactionInsertOrReplaceGrpcModel {
            schema: schema(),
            rows,
            use_client_time_stamp: false,
        }),
        ..action(transaction_id)
    }
}

async fn post_actions(
    service: &WriterGrpcService,
    actions: Vec<TransactionActionGrpcModel>,
) -> Result<(), tonic::Status> {
    service
        .apply_transaction_actions(tokio_stream::iter(actions.into_iter().map(Ok)))
        .await
        .map(|_| ())
}

async fn commit(service: &WriterGrpcService, transaction_id: &str) -> Result<(), tonic::Status> {
    service
        .commit_transaction(Request::new(TransactionGrpcRequest {
            transaction_id: transaction_id.to_string(),
        }))
        .await
        .map(|_| ())
}

/// Cleaning and inserting in one step is the whole point: the reader is told to
/// throw the partition away and what to put back, in that order and with nobody
/// else's chunks in between.
#[tokio::test]
async fn a_transaction_cleans_and_inserts_in_one_step() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    bulk_write(
        &writer,
        vec![bulk_message(
            BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
            vec![entity("acc-1", "old", "x"), entity("acc-2", "keep", "x")],
        )],
    )
    .await
    .unwrap();

    let reader = ReaderGrpcService::new(writer.app.clone());
    let session_id = greet(&reader).await;
    assert_eq!(subscribe(&reader, &session_id).await, vec!["old", "keep"]);

    let transaction_id = start_transaction(&writer).await;

    post_actions(
        &writer,
        vec![
            delete_partitions_action(&transaction_id, &["acc-1"]),
            insert_action(&transaction_id, vec![entity("acc-1", "new", "x")]),
        ],
    )
    .await
    .unwrap();

    // Nothing has reached the table yet - that is what the commit is for.
    assert!(get_row(&writer, "acc-1", "old").await.is_some());
    assert!(get_row(&writer, "acc-1", "new").await.is_none());

    commit(&writer, &transaction_id).await.unwrap();

    assert!(get_row(&writer, "acc-1", "old").await.is_none());
    assert!(get_row(&writer, "acc-1", "new").await.is_some());
    assert!(get_row(&writer, "acc-2", "keep").await.is_some());

    let change = get_change(&reader, &session_id).await;
    let cleaned = change
        .clean_partitions
        .expect("CleanPartitions must come first");
    assert_eq!(cleaned.partition_keys, vec!["acc-1".to_string()]);

    let change = get_change(&reader, &session_id).await;
    let update = change.update_rows.expect("...and the rows after it");
    assert_eq!(
        ParsedEntity::parse(&update.rows[0]).unwrap().get_row_key(),
        "new"
    );

    assert!(
        get_change(&reader, &session_id)
            .await
            .update_rows_end
            .is_some()
    );
}

/// The table is named once, at the start; the actions only ever name their keys.
#[tokio::test]
async fn a_transaction_resolves_its_table_through_the_transaction_id() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    let transaction_id = start_transaction(&writer).await;

    // Several posts, and a stream of several messages inside one of them.
    post_actions(
        &writer,
        vec![insert_action(
            &transaction_id,
            vec![entity("acc-1", "a", "x")],
        )],
    )
    .await
    .unwrap();

    post_actions(
        &writer,
        vec![
            insert_action(&transaction_id, vec![entity("acc-1", "b", "x")]),
            insert_action(&transaction_id, vec![entity("acc-2", "a", "x")]),
        ],
    )
    .await
    .unwrap();

    commit(&writer, &transaction_id).await.unwrap();

    assert!(get_row(&writer, "acc-1", "a").await.is_some());
    assert!(get_row(&writer, "acc-1", "b").await.is_some());
    assert!(get_row(&writer, "acc-2", "a").await.is_some());
}

#[tokio::test]
async fn a_committed_transaction_survives_a_restart() {
    let folder = new_test_folder();

    {
        let writer = start(&folder).await;
        create_table(&writer).await;

        bulk_write(
            &writer,
            vec![bulk_message(
                BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
                vec![entity("acc-1", "old", "x"), entity("acc-2", "gone", "x")],
            )],
        )
        .await
        .unwrap();
        drain_persist(&writer).await;

        let transaction_id = start_transaction(&writer).await;
        post_actions(
            &writer,
            vec![
                delete_partitions_action(&transaction_id, &["acc-2"]),
                delete_rows_action(&transaction_id, "acc-1", &["old"]),
                insert_action(&transaction_id, vec![entity("acc-1", "new", "x")]),
            ],
        )
        .await
        .unwrap();
        commit(&writer, &transaction_id).await.unwrap();

        drain_persist(&writer).await;
    }

    let writer = start(&folder).await;

    assert!(get_row(&writer, "acc-1", "new").await.is_some());
    assert!(get_row(&writer, "acc-1", "old").await.is_none());
    assert!(get_row(&writer, "acc-2", "gone").await.is_none());
}

#[tokio::test]
async fn a_cancelled_transaction_never_reaches_the_table() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    let transaction_id = start_transaction(&writer).await;
    post_actions(
        &writer,
        vec![insert_action(
            &transaction_id,
            vec![entity("acc-1", "a", "x")],
        )],
    )
    .await
    .unwrap();

    let cancel = TransactionGrpcRequest {
        transaction_id: transaction_id.clone(),
    };
    writer
        .cancel_transaction(Request::new(cancel.clone()))
        .await
        .unwrap();

    // Cancelling twice is not an error: nothing it held ever reached a table, so
    // a client can always cancel in its cleanup path without checking first.
    writer
        .cancel_transaction(Request::new(cancel))
        .await
        .unwrap();

    assert!(get_row(&writer, "acc-1", "a").await.is_none());
    assert_eq!(
        commit(&writer, &transaction_id).await.unwrap_err().code(),
        tonic::Code::NotFound
    );
}

/// A refused post leaves the transaction exactly as it was, so the whole post is
/// simply repeated - which is what makes streaming the actions safe.
#[tokio::test]
async fn a_refused_post_leaves_the_transaction_alone() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    let transaction_id = start_transaction(&writer).await;

    post_actions(
        &writer,
        vec![insert_action(
            &transaction_id,
            vec![entity("acc-1", "good", "x")],
        )],
    )
    .await
    .unwrap();

    // The second message of this post carries a broken entity.
    let status = post_actions(
        &writer,
        vec![
            insert_action(&transaction_id, vec![entity("acc-1", "a", "x")]),
            insert_action(&transaction_id, vec![Vec::new()]),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // Repeat the post without the broken half, then commit.
    post_actions(
        &writer,
        vec![insert_action(
            &transaction_id,
            vec![entity("acc-1", "a", "x")],
        )],
    )
    .await
    .unwrap();
    commit(&writer, &transaction_id).await.unwrap();

    assert!(get_row(&writer, "acc-1", "good").await.is_some());
    assert!(get_row(&writer, "acc-1", "a").await.is_some());
}

#[tokio::test]
async fn an_action_which_is_not_exactly_one_instruction_is_refused() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    let transaction_id = start_transaction(&writer).await;

    // Nothing at all.
    let status = post_actions(&writer, vec![action(&transaction_id)])
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // Two at once - their order against each other is undefined, and order is
    // the one thing a transaction may not guess at.
    let mut both = clean_table_action(&transaction_id);
    both.delete_partitions = Some(TransactionDeletePartitionsGrpcModel {
        partition_keys: vec!["acc-1".to_string()],
    });

    let status = post_actions(&writer, vec![both]).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn a_post_which_names_two_transactions_is_refused() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    let first = start_transaction(&writer).await;
    let second = start_transaction(&writer).await;

    let status = post_actions(
        &writer,
        vec![
            insert_action(&first, vec![entity("acc-1", "a", "x")]),
            insert_action(&second, vec![entity("acc-1", "b", "x")]),
        ],
    )
    .await
    .unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn an_unknown_transaction_is_refused() {
    let folder = new_test_folder();
    let writer = start(&folder).await;
    create_table(&writer).await;

    let status = post_actions(&writer, vec![clean_table_action("no-such-transaction")])
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);

    assert_eq!(
        commit(&writer, "no-such-transaction")
            .await
            .unwrap_err()
            .code(),
        tonic::Code::NotFound
    );
}

#[tokio::test]
async fn a_transaction_against_a_table_which_does_not_exist_is_refused() {
    let folder = new_test_folder();
    let writer = start(&folder).await;

    let status = writer
        .start_transaction(Request::new(StartTransactionGrpcRequest {
            name_space: String::new(),
            table_name: TABLE.to_string(),
            sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::NotFound);
}

// ---- clean / delete --------------------------------------------------------

#[tokio::test]
async fn clean_table_empties_it_on_disk_too_and_tells_the_reader() {
    let folder = new_test_folder();

    {
        let writer = start(&folder).await;
        create_table(&writer).await;

        bulk_write(
            &writer,
            vec![bulk_message(
                BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
                vec![entity("acc-1", "a", "x"), entity("acc-2", "a", "x")],
            )],
        )
        .await
        .unwrap();
        drain_persist(&writer).await;

        let reader = ReaderGrpcService::new(writer.app.clone());
        let session_id = greet(&reader).await;
        assert_eq!(subscribe(&reader, &session_id).await.len(), 2);

        writer
            .clean_table(Request::new(CleanTableGrpcRequest {
                name_space: String::new(),
                table_name: TABLE.to_string(),
                sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
            }))
            .await
            .unwrap();

        assert!(get_change(&reader, &session_id).await.clean_table.is_some());

        drain_persist(&writer).await;
    }

    let writer = start(&folder).await;
    let db_namespace = writer.app.namespaces.get("").unwrap();

    // The table itself stays - it was cleaned, not deleted.
    assert!(db_namespace.tables.has_table(TABLE));
    assert_eq!(
        db_namespace
            .tables
            .get_table(TABLE)
            .unwrap()
            .get_partitions_amount(),
        0
    );
}

#[tokio::test]
async fn delete_partitions_drops_them_and_tells_the_reader_which_ones() {
    let folder = new_test_folder();

    {
        let writer = start(&folder).await;
        create_table(&writer).await;

        bulk_write(
            &writer,
            vec![bulk_message(
                BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
                vec![entity("acc-1", "a", "x"), entity("acc-2", "a", "x")],
            )],
        )
        .await
        .unwrap();
        drain_persist(&writer).await;

        let reader = ReaderGrpcService::new(writer.app.clone());
        let session_id = greet(&reader).await;
        assert_eq!(subscribe(&reader, &session_id).await.len(), 2);

        writer
            .delete_partitions(Request::new(DeletePartitionsGrpcRequest {
                name_space: String::new(),
                table_name: TABLE.to_string(),
                partition_keys: vec!["acc-1".to_string(), "never-existed".to_string()],
                sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
            }))
            .await
            .unwrap();

        let change = get_change(&reader, &session_id).await;
        let cleaned = change
            .clean_partitions
            .expect("CleanPartitions must be delivered");
        // Only the partition which was really there.
        assert_eq!(cleaned.partition_keys, vec!["acc-1".to_string()]);

        drain_persist(&writer).await;
    }

    let writer = start(&folder).await;

    assert!(get_row(&writer, "acc-1", "a").await.is_none());
    assert!(get_row(&writer, "acc-2", "a").await.is_some());
}

#[tokio::test]
async fn delete_table_removes_it_from_disk_and_tells_the_reader() {
    let folder = new_test_folder();

    {
        let writer = start(&folder).await;
        create_table(&writer).await;

        bulk_write(
            &writer,
            vec![bulk_message(
                BulkWriteModeGrpcModel::BulkWriteInsertOrReplace,
                vec![entity("acc-1", "a", "x")],
            )],
        )
        .await
        .unwrap();
        drain_persist(&writer).await;

        let reader = ReaderGrpcService::new(writer.app.clone());
        let session_id = greet(&reader).await;
        assert_eq!(subscribe(&reader, &session_id).await.len(), 1);

        writer
            .delete_table(Request::new(DeleteTableGrpcRequest {
                name_space: String::new(),
                table_name: TABLE.to_string(),
                sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
            }))
            .await
            .unwrap();

        // The table is gone, not emptied - and the reader is told exactly that.
        assert!(
            get_change(&reader, &session_id)
                .await
                .delete_table
                .is_some()
        );

        drain_persist(&writer).await;
    }

    let writer = start(&folder).await;

    assert!(
        !writer
            .app
            .namespaces
            .get("")
            .unwrap()
            .tables
            .has_table(TABLE)
    );
}

/// Turning persistence off does not take back what the table already wrote to
/// disk, so deleting it afterwards still has to reach the disk. When the marks
/// were gated on the attribute, this queued nothing at all: the slots stayed
/// occupied, the `tables.meta` entry stayed, and the next start - which never
/// looks at the attribute either - brought the whole table back with every row.
#[tokio::test]
async fn deleting_a_table_whose_persistence_was_turned_off_still_frees_the_disk() {
    let folder = new_test_folder();

    {
        let writer = start(&folder).await;
        create_table(&writer).await;

        writer
            .insert_or_replace(Request::new(write_request("acc-1", "rk", "x")))
            .await
            .unwrap();
        drain_persist(&writer).await;

        set_persist(&writer, false).await;

        writer
            .delete_table(Request::new(DeleteTableGrpcRequest {
                name_space: String::new(),
                table_name: TABLE.to_string(),
                sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
            }))
            .await
            .unwrap();

        drain_persist(&writer).await;
    }

    let writer = start(&folder).await;

    assert!(
        !writer
            .app
            .namespaces
            .get("")
            .unwrap()
            .tables
            .has_table(TABLE),
        "a deleted table came back because its persist flag was off when it was deleted"
    );
}

/// The same for a clean, where the table itself stays: here the persist loop is
/// the one that has to decide, because it finds a table which is still there and
/// says it is not persisted - and a slot it leaves behind is a row that comes
/// back on the next start.
#[tokio::test]
async fn cleaning_a_table_whose_persistence_was_turned_off_still_frees_its_slots() {
    let folder = new_test_folder();

    {
        let writer = start(&folder).await;
        create_table(&writer).await;

        writer
            .insert_or_replace(Request::new(write_request("acc-1", "rk", "x")))
            .await
            .unwrap();
        drain_persist(&writer).await;

        set_persist(&writer, false).await;

        writer
            .clean_table(Request::new(CleanTableGrpcRequest {
                name_space: String::new(),
                table_name: TABLE.to_string(),
                sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
            }))
            .await
            .unwrap();

        drain_persist(&writer).await;
    }

    let writer = start(&folder).await;

    assert!(get_row(&writer, "acc-1", "rk").await.is_none());
}

/// The sequence a migration runs: the attributes of the other server are applied
/// first, and only then is what changed queued. Deciding the marks by the
/// attributes that were just set meant the one case which most needs them -
/// persistence being turned off - was the one that skipped them, so after a
/// restart the migration was gone and the rows it had replaced were back.
#[tokio::test]
async fn a_write_which_turns_persistence_off_still_queues_what_it_changed() {
    let folder = new_test_folder();

    {
        let writer = start(&folder).await;
        create_table(&writer).await;

        writer
            .insert_or_replace(Request::new(write_request("acc-1", "before", "x")))
            .await
            .unwrap();
        drain_persist(&writer).await;

        set_persist(&writer, false).await;

        bulk_write(
            &writer,
            vec![bulk_message(
                BulkWriteModeGrpcModel::BulkWriteCleanTableAndInsert,
                vec![entity("acc-2", "after", "x")],
            )],
        )
        .await
        .unwrap();

        drain_persist(&writer).await;
    }

    let writer = start(&folder).await;

    assert!(
        get_row(&writer, "acc-1", "before").await.is_none(),
        "the row the write replaced came back from a slot nobody freed"
    );

    let db_namespace = writer.app.namespaces.get("").unwrap();
    let db_table = db_namespace.tables.get_table(TABLE).unwrap();

    // The table is still known, and it still says persistence is off - that is
    // the metadata write which used to be skipped by its own new value.
    assert!(!db_table.get_attributes().persist);
}

/// `CreateTableIfNotExists` is the call a service makes at start up with the
/// attributes it wants, so it has to apply them to a table which is already
/// there - a service which raised its limit and redeployed would otherwise go on
/// evicting at the old one with nothing anywhere to say why.
#[tokio::test]
async fn create_table_if_not_exists_applies_the_attributes_to_a_table_which_is_already_there() {
    let folder = new_test_folder();

    let created = {
        let writer = start(&folder).await;
        create_table(&writer).await;

        let created = writer
            .app
            .namespaces
            .get("")
            .unwrap()
            .tables
            .get_table(TABLE)
            .unwrap()
            .get_attributes()
            .created;

        writer
            .create_table_if_not_exists(Request::new(CreateTableGrpcRequest {
                name_space: String::new(),
                table_name: TABLE.to_string(),
                attributes: Some(TableAttributesGrpcModel {
                    persist: true,
                    max_partitions_amount: None,
                    max_rows_per_partition_amount: Some(5),
                }),
                sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
            }))
            .await
            .unwrap();

        let attributes = writer
            .app
            .namespaces
            .get("")
            .unwrap()
            .tables
            .get_table(TABLE)
            .unwrap()
            .get_attributes();

        assert_eq!(attributes.max_rows_per_partition_amount, Some(5));
        // Setting a limit is not founding a table.
        assert_eq!(
            attributes.created.unix_microseconds,
            created.unix_microseconds
        );

        drain_persist(&writer).await;

        created
    };

    let writer = start(&folder).await;

    let attributes = writer
        .app
        .namespaces
        .get("")
        .unwrap()
        .tables
        .get_table(TABLE)
        .unwrap()
        .get_attributes();

    assert_eq!(attributes.max_rows_per_partition_amount, Some(5));
    assert_eq!(
        attributes.created.unix_microseconds,
        created.unix_microseconds
    );
}

#[tokio::test]
async fn deleting_a_table_which_does_not_exist_is_refused() {
    let folder = new_test_folder();
    let writer = start(&folder).await;

    let status = writer
        .delete_table(Request::new(DeleteTableGrpcRequest {
            name_space: String::new(),
            table_name: TABLE.to_string(),
            sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::NotFound);
}

/// A reader which never subscribed to a table must not be told about it.
#[tokio::test]
async fn a_table_nobody_subscribed_to_is_not_queued() {
    let folder = new_test_folder();

    let writer = start(&folder).await;
    create_table(&writer).await;

    let reader = ReaderGrpcService::new(writer.app.clone());
    let session_id = greet(&reader).await;

    writer
        .insert_or_replace(Request::new(write_request("acc-1", "rk", "x")))
        .await
        .unwrap();

    let session = writer.app.reader_sessions.get(&session_id).unwrap();
    assert!(!session.is_subscribed(TABLE));
}

#[tokio::test]
async fn an_unknown_session_is_refused() {
    let folder = new_test_folder();
    let writer = start(&folder).await;
    let reader = ReaderGrpcService::new(writer.app.clone());

    let status = reader
        .get_change(Request::new(GetChangeGrpcRequest {
            session_id: "no-such-session".to_string(),
            read_statistics: Vec::new(),
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::NotFound);
}

/// The namespace of a request becomes a folder inside the persistence root, so
/// the writer is where a name which is not one has to stop - before anything
/// creates a directory out of it.
#[tokio::test]
async fn a_namespace_name_which_is_not_one_never_reaches_the_disk() {
    let folder = new_test_folder();
    let service = start(&folder).await;

    let escaped = "../my-no-sql-grpc-e2e-escaped";

    let status = service
        .create_table_if_not_exists(Request::new(CreateTableGrpcRequest {
            name_space: escaped.to_string(),
            table_name: TABLE.to_string(),
            attributes: Some(TableAttributesGrpcModel {
                persist: true,
                max_partitions_amount: None,
                max_rows_per_partition_amount: None,
            }),
            sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    assert!(
        !service
            .app
            .namespaces
            .get_all()
            .iter()
            .any(|itm| itm.name == escaped)
    );

    let outside = std::path::Path::new(&folder)
        .parent()
        .unwrap()
        .join("my-no-sql-grpc-e2e-escaped");

    assert!(
        !outside.exists(),
        "a namespace name walked out of the persistence root"
    );
}

/// A namespace whose folder did not go is not a namespace that was deleted: the
/// leftover is a complete copy which the next start loads back. Nothing ever
/// retries it - the namespace is out of the server by then - so the caller is
/// the only one who can be told.
#[tokio::test]
async fn a_namespace_whose_folder_stays_is_reported_rather_than_swallowed() {
    let folder = new_test_folder();
    let service = start(&folder).await;

    service
        .app
        .namespaces
        .get_or_create("archive", &service.app.settings)
        .await
        .unwrap();

    // The folder replaced by a file: `remove_dir_all` can not remove that, and a
    // folder it can not remove is the whole of what this is about.
    let namespace_folder = format!("{folder}/archive");
    tokio::fs::remove_dir_all(&namespace_folder).await.unwrap();
    tokio::fs::write(&namespace_folder, b"not a folder")
        .await
        .unwrap();

    let err = crate::db_operations::write::delete_namespace(
        &service.app,
        "archive",
        rust_extensions::date_time::DateTimeAsMicroseconds::now(),
    )
    .await
    .unwrap_err();

    assert!(
        matches!(
            err,
            crate::db_operations::DbOperationError::NamespaceFolderNotDeleted(_)
        ),
        "{err}"
    );

    // Gone from the server whatever happened to the folder - which is exactly
    // why the answer has to say so.
    assert!(service.app.namespaces.get("archive").is_none());

    let _ = std::fs::remove_file(&namespace_folder);
}

/// The persist pass and the folder removal are kept apart by the same lock the
/// timer, the flush and the shutdown drain take: a pass which snapshotted the
/// namespaces before the delete is inside the files of this folder right now,
/// and every file it opens there is an `.expect(..)`.
#[tokio::test]
async fn dropping_a_namespace_waits_for_the_persist_pass() {
    let folder = new_test_folder();
    let service = start(&folder).await;

    service
        .app
        .namespaces
        .get_or_create("archive", &service.app.settings)
        .await
        .unwrap();

    let persisting = service.app.persist_lock.lock().await;

    let app = service.app.clone();
    let deleting = tokio::spawn(async move {
        crate::db_operations::write::delete_namespace(
            &app,
            "archive",
            rust_extensions::date_time::DateTimeAsMicroseconds::now(),
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert!(
        !deleting.is_finished(),
        "the folder went while a persist pass was still writing into it"
    );

    drop(persisting);

    deleting.await.unwrap().unwrap();

    assert!(!std::path::Path::new(&format!("{folder}/archive")).exists());
}

/// A move takes the rows out of the source table only after that table has left
/// its namespace, and publishes the destination one only once it holds them -
/// so what the source folder has to free is handed over by the same cleanup a
/// delete does, and the restart is what proves it happened.
#[tokio::test]
async fn a_moved_table_takes_its_rows_and_frees_the_source_folder() {
    let folder = new_test_folder();

    {
        let service = start(&folder).await;
        create_table(&service).await;

        service
            .insert_or_replace(Request::new(write_request("acc-1", "eur-usd", "hello")))
            .await
            .unwrap();
        service
            .insert_or_replace(Request::new(write_request("acc-2", "eur-gbp", "world")))
            .await
            .unwrap();

        service
            .move_table_to_namespace(Request::new(MoveTableToNamespaceGrpcRequest {
                name_space: String::new(),
                table_name: TABLE.to_string(),
                destination_name_space: "archive".to_string(),
                sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
            }))
            .await
            .unwrap();

        drain_persist(&service).await;
    }

    let service = start(&folder).await;

    assert!(
        !service
            .app
            .namespaces
            .get("")
            .unwrap()
            .tables
            .has_table(TABLE)
    );

    let archive = service.app.namespaces.get("archive").unwrap();
    let db_table = archive.tables.get_table(TABLE).unwrap();

    assert_eq!(
        db_table
            .get_rows(&my_no_sql_grpc_core::db::GetRowsFilter::all())
            .len(),
        2
    );
}
