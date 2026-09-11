use my_json::json_writer::{JsonArrayWriter, JsonObjectWriter};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;

/// Everything this server can say about itself, as one answer.
///
/// It covers **every** namespace rather than taking one by name. A namespace is
/// the unit of everything here - its own tables, its own folder and its own
/// persist queue - so a status of one of them is not a status of the server; and a monitoring call which 404s on a namespace name that is not
/// there is a monitoring call nobody can rely on.
///
/// The only lock taken is a table's read lock, once per table, through
/// `DbTable::get_metrics`. Nothing walks rows, nothing allocates a partition
/// key, and the persist repository - whose mutex is held across file I/O - is
/// not touched at all.
pub fn render(app: &AppContext, now: DateTimeAsMicroseconds) -> String {
    JsonObjectWriter::new()
        .write_json_object("server", |server| write_server(app, server, now))
        .write_json_array("namespaces", |namespaces| write_namespaces(app, namespaces))
        .write_json_array("readers", |readers| {
            let mut readers = readers;

            for reader in super::readers::collect(app).iter() {
                readers =
                    readers.write_json_object(|writer| super::readers::write(writer, reader, now));
            }

            readers
        })
        .write_json_array("transactions", |transactions| {
            write_transactions(app, transactions, now)
        })
        .build()
}

fn write_server(
    app: &AppContext,
    writer: JsonObjectWriter,
    now: DateTimeAsMicroseconds,
) -> JsonObjectWriter {
    writer
        .write("name", crate::app::APP_NAME)
        .write("version", crate::app::APP_VERSION)
        .write("location", app.settings.location.as_str())
        .write("startedAt", app.created.to_rfc3339())
        .write("upTimeSecs", super::secs_ago(now, app.created))
        .write("grpcPort", crate::consts::GRPC_PORT)
        .write("httpPort", crate::consts::HTTP_PORT)
        .write("compressData", app.settings.compress_data)
        .write("persistenceDest", app.settings.get_persistence_dest())
        .write_json_object("backups", |backups| {
            backups
                .write("configured", app.backups.is_configured())
                .write_if_some("intervalSecs", app.settings.backup_interval_secs)
                .write_if_some("maxBackups", app.settings.max_backups)
        })
        // A window nobody can see is a window nobody remembers to shut. It is
        // here rather than in a page of its own because "what is this server
        // doing right now" is the question this route already answers.
        .write_json_object("mcpWrites", |mcp| {
            let remaining = app.mcp_writes_remaining_secs(now);

            mcp.write("open", remaining.is_some())
                .write_if_some("remainingSecs", remaining)
        })
}

fn write_namespaces(app: &AppContext, writer: JsonArrayWriter) -> JsonArrayWriter {
    let mut writer = writer;

    let mut namespaces = app.namespaces.get_all();
    namespaces.sort_by(|left, right| left.name.cmp(&right.name));

    for db_namespace in namespaces {
        let db_tables = db_namespace.tables.get_tables();

        // Collected once and rolled up from what was collected: asking the
        // tables again for the namespace totals would be a second round of locks
        // answering about a different moment.
        let mut tables: Vec<_> = db_tables
            .iter()
            .map(|db_table| {
                (
                    db_table.clone(),
                    db_table.get_attributes(),
                    db_table.get_metrics(),
                )
            })
            .collect();

        tables.sort_by(|left, right| left.0.name.cmp(&right.0.name));

        let queue = db_namespace.persist_markers.get_queue_metrics();

        writer = writer.write_json_object(|namespace| {
            namespace
                .write("name", db_namespace.name.as_str())
                .write("tablesCount", tables.len())
                .write(
                    "partitionsCount",
                    tables
                        .iter()
                        .map(|(_, _, metrics)| metrics.partitions_amount)
                        .sum::<usize>(),
                )
                .write(
                    "rowsCount",
                    tables
                        .iter()
                        .map(|(_, _, metrics)| metrics.rows_amount)
                        .sum::<usize>(),
                )
                .write(
                    "dataSize",
                    tables
                        .iter()
                        .map(|(_, _, metrics)| metrics.content_size)
                        .sum::<usize>(),
                )
                .write_json_object("persistQueue", |persist_queue| {
                    persist_queue
                        .write("partitions", queue.partitions)
                        .write("tablesMetadata", queue.tables_metadata)
                        .write_if_some(
                            "lastPersistedAt",
                            db_namespace
                                .get_last_persisted()
                                .map(|moment| moment.to_rfc3339()),
                        )
                })
                .write_json_array("tables", |writer| {
                    let mut writer = writer;

                    for (db_table, attributes, metrics) in tables.iter() {
                        writer = writer.write_json_object(|table| {
                            table
                                .write("name", db_table.name.as_str())
                                .write("persist", attributes.persist)
                                .write_if_some(
                                    "maxPartitionsAmount",
                                    attributes.max_partitions_amount,
                                )
                                .write_if_some(
                                    "maxRowsPerPartitionAmount",
                                    attributes.max_rows_per_partition_amount,
                                )
                                .write("partitionsCount", metrics.partitions_amount)
                                .write("rowsCount", metrics.rows_amount)
                                // One table is one entity in about every real
                                // case, so anything but 1 is worth seeing: it
                                // is either a deploy going through or two
                                // different entities aimed at one table.
                                .write("schemasCount", attributes.schemas.len())
                                .write("dataSize", metrics.content_size)
                                .write("created", attributes.created.to_rfc3339())
                                // Absent means nothing has written to it since
                                // the process started - the moment is not on
                                // disk, so a restart forgets it rather than
                                // reporting the previous run's.
                                .write_if_some(
                                    "lastWriteAt",
                                    db_table
                                        .get_last_write_moment()
                                        .map(|moment| moment.to_rfc3339()),
                                )
                        });
                    }

                    writer
                })
        });
    }

    writer
}

fn write_transactions(
    app: &AppContext,
    writer: JsonArrayWriter,
    now: DateTimeAsMicroseconds,
) -> JsonArrayWriter {
    let mut writer = writer;

    let mut transactions = app.transactions.get_all();
    transactions.sort_by(|left, right| left.id.cmp(&right.id));

    for transaction in transactions {
        writer = writer.write_json_object(|itm| {
            itm.write("id", transaction.id.as_str())
                .write("namespace", transaction.namespace.as_str())
                .write("table", transaction.table_name.as_str())
                .write("actions", transaction.get_actions_amount())
                .write("startedAt", transaction.started.to_rfc3339())
                .write(
                    "lastIncomingSecsAgo",
                    super::secs_ago(now, transaction.get_last_incoming()),
                )
        });
    }

    writer
}
