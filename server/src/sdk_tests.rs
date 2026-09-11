//! The two client crates against a real server on a real socket.
//!
//! Everything else in this crate's tests calls the handlers directly, which
//! skips the transport entirely. Here the server is started the way `main` does,
//! bound to an ephemeral port, and driven by `my-no-sql-grpc-writer` and
//! `my-no-sql-grpc-reader` - so the streaming calls, the long poll and the
//! client-side batching are all in the picture.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rust_extensions::date_time::DateTimeAsMicroseconds;

use my_no_sql_grpc_macros::{my_no_sql_entity, my_no_sql_message};
use my_no_sql_grpc_reader::MyNoSqlGrpcReader;
use my_no_sql_grpc_writer::{
    MyNoSqlGrpcConnection, MyNoSqlGrpcWriter, TableAttributesGrpcModel, my_no_sql_writer_grpc,
};

use crate::app::AppContext;
use crate::grpc_server::{ReaderGrpcService, WriterGrpcService};
use crate::my_no_sql_reader_grpc::reader_server::ReaderServer;
use crate::my_no_sql_writer_grpc::writer_server::WriterServer;
use crate::settings_reader::SettingsModel;

static FOLDER_NO: AtomicU64 = AtomicU64::new(0);

/// A row of the table under test. The macro puts `PartitionKey`, `RowKey`,
/// `TimeStamp` and `Expires` in front of what is declared here, so this file
/// never spells the reserved four out.
#[my_no_sql_entity(table_name: "sdk-traders")]
#[derive(Clone, PartialEq, Debug)]
pub struct TestEntity {
    #[proto_no(5)]
    pub payload: String,
    #[proto_no(6)]
    pub amount: f64,
    #[proto_no(7)]
    pub tags: Vec<String>,
    /// No `proto_no`: it lives in the struct and never touches the wire.
    pub computed_locally: u64,
}

/// A message an entity carries. It numbers its fields from 1: the four the
/// contract reserves belong to the entity, and a message carries no keys.
#[my_no_sql_message]
#[derive(Clone, PartialEq, Debug)]
pub struct Limits {
    #[proto_no(1)]
    pub max_lots: f64,
    #[proto_no(2)]
    pub instruments: Vec<String>,
}

#[my_no_sql_entity(table_name: "sdk-nested")]
#[derive(Clone, PartialEq, Debug)]
pub struct NestedEntity {
    #[proto_no(5)]
    pub limits: Limits,
    #[proto_no(6)]
    pub history: Vec<Limits>,
    #[proto_no(7)]
    pub note: String,
}

fn entity(partition_key: &str, row_key: &str, payload: &str) -> TestEntity {
    TestEntity {
        partition_key: partition_key.to_string(),
        row_key: row_key.to_string(),
        payload: payload.to_string(),
        ..Default::default()
    }
}

fn new_test_folder() -> String {
    let folder = std::env::temp_dir().join(format!(
        "my-no-sql-grpc-sdk-{}-{}",
        std::process::id(),
        FOLDER_NO.fetch_add(1, Ordering::SeqCst)
    ));

    let _ = std::fs::remove_dir_all(&folder);
    folder.to_string_lossy().to_string()
}

/// A running server: what it is serving, where, and the handles which stop it.
///
/// Dropping this leaves the server running, so a test which only needs the
/// server up can ignore it.
struct RunningServer {
    app: Arc<AppContext>,
    address: std::net::SocketAddr,
    url: String,
    task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

impl RunningServer {
    /// Takes the server away and waits until it is really gone.
    ///
    /// Aborting the task is not enough: it drops the listener, but the
    /// connections tonic already accepted are served by tasks of their own, so a
    /// client would keep talking to a server nobody can reach any more. The
    /// shutdown has to go through the server itself, and the port is only free
    /// once its task has finished.
    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// Starts the server the way `main` does. `address: None` lets the OS pick the
/// port, so tests never collide; passing one back in is what lets a test put the
/// server back where a client is still looking for it.
async fn start_server(folder: &str, address: Option<std::net::SocketAddr>) -> RunningServer {
    let app = Arc::new(AppContext::new(Arc::new(SettingsModel {
        persistence_dest: folder.to_string(),
        location: "test".to_string(),
        compress_data: true,
        skip_broken_partitions: false,
        // Beside this test's own folder, not inside it: every folder inside the
        // persistence root is loaded as a namespace. And per test, or the
        // backups of one would be listed by another.
        backups_dest: Some(format!("{folder}-backups")),
        backup_interval_secs: None,
        max_backups: None,
        api_key: None,
    })));

    crate::operations::load_from_disk(app.clone()).await;

    let listener = match address {
        Some(address) => tokio::net::TcpListener::bind(address).await.unwrap(),
        None => tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
    };
    let address = listener.local_addr().unwrap();

    let writer = WriterServer::new(WriterGrpcService::new(app.clone()));
    let reader = ReaderServer::new(ReaderGrpcService::new(app.clone()));

    let (shutdown, stop) = tokio::sync::oneshot::channel();

    let task = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(writer)
            .add_service(reader)
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                async {
                    // A dropped sender must not stop the server: a test which
                    // never intends to stop it simply drops the handle.
                    if stop.await.is_err() {
                        std::future::pending::<()>().await;
                    }
                },
            )
            .await
            .unwrap();
    });

    RunningServer {
        app,
        address,
        url: format!("http://{address}"),
        task,
        shutdown,
    }
}

async fn start_writer() -> (RunningServer, MyNoSqlGrpcWriter<TestEntity>) {
    let server = start_server(&new_test_folder(), None).await;
    let writer = build_writer(&server.url).await;

    (server, writer)
}

async fn build_writer(url: &str) -> MyNoSqlGrpcWriter<TestEntity> {
    let writer: MyNoSqlGrpcWriter<TestEntity> =
        MyNoSqlGrpcWriter::new(MyNoSqlGrpcConnection::new(url.to_string()).unwrap())
            .with_sync_period(my_no_sql_writer_grpc::SyncPeriodGrpcModel::SyncPeriodImmediately);

    writer
        .create_table_if_not_exists(TableAttributesGrpcModel {
            persist: true,
            max_partitions_amount: None,
            max_rows_per_partition_amount: None,
        })
        .await
        .unwrap();

    writer
}

/// The reader is fed by a long poll, so what it holds is eventually consistent
/// with what was written. Waiting for the condition is the honest way to assert
/// on it; the delivery itself takes milliseconds.
async fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..400 {
        if condition() {
            return;
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    panic!("Waited 10 seconds for {what} and it never happened");
}

/// The timer runs every thirty seconds in the real server; a test asks for the
/// pass itself, so it never waits for one.
fn collect_garbage(server: &RunningServer) {
    let now = DateTimeAsMicroseconds::now();

    for db_namespace in server.app.namespaces.get_all() {
        for db_table in db_namespace.tables.get_tables().iter() {
            crate::db_operations::gc::collect(&server.app, &db_namespace, db_table, now, now);
        }
    }
}

/// `Expires` travels with the entity, so a row can be written already knowing
/// when it stops being wanted - and when it goes, the subscribers are told the
/// same way a delete tells them.
#[tokio::test]
async fn an_expired_row_is_collected_and_the_reader_hears_about_it() {
    let (server, writer) = start_writer().await;

    let mut expiring = entity("acc-1", "expiring", "x");
    // 1970: due the moment anything looks at it.
    expiring.expires = 1;

    writer
        .bulk_insert_or_replace(&[expiring, entity("acc-1", "staying", "x")])
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();
    traders.wait_until_initialized().await;
    assert_eq!(traders.get_rows_amount(), 2);

    collect_garbage(&server);

    wait_for("the expired row to leave the reader", || {
        traders.get_row("acc-1", "expiring").unwrap().is_none()
    })
    .await;

    // The row which never expires is untouched, and so is the writer's view.
    assert!(traders.get_row("acc-1", "staying").unwrap().is_some());
    assert!(writer.get_row("acc-1", "expiring").await.unwrap().is_none());
    assert!(writer.get_row("acc-1", "staying").await.unwrap().is_some());

    reader.stop();
}

/// A backup is a zip of one namespace: the partition blobs the persist layer
/// already writes into slots, one entry each, with the schemas and the table
/// attributes they can not be read without. This is the whole round trip: take
/// one, look inside it without restoring, wreck the table, put it back.
/// The one thing a flush has to prove: a change which asked to wait is on disk
/// once the call returns, and a server built over the same folder finds it.
#[tokio::test]
async fn a_flush_puts_on_disk_what_was_asked_to_wait() {
    let folder = new_test_folder();
    let server = start_server(&folder, None).await;

    // A minute away from being written by itself.
    let writer: MyNoSqlGrpcWriter<TestEntity> =
        MyNoSqlGrpcWriter::new(MyNoSqlGrpcConnection::new(server.url.clone()).unwrap())
            .with_sync_period(my_no_sql_writer_grpc::SyncPeriodGrpcModel::SyncPeriodMin1);

    writer
        .create_table_if_not_exists(TableAttributesGrpcModel {
            persist: true,
            max_partitions_amount: None,
            max_rows_per_partition_amount: None,
        })
        .await
        .unwrap();

    writer
        .insert_or_replace(&entity("acc-1", "waiting", "x"))
        .await
        .unwrap();

    let db_namespace = server.app.namespaces.get("").unwrap();
    assert!(
        db_namespace.persist_markers.has_something_to_persist(),
        "the write asked for a minute, so it is still owed to the disk"
    );

    assert!(writer.flush_to_disk().await.unwrap() > 0);

    assert!(
        !db_namespace.persist_markers.has_something_to_persist(),
        "the flush took the whole queue"
    );

    // A second flush finds nothing: what it took, it wrote.
    assert_eq!(writer.flush_to_disk().await.unwrap(), 0);

    // The proof is on disk, not in the queue - a new server over the same folder
    // is built from the page files alone.
    server.stop().await;

    let reopened = start_server(&folder, None).await;

    let row = reopened
        .app
        .namespaces
        .get("")
        .unwrap()
        .tables
        .get_table(TestEntity::TABLE_NAME)
        .expect("the table came back")
        .get_row("acc-1", "waiting");

    assert!(row.is_some(), "the flushed row came back off the disk");
}

#[tokio::test]
async fn a_backup_is_taken_inspected_and_put_back() {
    let (server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[
            entity("acc-1", "a", "one"),
            entity("acc-1", "b", "two"),
            // A key which would have had a path in it under standard base64.
            entity("acc/2", "a", "three"),
        ])
        .await
        .unwrap();

    let taken = writer.make_backup().await.unwrap();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].name_space, "default");
    let name = taken[0].name.clone();

    let backups = writer.get_backups("").await.unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0].name, name);
    assert!(backups[0].size > 0);

    // What is inside, without restoring any of it.
    let tables = writer.inspect_backup("", &name).await.unwrap();
    let table = tables
        .iter()
        .find(|itm| itm.table_name == TestEntity::TABLE_NAME)
        .expect("the table has to be in there");
    assert_eq!(table.partitions.len(), 2);
    assert_eq!(
        table
            .partitions
            .iter()
            .map(|itm| itm.rows_amount)
            .sum::<i32>(),
        3
    );
    // The partition key came back as it was written, path or no path.
    assert!(
        table
            .partitions
            .iter()
            .any(|itm| itm.partition_key == "acc/2")
    );

    // ...and the rows of one partition, as they were stored.
    let rows: Vec<TestEntity> = writer
        .get_backup_rows("", &name, TestEntity::TABLE_NAME, "acc-1")
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);

    // It is an ordinary zip, which is what makes downloading it worth anything.
    let downloaded = writer.download_backup("", &name).await.unwrap();
    assert_eq!(downloaded.len() as i64, backups[0].size);
    assert_eq!(&downloaded[..2], b"PK");

    // Now wreck it and put the whole thing back.
    writer.clean_table().await.unwrap();
    assert!(writer.get_rows(None, None).await.unwrap().is_empty());

    let restored = writer.restore_backup("", &name).await.unwrap();
    assert_eq!(restored, 2);
    assert_eq!(writer.get_rows(None, None).await.unwrap().len(), 3);

    // One partition alone is what somebody who lost one thing wants.
    writer
        .delete_partitions(vec!["acc/2".to_string()])
        .await
        .unwrap();
    assert_eq!(writer.get_rows(None, None).await.unwrap().len(), 2);

    let restored = writer
        .restore_backup_partition("", &name, TestEntity::TABLE_NAME, "acc/2")
        .await
        .unwrap();
    assert_eq!(restored, 1);
    assert_eq!(writer.get_rows(None, None).await.unwrap().len(), 3);

    // The schema came back with it, so the rows are still showable - out of the
    // restored table's own metadata, which is the archive entry it rode in.
    let schema_id = <TestEntity as my_no_sql_grpc_core::MyNoSqlEntity>::get_schema().id;
    assert!(
        server
            .app
            .namespaces
            .get("")
            .unwrap()
            .tables
            .get_table(TestEntity::TABLE_NAME)
            .unwrap()
            .get_schema(schema_id)
            .is_some()
    );
}

/// An archive somebody hands back goes into the backups folder and is restored
/// from there - two calls, because an archive worth keeping is worth looking
/// inside before it replaces anything.
#[tokio::test]
async fn a_backup_can_be_downloaded_and_handed_back() {
    let (server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[entity("acc-1", "a", "one")])
        .await
        .unwrap();

    let taken = writer.make_backup().await.unwrap();
    let content = writer.download_backup("", &taken[0].name).await.unwrap();

    // Somewhere else entirely - a different server, or the same one later.
    let elsewhere = start_server(&new_test_folder(), None).await;
    let there = build_writer(&elsewhere.url).await;

    let name = there.upload_backup("", &content).await.unwrap();
    assert!(there.get_rows(None, None).await.unwrap().is_empty());

    there.restore_backup("", &name).await.unwrap();
    assert_eq!(there.get_rows(None, None).await.unwrap().len(), 1);

    // What is not an archive never gets to look like one.
    assert!(there.upload_backup("", b"not a zip").await.is_err());
    assert_eq!(there.get_backups("").await.unwrap().len(), 1);

    let _ = server;
}

/// A subscriber has to be told what a restore did to the table it is holding.
#[tokio::test]
async fn a_restore_reaches_the_reader() {
    let (server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[entity("acc-1", "a", "x")])
        .await
        .unwrap();

    let name = writer.make_backup().await.unwrap()[0].name.clone();

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();
    traders.wait_until_initialized().await;

    writer.clean_table().await.unwrap();
    wait_for("the clean to reach the reader", || {
        traders.get_rows_amount() == 0
    })
    .await;

    writer.restore_backup("", &name).await.unwrap();

    wait_for("the restore to reach the reader", || {
        traders.get_rows_amount() == 1
    })
    .await;

    reader.stop();
}

#[tokio::test]
async fn something_which_is_not_a_backup_of_this_server_is_refused() {
    let (_server, writer) = start_writer().await;

    assert!(
        writer
            .inspect_backup("", "20260810T071415.zip")
            .await
            .is_err()
    );
    assert!(
        writer
            .restore_backup("", "not-a-backup-name")
            .await
            .is_err()
    );
}

/// The rule this server has everywhere else, applied to a migration: the schema
/// travels with the rows, so the destination registers one it has never seen and
/// every row keeps the reference to it. A migrated table is showable on the
/// other side without anybody copying a schema by hand.
#[tokio::test]
async fn a_table_migrates_to_another_server_with_its_schema() {
    let (source, source_writer) = start_writer().await;

    source_writer
        .bulk_insert_or_replace(&[
            entity("acc-1", "a", "one"),
            entity("acc-1", "b", "two"),
            entity("acc-2", "a", "three"),
        ])
        .await
        .unwrap();

    source_writer
        .set_table_attributes(TableAttributesGrpcModel {
            persist: true,
            max_partitions_amount: Some(9),
            max_rows_per_partition_amount: None,
        })
        .await
        .unwrap();

    // A second server which has never heard of this entity.
    let destination = start_server(&new_test_folder(), None).await;
    let there: MyNoSqlGrpcWriter<TestEntity> =
        MyNoSqlGrpcWriter::new(MyNoSqlGrpcConnection::new(destination.url.clone()).unwrap())
            .with_sync_period(my_no_sql_writer_grpc::SyncPeriodGrpcModel::SyncPeriodImmediately);

    let schema_id = <TestEntity as my_no_sql_grpc_core::MyNoSqlEntity>::get_schema().id;
    assert!(
        destination
            .app
            .namespaces
            .get("")
            .unwrap()
            .tables
            .get_table(TestEntity::TABLE_NAME)
            .is_none()
    );

    let migrated = there
        .migrate_from(&source.url, "", TestEntity::TABLE_NAME)
        .await
        .unwrap();

    assert_eq!(migrated, 3);
    assert_eq!(there.get_rows(None, None).await.unwrap().len(), 3);

    // The schema is there now, and the rows point at it - so the destination can
    // show them.
    let db_namespace = destination.app.namespaces.get("").unwrap();
    assert!(
        db_namespace
            .tables
            .get_table(TestEntity::TABLE_NAME)
            .unwrap()
            .get_schema(schema_id)
            .is_some()
    );
    assert_eq!(
        db_namespace
            .tables
            .get_table(TestEntity::TABLE_NAME)
            .unwrap()
            .get_row("acc-1", "a")
            .unwrap()
            .get_schema_id(),
        schema_id
    );

    // The table arrived with the attributes it had there, not with the defaults.
    assert_eq!(
        db_namespace
            .tables
            .get_table(TestEntity::TABLE_NAME)
            .unwrap()
            .get_attributes()
            .max_partitions_amount,
        Some(9)
    );

    // The source is untouched - a migration is a pull, not a move.
    assert_eq!(source_writer.get_rows(None, None).await.unwrap().len(), 3);
}

#[tokio::test]
async fn a_migration_from_somewhere_which_is_not_there_is_refused() {
    let (_server, writer) = start_writer().await;

    // Nothing listening.
    assert!(
        writer
            .migrate_from("http://127.0.0.1:1", "", TestEntity::TABLE_NAME)
            .await
            .is_err()
    );

    // Not a url at all.
    assert!(
        writer
            .migrate_from("not-a-url", "", TestEntity::TABLE_NAME)
            .await
            .is_err()
    );
}

/// A namespace owns its tables, its schemas and its folder, so moving a table
/// between two of them has to carry all three.
#[tokio::test]
async fn a_table_moves_between_namespaces_with_its_data() {
    let (server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[entity("acc-1", "a", "one"), entity("acc-2", "b", "two")])
        .await
        .unwrap();

    // A reader of the destination sees the table arrive.
    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0")
        .unwrap()
        .with_name_space("archive");
    let traders = reader.subscribe::<TestEntity>();

    // Neither the namespace nor the table is there yet, and the reader is
    // started into exactly that - which is the ordinary cold start.
    reader.start();

    let in_archive: MyNoSqlGrpcWriter<TestEntity> =
        MyNoSqlGrpcWriter::new(MyNoSqlGrpcConnection::new(server.url.clone()).unwrap())
            .with_name_space("archive")
            .with_sync_period(my_no_sql_writer_grpc::SyncPeriodGrpcModel::SyncPeriodImmediately);

    writer.move_table_to_namespace("archive").await.unwrap();

    wait_for("the moved table to reach the destination's reader", || {
        traders.get_rows_amount() == 2
    })
    .await;

    // The source no longer has it...
    assert!(
        writer
            .get_rows(None, None)
            .await
            .unwrap_err()
            .is_not_found()
    );
    // ...and the destination does, rows and all.
    assert_eq!(in_archive.get_rows(None, None).await.unwrap().len(), 2);

    // The schema travelled too, so the destination can still show the rows - it
    // is an attribute of the table, and the table is what moved.
    let archive = server.app.namespaces.get("archive").unwrap();
    let schema_id = <TestEntity as my_no_sql_grpc_core::MyNoSqlEntity>::get_schema().id;
    assert!(
        archive
            .tables
            .get_table(TestEntity::TABLE_NAME)
            .unwrap()
            .get_schema(schema_id)
            .is_some()
    );

    let namespaces = writer.get_namespaces().await.unwrap();
    assert!(namespaces.iter().any(|itm| itm.name == "archive"));
    assert!(namespaces.iter().any(|itm| itm.name == "default"));

    reader.stop();
}

/// The ordinary cold start: a reader deployed before its writer wants a table
/// nobody has created yet. It has to come up empty and initialized rather than
/// wedged - and, above all, one table which is not there must not take the
/// tables beside it down with it, which is what refusing the subscription did:
/// the session starts over from the greeting, so nothing ordered behind the
/// missing table ever got its turn.
#[tokio::test]
async fn a_reader_which_subscribes_before_the_table_exists_starts_empty_and_then_fills() {
    let server = start_server(&new_test_folder(), None).await;

    // One of the two tables exists and has a row in it; the other has never
    // been written to at all.
    let nested_writer: MyNoSqlGrpcWriter<NestedEntity> =
        MyNoSqlGrpcWriter::new(MyNoSqlGrpcConnection::new(server.url.clone()).unwrap())
            .with_sync_period(my_no_sql_writer_grpc::SyncPeriodGrpcModel::SyncPeriodImmediately);

    nested_writer
        .create_table_if_not_exists(TableAttributesGrpcModel {
            persist: true,
            max_partitions_amount: None,
            max_rows_per_partition_amount: None,
        })
        .await
        .unwrap();

    nested_writer
        .insert_or_replace(&NestedEntity {
            partition_key: "acc-1".to_string(),
            row_key: "a".to_string(),
            note: "already there".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    let nested = reader.subscribe::<NestedEntity>();
    reader.start();

    // Bounded rather than plain: this used to hang for the whole life of the
    // process, and a test which hangs says nothing about why.
    tokio::time::timeout(Duration::from_secs(10), traders.wait_until_initialized())
        .await
        .expect("the reader never came up on a table which does not exist yet");

    // "No rows" is the honest answer for a table which is not there, and it is
    // the answer which lets an application past its own start up.
    assert_eq!(traders.get_rows_amount(), 0);

    // The table which does exist is filled all the same - in either order.
    tokio::time::timeout(Duration::from_secs(10), nested.wait_until_initialized())
        .await
        .expect("a table which does exist was held up by one which does not");
    assert_eq!(nested.get_rows_amount(), 1);

    // And the first write into the missing table reaches this same session,
    // without a reconnect: the subscription was registered before the table.
    let writer = build_writer(&server.url).await;
    writer
        .insert_or_replace(&entity("acc-1", "a", "one"))
        .await
        .unwrap();

    wait_for(
        "the first write to reach the reader which arrived first",
        || traders.get_rows_amount() == 1,
    )
    .await;

    reader.stop();
}

/// A reader which stops draining is not held on to for ever: `GetChange` hands
/// out one chunk per call, so a backlog past the ceiling is one nobody is
/// catching up with. It is dropped whole and the reader starts over - the same
/// recovery every other failure has.
#[tokio::test]
async fn a_reader_which_falls_hopelessly_behind_is_dropped_and_starts_over() {
    let (server, writer) = start_writer().await;

    writer
        .insert_or_replace(&entity("acc-1", "a", "one"))
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();

    traders.wait_until_initialized().await;
    assert_eq!(traders.get_rows_amount(), 1);

    let session = server.app.reader_sessions.get_all().pop().unwrap();
    let session_id = session.id.clone();

    // What a reader which went quiet under a busy writer does to the server,
    // done in one go. The chunks are `CleanTable` on purpose: if any of them
    // were delivered the cache would empty, and it does not.
    session.enqueue((0..crate::reader::MAX_QUEUED_CHUNKS + 1).map(|_| {
        crate::reader::SyncChunk::CleanTable {
            table_name: TestEntity::TABLE_NAME.to_string(),
        }
    }));

    wait_for("the session which fell behind to be forgotten", || {
        server.app.reader_sessions.get(&session_id).is_none()
    })
    .await;

    // The reader greets again by itself, and a write after that finds it.
    writer
        .insert_or_replace(&entity("acc-1", "b", "two"))
        .await
        .unwrap();

    wait_for("the reader which was dropped to come back", || {
        traders.get_rows_amount() == 2
    })
    .await;

    reader.stop();
}

#[tokio::test]
async fn a_namespace_can_be_dropped_but_not_the_default_one() {
    let (server, _writer) = start_writer().await;

    let in_archive: MyNoSqlGrpcWriter<TestEntity> =
        MyNoSqlGrpcWriter::new(MyNoSqlGrpcConnection::new(server.url.clone()).unwrap())
            .with_name_space("archive")
            .with_sync_period(my_no_sql_writer_grpc::SyncPeriodGrpcModel::SyncPeriodImmediately);

    in_archive
        .create_table_if_not_exists(TableAttributesGrpcModel {
            persist: true,
            max_partitions_amount: None,
            max_rows_per_partition_amount: None,
        })
        .await
        .unwrap();
    in_archive
        .insert_or_replace(&entity("acc-1", "a", "x"))
        .await
        .unwrap();

    in_archive.delete_namespace("archive").await.unwrap();

    assert!(server.app.namespaces.get("archive").is_none());
    assert!(
        in_archive
            .get_rows(None, None)
            .await
            .unwrap_err()
            .is_not_found()
    );

    // The one place every write which names no namespace lands is not a
    // namespace like the others.
    let err = in_archive.delete_namespace("").await.unwrap_err();
    assert!(!err.is_not_found(), "{err}");
}

/// The three reads a row-key-as-a-moment table lives on, plus the counters.
#[tokio::test]
async fn the_narrow_reads_answer_what_they_say_they_do() {
    let (_server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[
            entity("acc-1", "2026-01-01", "january"),
            entity("acc-1", "2026-02-01", "february"),
            entity("acc-1", "2026-03-01", "march"),
            entity("acc-2", "2026-01-01", "other partition"),
        ])
        .await
        .unwrap();

    // What was in force at that moment, and what came before it - highest first.
    let found = writer
        .get_highest_row_and_below("acc-1", "2026-02-15", None)
        .await
        .unwrap();
    let keys: Vec<&str> = found.iter().map(|itm| itm.row_key.as_str()).collect();
    assert_eq!(keys, vec!["2026-02-01", "2026-01-01"]);

    // A key which is there exactly is at or below itself.
    let found = writer
        .get_highest_row_and_below("acc-1", "2026-02-01", Some(1))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].row_key, "2026-02-01");

    // Nothing at or below.
    assert!(
        writer
            .get_highest_row_and_below("acc-1", "2025-12-31", None)
            .await
            .unwrap()
            .is_empty()
    );

    // Several named rows in one call; a key which is not there is left out.
    let found = writer
        .get_single_partition_multiple_rows(
            "acc-1",
            vec!["2026-03-01".to_string(), "never".to_string()],
        )
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].payload, "march");

    let size = writer.get_table_size().await.unwrap();
    assert_eq!(size.partitions_amount, 2);
    assert_eq!(size.rows_amount, 4);
    assert!(size.content_size > 0);
}

/// A limit which turned out wrong has to be changeable without moving the data,
/// and a subscriber which shows a table wants to know it changed.
#[tokio::test]
async fn table_attributes_can_be_changed_and_the_reader_is_told() {
    let (server, writer) = start_writer().await;

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();
    traders.wait_until_initialized().await;

    writer
        .set_table_attributes(TableAttributesGrpcModel {
            persist: true,
            max_partitions_amount: Some(7),
            max_rows_per_partition_amount: None,
        })
        .await
        .unwrap();

    wait_for("the new attributes to reach the reader", || {
        traders.get_attributes().max_partitions_amount == Some(7)
    })
    .await;

    // The server agrees, and the moment the table was created is not a thing a
    // limit change resets.
    let db_table = server
        .app
        .namespaces
        .get("")
        .unwrap()
        .tables
        .get_table(TestEntity::TABLE_NAME)
        .unwrap();
    assert_eq!(db_table.get_attributes().max_partitions_amount, Some(7));

    let tables = writer.get_tables().await.unwrap();
    let table = tables
        .iter()
        .find(|itm| itm.name == TestEntity::TABLE_NAME)
        .unwrap();
    assert_eq!(
        table.attributes.as_ref().unwrap().max_partitions_amount,
        Some(7)
    );

    reader.stop();
}

/// The limits are applied on a schedule; these ask for them right now, with a
/// number of their own.
#[tokio::test]
async fn a_limit_can_be_applied_by_hand_through_the_sdk() {
    let (server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[
            entity("acc-1", "a", "x"),
            entity("acc-2", "a", "x"),
            entity("acc-3", "a", "x"),
            entity("acc-3", "b", "x"),
        ])
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();
    traders.wait_until_initialized().await;
    assert_eq!(traders.get_rows_amount(), 4);

    writer
        .clean_partition_and_keep_max_rows("acc-3", 1)
        .await
        .unwrap();

    wait_for("the row limit to reach the reader", || {
        traders.get_by_partition_key("acc-3").unwrap().len() == 1
    })
    .await;

    writer.clean_and_keep_max_partitions(1).await.unwrap();

    wait_for("the partition limit to reach the reader", || {
        traders.get_partition_keys().len() == 1
    })
    .await;

    assert_eq!(writer.get_rows(None, None).await.unwrap().len(), 1);

    // A limit nobody could mean is refused rather than read as "no limit".
    assert!(writer.clean_and_keep_max_partitions(-1).await.is_err());

    reader.stop();
}

/// The point of the schema the macro builds: the server has to be able to read
/// it back and show a stored row under its own field names. Nothing else in this
/// workspace reads those bytes back, so if the macro emitted a shape the reader
/// refuses - a message out of order, a field twice - only this would say so.
#[tokio::test]
async fn the_schema_the_macro_built_renders_a_row_by_its_field_names() {
    let (server, writer) = start_writer().await;

    let mut entity = entity("acc-1", "eur-usd", "hello");
    entity.amount = 1.5;
    entity.tags = vec!["a".to_string(), "b".to_string()];
    entity.computed_locally = 42;

    writer.insert_or_replace(&entity).await.unwrap();

    let db_namespace = server.app.namespaces.get("").unwrap();

    let schema_id = <TestEntity as my_no_sql_grpc_core::MyNoSqlEntity>::get_schema().id;
    let stored_schema = db_namespace
        .tables
        .get_table(TestEntity::TABLE_NAME)
        .unwrap()
        .get_schema(schema_id)
        .expect("the schema travelled with the write");

    let index = server
        .app
        .json_schemas
        .get_or_build(&stored_schema)
        .expect("the server has to be able to decode what the macro built");

    let db_row = db_namespace
        .tables
        .get_table(TestEntity::TABLE_NAME)
        .unwrap()
        .get_row("acc-1", "eur-usd")
        .unwrap();

    let json = crate::json_view::write_row_as_json(
        my_json::json_writer::JsonObjectWriter::new(),
        &db_row.to_vec(),
        Some(&index),
    )
    .build();

    // The four reserved fields keep the names the contract gives them...
    assert!(json.contains(r#""PartitionKey":"acc-1""#), "{json}");
    assert!(json.contains(r#""RowKey":"eur-usd""#), "{json}");
    assert!(json.contains(r#""TimeStamp":"2"#), "{json}");
    // ...and the declared ones are rendered under their own, in PascalCase.
    assert!(json.contains(r#""Payload":"hello""#), "{json}");
    assert!(json.contains(r#""Amount":1.5"#), "{json}");
    assert!(json.contains(r#""Tags":["a","b"]"#), "{json}");
    // The field with no proto_no never went anywhere.
    assert!(!json.contains("Computed"), "{json}");

    // And it survives the round trip back into the entity.
    let read_back = writer.get_row("acc-1", "eur-usd").await.unwrap().unwrap();
    assert_eq!(read_back.amount, 1.5);
    assert_eq!(read_back.tags, vec!["a".to_string(), "b".to_string()]);
    assert!(read_back.time_stamp > 0);
    assert_eq!(read_back.computed_locally, 0);
}

/// The nested case of the same promise the schema makes: a message an
/// entity carries is declared beside it, so the server shows it as a nested
/// object rather than as a blob it can not read.
#[tokio::test]
async fn a_nested_message_renders_as_a_nested_object() {
    let (server, _writer) = start_writer().await;

    let writer: MyNoSqlGrpcWriter<NestedEntity> =
        MyNoSqlGrpcWriter::new(MyNoSqlGrpcConnection::new(server.url.clone()).unwrap())
            .with_sync_period(my_no_sql_writer_grpc::SyncPeriodGrpcModel::SyncPeriodImmediately);

    writer
        .create_table_if_not_exists(TableAttributesGrpcModel {
            persist: true,
            max_partitions_amount: None,
            max_rows_per_partition_amount: None,
        })
        .await
        .unwrap();

    let source = NestedEntity {
        partition_key: "acc-1".to_string(),
        row_key: "rk".to_string(),
        limits: Limits {
            max_lots: 12.5,
            instruments: vec!["EURUSD".to_string(), "BTCUSD".to_string()],
        },
        history: vec![
            Limits {
                max_lots: 1.0,
                instruments: vec!["GBPUSD".to_string()],
            },
            Limits {
                max_lots: 2.0,
                instruments: Vec::new(),
            },
        ],
        note: "hello".to_string(),
        ..Default::default()
    };

    writer.insert_or_replace(&source).await.unwrap();

    // It survives the round trip through the wire as itself - bar `TimeStamp`,
    // which the server stamped because the entity carried none.
    let mut read_back = writer.get_row("acc-1", "rk").await.unwrap().unwrap();
    assert!(read_back.time_stamp > 0);
    read_back.time_stamp = 0;
    assert_eq!(read_back, source);

    // ...and the server, which never saw the Rust types, renders it by name.
    let db_namespace = server.app.namespaces.get("").unwrap();

    let schema_id = <NestedEntity as my_no_sql_grpc_core::MyNoSqlEntity>::get_schema().id;
    let stored_schema = db_namespace
        .tables
        .get_table(NestedEntity::TABLE_NAME)
        .unwrap()
        .get_schema(schema_id)
        .expect("the schema travelled with the write");

    let index = server
        .app
        .json_schemas
        .get_or_build(&stored_schema)
        .expect("the schema the macro built has to be readable");

    let db_row = db_namespace
        .tables
        .get_table(NestedEntity::TABLE_NAME)
        .unwrap()
        .get_row("acc-1", "rk")
        .unwrap();

    let json = crate::json_view::write_row_as_json(
        my_json::json_writer::JsonObjectWriter::new(),
        &db_row.to_vec(),
        Some(&index),
    )
    .build();

    // A message is an object, not a blob...
    assert!(
        json.contains(r#""Limits":{"MaxLots":12.5,"Instruments":["EURUSD","BTCUSD"]}"#),
        "{json}"
    );
    // ...and a repeated one is an array of them. The second carries no
    // instruments, and an empty repeated field is not on the wire at all - so
    // the object it renders as does not have the key either.
    assert!(
        json.contains(r#""History":[{"MaxLots":1,"Instruments":["GBPUSD"]},{"MaxLots":2}]"#),
        "{json}"
    );
    assert!(json.contains(r#""Note":"hello""#), "{json}");
    // The reserved four stay the entity's own - a nested message has none.
    assert!(json.contains(r#""PartitionKey":"acc-1""#), "{json}");
}

/// The reader's headline promise: any failed call means "start over", and the
/// cache keeps answering with what it had while that happens. A server which
/// went away and came back has to be invisible to whoever is reading.
#[tokio::test]
async fn the_reader_survives_the_server_going_away() {
    let folder = new_test_folder();

    let server = start_server(&folder, None).await;
    let address = server.address;
    let writer = build_writer(&server.url).await;

    writer
        .insert_or_replace(&entity("acc-1", "before", "x"))
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();
    traders.wait_until_initialized().await;

    // Everything queued has to be on disk, or the server which starts next over
    // the same folder would come up empty for reasons that have nothing to do
    // with the reader.
    while crate::operations::persist(&server.app, None).await {}

    server.stop().await;

    // The reader is now failing its calls, and it keeps serving what it holds.
    // A cache which answered "no rows" here would be worse than a stale one.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(traders.get_row("acc-1", "before").unwrap().is_some());

    // The same address, so the client finds it again without being told.
    let server = start_server(&folder, Some(address)).await;
    let writer = build_writer(&server.url).await;

    writer
        .insert_or_replace(&entity("acc-1", "after", "x"))
        .await
        .unwrap();

    wait_for("the reader to greet again and re-subscribe", || {
        traders.get_row("acc-1", "after").unwrap().is_some()
    })
    .await;

    // The re-subscription brought the whole table back, not just what changed.
    assert!(traders.get_row("acc-1", "before").unwrap().is_some());

    reader.stop();
}

#[tokio::test]
async fn the_reader_gets_the_snapshot_and_then_every_change() {
    let (server, writer) = start_writer().await;

    writer
        .insert_or_replace(&entity("acc-1", "before", "hello"))
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();

    traders.wait_until_initialized().await;

    // What was written before the subscription is in the snapshot.
    let row = traders.get_row("acc-1", "before").unwrap().unwrap();
    assert_eq!(row.payload, "hello");
    // The server stamped it on the way in, and the stamp survived the round trip.
    assert!(row.time_stamp > 0);

    // ...and what is written after it arrives through the queue.
    writer
        .insert_or_replace(&entity("acc-1", "after", "world"))
        .await
        .unwrap();

    wait_for("the new row to reach the reader", || {
        traders.get_row("acc-1", "after").unwrap().is_some()
    })
    .await;

    writer.delete_row("acc-1", "after").await.unwrap();

    wait_for("the deleted row to leave the reader", || {
        traders.get_row("acc-1", "after").unwrap().is_none()
    })
    .await;

    // The row which was not deleted is still there - a delete is not a reset.
    assert!(traders.get_row("acc-1", "before").unwrap().is_some());

    reader.stop();
}

#[tokio::test]
async fn a_batch_written_through_the_sdk_reaches_the_reader_whole() {
    let (server, writer) = start_writer().await;

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();
    traders.wait_until_initialized().await;

    let batch: Vec<TestEntity> = (0..250)
        .map(|no| entity(&format!("acc-{}", no % 5), &format!("rk-{no}"), "x"))
        .collect();

    writer.bulk_insert_or_replace(&batch).await.unwrap();

    wait_for("the whole batch to reach the reader", || {
        traders.get_rows_amount() == 250
    })
    .await;

    assert_eq!(traders.get_partition_keys().len(), 5);
    assert_eq!(traders.get_by_partition_key("acc-0").unwrap().len(), 50);

    // The partitions the batch names end up holding exactly the batch.
    writer
        .clean_partitions_and_insert(&[entity("acc-0", "only-one", "fresh")])
        .await
        .unwrap();

    wait_for("the partition to be replaced in the reader", || {
        traders.get_by_partition_key("acc-0").unwrap().len() == 1
    })
    .await;

    // ...and the partitions it does not name are untouched.
    assert_eq!(traders.get_by_partition_key("acc-1").unwrap().len(), 50);

    reader.stop();
}

#[tokio::test]
async fn rows_named_by_key_are_deleted_in_one_go() {
    use my_no_sql_writer_grpc::PartitionRowKeysGrpcModel;

    let (server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[
            entity("acc-1", "gone", "x"),
            entity("acc-1", "stays", "x"),
            entity("acc-2", "gone-too", "x"),
            entity("acc-3", "untouched", "x"),
        ])
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();
    traders.wait_until_initialized().await;
    assert_eq!(traders.get_rows_amount(), 4);

    let deleted = writer
        .bulk_delete(vec![
            PartitionRowKeysGrpcModel {
                partition_key: "acc-1".to_string(),
                // "never-was" is not there, and naming it is not an error.
                row_keys: vec!["gone".to_string(), "never-was".to_string()],
            },
            PartitionRowKeysGrpcModel {
                partition_key: "acc-2".to_string(),
                row_keys: vec!["gone-too".to_string()],
            },
            // A partition which is not there either.
            PartitionRowKeysGrpcModel {
                partition_key: "acc-9".to_string(),
                row_keys: vec!["nothing".to_string()],
            },
        ])
        .await
        .unwrap();

    // What was there, not what was named.
    assert_eq!(deleted, 2);

    wait_for("the deletes to reach the reader", || {
        traders.get_rows_amount() == 2
    })
    .await;

    assert!(traders.get_row("acc-1", "gone").unwrap().is_none());
    assert!(traders.get_row("acc-1", "stays").unwrap().is_some());
    assert!(traders.get_row("acc-2", "gone-too").unwrap().is_none());
    assert!(traders.get_row("acc-3", "untouched").unwrap().is_some());

    reader.stop();
}

#[tokio::test]
async fn an_older_row_is_declined_and_a_newer_one_is_taken() {
    let (server, writer) = start_writer().await;

    let at = |payload: &str, time_stamp: i64| TestEntity {
        time_stamp,
        ..entity("acc-1", "rk", payload)
    };

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();
    traders.wait_until_initialized().await;

    // Nothing stored yet, so there is nothing to be older than.
    assert!(
        writer
            .insert_or_replace_if_new(&at("second", 2000))
            .await
            .unwrap()
    );

    wait_for("the row to reach the reader", || {
        traders.get_row("acc-1", "rk").unwrap().is_some()
    })
    .await;

    // Older: declined, and declining is not a failure.
    assert!(
        !writer
            .insert_or_replace_if_new(&at("first", 1000))
            .await
            .unwrap()
    );

    // Same moment is not strictly newer either.
    assert!(
        !writer
            .insert_or_replace_if_new(&at("also-second", 2000))
            .await
            .unwrap()
    );

    assert_eq!(
        writer
            .get_row("acc-1", "rk")
            .await
            .unwrap()
            .unwrap()
            .payload,
        "second"
    );

    assert!(
        writer
            .insert_or_replace_if_new(&at("third", 3000))
            .await
            .unwrap()
    );

    // A declined write tells the reader nothing, so what it ends up holding is
    // the one write that was taken - it never passed through "first".
    wait_for("the newer row to reach the reader", || {
        traders
            .get_row("acc-1", "rk")
            .unwrap()
            .is_some_and(|row| row.payload == "third")
    })
    .await;

    reader.stop();
}

#[tokio::test]
async fn a_transaction_through_the_sdk_lands_as_one_step() {
    let (server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[
            entity("acc-1", "old", "x"),
            entity("acc-2", "gone", "x"),
            entity("acc-2", "stays", "x"),
        ])
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();
    traders.wait_until_initialized().await;
    assert_eq!(traders.get_rows_amount(), 3);

    let mut transaction = writer.begin_transaction().await.unwrap();
    transaction.delete_partitions(vec!["acc-1".to_string()]);
    transaction.delete_rows("acc-2", vec!["gone".to_string()]);
    transaction.insert_or_replace(&[entity("acc-3", "new", "fresh")]);

    // Nothing has been applied yet - that is what the commit is for.
    assert_eq!(traders.get_rows_amount(), 3);

    transaction.commit().await.unwrap();

    // The transaction is one step on the server, but its instructions reach the
    // reader one per answer, so the cache passes through the states in between.
    // What says "all of it has landed" is the last thing the transaction did -
    // the queue keeps the order, so everything before it is already applied.
    wait_for("the whole transaction to reach the reader", || {
        traders.get_row("acc-3", "new").unwrap().is_some()
    })
    .await;

    assert_eq!(traders.get_rows_amount(), 2);
    assert!(traders.get_row("acc-1", "old").unwrap().is_none());
    assert!(traders.get_row("acc-2", "gone").unwrap().is_none());
    assert!(traders.get_row("acc-2", "stays").unwrap().is_some());
    assert!(traders.get_row("acc-3", "new").unwrap().is_some());

    reader.stop();
}

/// A commit which fails has to leave the handle behind, because the handle is
/// the only thing holding the id: the server keeps the transaction open, and
/// cancelling it needs that id. A commit which consumed the handle turned "the
/// client can always cancel in its `finally`" into a promise broken exactly
/// where a client wants to use it.
#[tokio::test]
async fn a_failed_commit_leaves_the_transaction_cancellable() {
    let (_server, writer) = start_writer().await;

    let mut transaction = writer.begin_transaction().await.unwrap();
    transaction.insert_or_replace(&[entity("acc-1", "one", "x")]);
    transaction.commit().await.unwrap();

    // The server takes the transaction out of its registry as it applies it, so
    // this commit is answered `not_found` - a commit that failed.
    let err = transaction.commit().await.unwrap_err();
    assert!(err.is_not_found(), "{err:?}");

    // And the handle is still here to clean up with.
    transaction.cancel().await.unwrap();

    assert!(writer.get_row("acc-1", "one").await.unwrap().is_some());
}

#[tokio::test]
async fn a_cancelled_transaction_changes_nothing() {
    let (_server, writer) = start_writer().await;

    let mut transaction = writer.begin_transaction().await.unwrap();
    transaction.insert_or_replace(&[entity("acc-1", "never", "x")]);
    transaction.post().await.unwrap();
    transaction.cancel().await.unwrap();

    assert!(writer.get_row("acc-1", "never").await.unwrap().is_none());
}

/// The channel is lazy, so "connected" is nothing anybody has checked until a
/// call is made. This is the cheapest one there is.
#[tokio::test]
async fn ping_proves_the_connection() {
    let (_server, writer) = start_writer().await;

    writer.ping().await.unwrap();
}

/// Half of the migration contract, and the shape a write comes in as: rows
/// grouped by the schema they were written under.
#[tokio::test]
async fn the_table_comes_back_grouped_by_the_schema_it_was_written_under() {
    let (_server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[entity("acc-1", "one", "x"), entity("acc-1", "two", "y")])
        .await
        .unwrap();

    let chunks = writer.get_rows_with_schema().await.unwrap();

    // One version of the entity was written, so one chunk.
    assert_eq!(chunks.len(), 1);

    let schema = chunks[0].schema.as_ref().unwrap();
    assert_eq!(
        schema.schema_id,
        <TestEntity as my_no_sql_grpc_core::MyNoSqlEntity>::get_schema().id
    );

    // The rows travel as they are stored, so the destination stores what the
    // source had rather than a re-serialization of it.
    let mut payloads: Vec<String> = chunks[0]
        .rows
        .iter()
        .map(|row| {
            <TestEntity as my_no_sql_grpc_core::MyNoSqlEntity>::from_slice(row)
                .unwrap()
                .payload
        })
        .collect();

    payloads.sort();

    assert_eq!(payloads, vec!["x".to_string(), "y".to_string()]);
}

/// The errors a caller routinely has to tell apart come back as themselves,
/// not as "something went wrong".
#[tokio::test]
async fn the_writer_reports_what_the_server_refused() {
    let (_server, writer) = start_writer().await;

    writer
        .insert(&entity("acc-1", "rk", "first"))
        .await
        .unwrap();

    let err = writer
        .insert(&entity("acc-1", "rk", "second"))
        .await
        .unwrap_err();
    assert!(err.is_already_exists(), "{err}");

    // Replace checks the version the entity carries, so an entity built from
    // nothing is refused before anything is looked up - it is given a version to
    // get past that and reach the row that is not there.
    let mut missing = entity("acc-1", "missing", "x");
    missing.time_stamp = 1;

    let err = writer.replace(&missing).await.unwrap_err();
    assert!(err.is_not_found(), "{err}");

    // And the version which is not the stored one comes back as a conflict -
    // the reply the read-modify-write loop is built on.
    let mut stale = entity("acc-1", "rk", "third");
    stale.time_stamp = 1;

    let err = writer.replace(&stale).await.unwrap_err();
    assert!(err.is_conflict(), "{err}");
}

#[tokio::test]
async fn the_writer_reads_back_what_it_wrote() {
    let (_server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[
            entity("acc-1", "a", "one"),
            entity("acc-1", "b", "two"),
            entity("acc-2", "a", "three"),
        ])
        .await
        .unwrap();

    let all = writer.get_rows(None, None).await.unwrap();
    assert_eq!(all.len(), 3);

    let of_partition = writer.get_rows(Some("acc-1"), None).await.unwrap();
    assert_eq!(of_partition.len(), 2);

    let one = writer.get_row("acc-2", "a").await.unwrap().unwrap();
    assert_eq!(one.payload, "three");

    let tables = writer.get_tables().await.unwrap();
    assert!(tables.iter().any(|itm| itm.name == TestEntity::TABLE_NAME));
}

/// Emptying the table through the contract has to empty the reader's cache too,
/// and cleaning is not deleting: the table stays and keeps taking rows.
#[tokio::test]
async fn clean_table_empties_the_reader_as_well() {
    let (server, writer) = start_writer().await;

    writer
        .bulk_insert_or_replace(&[entity("acc-1", "a", "x"), entity("acc-2", "a", "x")])
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(server.url.clone(), "sdk-test", "1.0.0").unwrap();
    let traders = reader.subscribe::<TestEntity>();
    reader.start();
    traders.wait_until_initialized().await;
    assert_eq!(traders.get_rows_amount(), 2);

    writer.clean_table().await.unwrap();

    wait_for("the clean to reach the reader", || {
        traders.get_rows_amount() == 0
    })
    .await;

    writer
        .insert_or_replace(&entity("acc-3", "after", "x"))
        .await
        .unwrap();

    wait_for("the table to take rows again", || {
        traders.get_rows_amount() == 1
    })
    .await;

    reader.stop();
}

/// The MCP surface builds a row out of JSON and out of the schema the table
/// already holds. What has to hold is that the bytes it produces are the same
/// kind of bytes a client produces - so the row is written here through
/// `insert_or_replace_row` and read back through the **client's own**
/// `from_slice`, which is a different decoder from the one the server renders
/// with. A row only the server can read would be a row nobody's application
/// could.
#[tokio::test]
async fn a_row_written_through_mcp_is_read_back_by_the_client() {
    use mcp_server_middleware::McpToolCall;

    let (server, writer) = start_writer().await;

    // The schema reaches the table the only way it ever does: with a write from
    // the client that owns the entity.
    writer
        .insert_or_replace(&entity("acc-1", "from-the-client", "x"))
        .await
        .unwrap();

    server.app.open_mcp_writes(DateTimeAsMicroseconds::now());

    crate::mcp::InsertOrReplaceRowToolCallHandler::new(server.app.clone())
        .execute_tool_call(crate::mcp::InsertOrReplaceRowInputData {
            namespace: None,
            table_name: TestEntity::TABLE_NAME.to_string(),
            entity_json: r#"{
                "PartitionKey": "acc-1",
                "RowKey": "from-mcp",
                "Payload": "written as json",
                "Amount": 12.5,
                "Tags": ["a", "b"]
            }"#
            .to_string(),
            schema_id: None,
        })
        .await
        .unwrap();

    let row = writer
        .get_row("acc-1", "from-mcp")
        .await
        .unwrap()
        .expect("the row the MCP surface wrote is not there");

    assert_eq!(row.payload, "written as json");
    assert_eq!(row.amount, 12.5);
    assert_eq!(row.tags, vec!["a".to_string(), "b".to_string()]);
    // Not on the wire, so the client sees its own default - the same as for a
    // row any other writer produced.
    assert_eq!(row.computed_locally, 0);

    server.stop().await;
}
