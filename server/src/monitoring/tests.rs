//! The three monitoring views build their output by hand, which is exactly the
//! class of thing a type checker does not catch - this repo has already lost a
//! JSON bracket that way. These render against a real `AppContext`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use my_no_sql_grpc_abstractions::db_entity::{consts, write_varint};
use rust_extensions::date_time::DateTimeAsMicroseconds;
use tonic::Request;

use crate::app::AppContext;
use crate::grpc_server::WriterGrpcService;
use crate::my_no_sql_writer_grpc::writer_server::Writer;
use crate::my_no_sql_writer_grpc::*;
use crate::settings_reader::SettingsModel;

static FOLDER_NO: AtomicU64 = AtomicU64::new(0);

async fn start() -> WriterGrpcService {
    let folder = std::env::temp_dir().join(format!(
        "my-no-sql-grpc-monitoring-{}-{}",
        std::process::id(),
        FOLDER_NO.fetch_add(1, Ordering::SeqCst)
    ));

    let _ = std::fs::remove_dir_all(&folder);

    let app = Arc::new(AppContext::new(Arc::new(SettingsModel {
        persistence_dest: folder.to_string_lossy().to_string(),
        location: "test".to_string(),
        compress_data: true,
        skip_broken_partitions: false,
        backups_dest: None,
        backup_interval_secs: None,
        max_backups: None,
        api_key: None,
    })));

    crate::operations::load_from_disk(app.clone()).await;

    WriterGrpcService::new(app)
}

/// A schema the write path will actually take. Nothing here shows a row, but a
/// placeholder blob is refused all the same: the server reads a schema back
/// before it keeps it, because bytes it can not read back could never show
/// anything and would sit in `tables.meta` outliving every restart.
fn schema_bytes() -> Vec<u8> {
    use my_no_sql_grpc_abstractions::schemas::{Field, ItemType, Message, Scalar, Schema, Tp};

    let string = Tp::Item(ItemType::Scalar(Scalar::String));

    let field = |no: u32, name: &str| Field {
        no,
        name: name.to_string(),
        tp: string,
    };

    Schema {
        root: 0,
        messages: vec![Message {
            name: "TraderEntity".to_string(),
            fields: vec![field(1, "PartitionKey"), field(2, "RowKey")],
        }],
    }
    .serialize()
}

fn entity(partition_key: &str, row_key: &str) -> Vec<u8> {
    let mut result = Vec::new();

    for (field_no, value) in [(1u32, partition_key), (2, row_key)] {
        write_varint(
            &mut result,
            u64::from(field_no) << 3 | u64::from(consts::WIRE_TYPE_LEN),
        );
        write_varint(&mut result, value.len() as u64);
        result.extend_from_slice(value.as_bytes());
    }

    result
}

async fn write_row(service: &WriterGrpcService, table_name: &str, row_key: &str) {
    service
        .create_table_if_not_exists(Request::new(CreateTableGrpcRequest {
            name_space: String::new(),
            table_name: table_name.to_string(),
            attributes: Some(TableAttributesGrpcModel {
                persist: true,
                max_partitions_amount: None,
                max_rows_per_partition_amount: None,
            }),
            sync_period: SyncPeriodGrpcModel::SyncPeriodMin1 as i32,
        }))
        .await
        .unwrap();

    service
        .insert_or_replace(Request::new(WriteRowGrpcRequest {
            name_space: String::new(),
            table_name: table_name.to_string(),
            // Nothing here shows a row, but the schema still has to be one: the
            // write path reads it back before keeping it, because bytes it can
            // not read back could never show anything.
            schema: Some(EntitySchemaGrpcModel {
                schema_id: 1,
                schema: schema_bytes(),
            }),
            row: entity("acc-1", row_key),
            sync_period: SyncPeriodGrpcModel::SyncPeriodMin1 as i32,
            use_client_time_stamp: false,
        }))
        .await
        .unwrap();
}

/// A line of the exposition format, as a scraper would read it.
fn sample(rendered: &str, prefix: &str) -> Option<String> {
    rendered
        .lines()
        .find(|line| line.starts_with(prefix))
        .map(|line| line.to_string())
}

#[tokio::test]
async fn the_metrics_report_what_the_tables_hold() {
    let service = start().await;
    write_row(&service, "traders", "rk-1").await;

    let rendered = super::metrics::render(&service.app, 3);

    assert_eq!(
        sample(&rendered, "mynosql_table_rows{"),
        Some("mynosql_table_rows{ns=\"default\",table=\"traders\"} 1".to_string())
    );
    assert_eq!(
        sample(&rendered, "mynosql_table_partitions{"),
        Some("mynosql_table_partitions{ns=\"default\",table=\"traders\"} 1".to_string())
    );
    assert_eq!(
        sample(&rendered, "mynosql_tables{"),
        Some("mynosql_tables{ns=\"default\"} 1".to_string())
    );
    assert_eq!(
        sample(&rendered, "mynosql_table_schemas{"),
        Some("mynosql_table_schemas{ns=\"default\",table=\"traders\"} 1".to_string())
    );
    assert_eq!(
        sample(&rendered, "mynosql_http_connections "),
        Some("mynosql_http_connections 3".to_string())
    );

    // Written with Min1, so it is owed to the disk and not yet on it.
    assert_eq!(
        sample(&rendered, "mynosql_persist_queue_partitions{"),
        Some("mynosql_persist_queue_partitions{ns=\"default\"} 1".to_string())
    );

    // Every sample belongs to a family which declared itself first.
    for line in rendered.lines().filter(|line| !line.starts_with('#')) {
        let name = line.split(['{', ' ']).next().unwrap();
        assert!(
            rendered.contains(&format!("# TYPE {name} ")),
            "{name} has samples and no # TYPE"
        );
    }
}

/// The thing a stored registry gets wrong: a table which is gone keeps reporting
/// its last value until the process restarts. Computed per scrape, it does not.
#[tokio::test]
async fn a_table_which_was_deleted_stops_being_reported() {
    let service = start().await;
    write_row(&service, "traders", "rk-1").await;

    assert!(super::metrics::render(&service.app, 0).contains("table=\"traders\""));

    service
        .delete_table(Request::new(DeleteTableGrpcRequest {
            name_space: String::new(),
            table_name: "traders".to_string(),
            sync_period: SyncPeriodGrpcModel::SyncPeriodImmediately as i32,
        }))
        .await
        .unwrap();

    assert!(!super::metrics::render(&service.app, 0).contains("table=\"traders\""));
}

#[tokio::test]
async fn the_status_says_what_the_server_is_made_of() {
    let service = start().await;
    write_row(&service, "traders", "rk-1").await;

    let rendered = super::status::render(&service.app, DateTimeAsMicroseconds::now());
    let json = rendered.as_bytes();

    // Read through paths rather than matched as substrings: what a hand-written
    // writer breaks is the structure, and `contains` would not notice.
    assert_eq!(text(json, "server.name"), crate::app::APP_NAME);
    assert_eq!(text(json, "server.location"), "test");
    // The endpoint as the server bound it, host included: a deployment may
    // narrow gRPC to loopback, and the port alone would hide that.
    assert_eq!(
        text(json, "server.grpcEndpoint"),
        service.app.grpc_endpoint.to_string()
    );
    assert_eq!(
        text(json, "server.httpEndpoint"),
        service.app.http_endpoint.to_string()
    );
    assert!(!boolean(json, "server.backups.configured"));
    // Backups are not configured, so the two policy numbers are absent rather
    // than zero - zero is a policy, and absent is the lack of one.
    assert!(absent(json, "server.backups.intervalSecs"));

    assert_eq!(text(json, "namespaces[0].name"), "default");
    assert_eq!(number(json, "namespaces[0].rowsCount"), 1);
    // Written asking for a minute, so it is owed to the disk and not on it.
    assert_eq!(number(json, "namespaces[0].persistQueue.partitions"), 1);
    assert!(absent(json, "namespaces[0].persistQueue.lastPersistedAt"));

    assert_eq!(text(json, "namespaces[0].tables[0].name"), "traders");
    assert_eq!(number(json, "namespaces[0].tables[0].rowsCount"), 1);
    // One entity per table is what almost every table holds, so this number is
    // read as "is a deploy going through" rather than as a size.
    assert_eq!(number(json, "namespaces[0].tables[0].schemasCount"), 1);
    // The write is what filled this in - a table nobody wrote to has no moment.
    assert!(!absent(json, "namespaces[0].tables[0].lastWriteAt"));

    // Nothing greeted and nothing was started, and both of those are arrays
    // rather than absent keys - a caller iterating them must not have to guess.
    assert_eq!(array_len(json, "readers"), 0);
    assert_eq!(array_len(json, "transactions"), 0);
}

#[tokio::test]
async fn a_flush_empties_the_queue_and_the_status_shows_it() {
    let service = start().await;
    write_row(&service, "traders", "rk-1").await;

    // The table's metadata and its one partition. The schema travelled with the
    // write and is owed to the disk inside that metadata - it is not a task of
    // its own, and creating the table and registering the schema coalesce into
    // the one write they always were.
    assert_eq!(crate::operations::flush(&service.app).await, 2);

    let rendered = super::status::render(&service.app, DateTimeAsMicroseconds::now());
    let json = rendered.as_bytes();

    assert_eq!(number(json, "namespaces[0].persistQueue.partitions"), 0);
    assert_eq!(number(json, "namespaces[0].persistQueue.tablesMetadata"), 0);
    assert!(!absent(json, "namespaces[0].persistQueue.lastPersistedAt"));

    // A second flush has nothing left to take.
    assert_eq!(crate::operations::flush(&service.app).await, 0);
}

/// The reader rows are the same rows `/api/Connections` answers with - the two
/// views share the collector so that a session looks the same wherever it shows.
#[tokio::test]
async fn a_greeted_reader_shows_up_with_what_it_is_subscribed_to() {
    let service = start().await;
    write_row(&service, "traders", "rk-1").await;

    let session = service.app.reader_sessions.create(
        "trading-bot".to_string(),
        "1.4.2".to_string(),
        "default".to_string(),
        "10.0.0.7:51422".to_string(),
    );

    session.subscribe("orders");
    session.subscribe("traders");

    let rendered = super::status::render(&service.app, DateTimeAsMicroseconds::now());
    let json = rendered.as_bytes();

    assert_eq!(array_len(json, "readers"), 1);
    assert_eq!(text(json, "readers[0].name"), "trading-bot");
    assert_eq!(text(json, "readers[0].version"), "1.4.2");
    assert_eq!(text(json, "readers[0].namespace"), "default");
    assert_eq!(text(json, "readers[0].ip"), "10.0.0.7:51422");
    assert_eq!(number(json, "readers[0].pendingChunks"), 0);
    // Sorted: the set behind it is a hash set, and two samples of an unsorted
    // list could not be compared.
    assert_eq!(text(json, "readers[0].tables[0]"), "orders");
    assert_eq!(text(json, "readers[0].tables[1]"), "traders");

    // A write to a table it is subscribed to is queued for it, and that is the
    // number an operator watches to see a reader falling behind.
    write_row(&service, "traders", "rk-2").await;

    let rendered = super::status::render(&service.app, DateTimeAsMicroseconds::now());
    assert!(number(rendered.as_bytes(), "readers[0].pendingChunks") > 0);
}

fn text(json: &[u8], path: &str) -> String {
    my_json::j_path::get_value(json, path)
        .unwrap()
        .unwrap_or_else(|| panic!("{path} is not there"))
        .as_str()
        .unwrap()
        .to_string()
}

fn number(json: &[u8], path: &str) -> i64 {
    my_json::j_path::get_value(json, path)
        .unwrap()
        .unwrap_or_else(|| panic!("{path} is not there"))
        .unwrap_as_number()
        .unwrap()
        .unwrap()
}

fn boolean(json: &[u8], path: &str) -> bool {
    my_json::j_path::get_value(json, path)
        .unwrap()
        .unwrap_or_else(|| panic!("{path} is not there"))
        .unwrap_as_bool()
        .unwrap()
}

fn absent(json: &[u8], path: &str) -> bool {
    my_json::j_path::get_value(json, path).unwrap().is_none()
}

fn array_len(json: &[u8], path: &str) -> usize {
    let value = my_json::j_path::get_value(json, path)
        .unwrap()
        .unwrap_or_else(|| panic!("{path} is not there"));

    assert!(value.is_array(), "{path} is not an array");

    let iterator = value.unwrap_as_array().unwrap();

    let mut result = 0;
    while let Some(item) = iterator.get_next() {
        item.unwrap();
        result += 1;
    }

    result
}
