use ahash::AHashMap;

use crate::app::AppContext;

use super::metrics_writer::{MetricKind, MetricsWriter};

/// Renders the whole metric set from live state.
///
/// Computed **per scrape**, not pushed by a timer. Every number below is an
/// `ArcSwap` load, an atomic, or one read lock over counters the partitions
/// already maintain, so a scrape costs less than a timer tick would - and it
/// buys the thing a stored registry gets wrong: a table which was deleted stops
/// being reported the same second, instead of holding its last value until the
/// process restarts.
pub fn render(app: &AppContext, http_connections: i64) -> String {
    let mut writer = MetricsWriter::new();

    write_server(app, http_connections, &mut writer);
    write_namespaces(app, &mut writer);
    write_readers(app, &mut writer);

    writer.build()
}

fn write_server(app: &AppContext, http_connections: i64, writer: &mut MetricsWriter) {
    writer.family(
        "mynosql_build_info",
        "Always 1. What it carries are its labels.",
        MetricKind::Gauge,
        |family| {
            family.sample(
                &[
                    ("version", crate::app::APP_VERSION),
                    ("location", app.settings.location.as_str()),
                ],
                1,
            );
        },
    );

    writer.family(
        "mynosql_initialized",
        "1 once the tables are loaded from disk and the server answers.",
        MetricKind::Gauge,
        |family| {
            family.sample(&[], i32::from(app.states.is_initialized()));
        },
    );

    writer.family(
        "mynosql_http_connections",
        "Open connections on the HTTP port.",
        MetricKind::Gauge,
        |family| {
            family.sample(&[], http_connections);
        },
    );

    writer.family(
        "mynosql_transactions_open",
        "Transactions which are still being built. None of them has reached a table.",
        MetricKind::Gauge,
        |family| {
            family.sample(&[], app.transactions.get_all().len());
        },
    );
}

fn write_namespaces(app: &AppContext, writer: &mut MetricsWriter) {
    let mut namespaces = app.namespaces.get_all();
    // The registry is a hash map: sorting is what makes two scrapes comparable
    // by eye and the test assertable.
    namespaces.sort_by(|left, right| left.name.cmp(&right.name));

    // Collected once and rendered several times: each family is a separate block
    // of the exposition format, and asking the tables again per family would be
    // one round of locks per metric, each answering about a different moment.
    struct Collected {
        name_space: String,
        tables: Vec<(String, my_no_sql_grpc_core::db::DbTableMetrics, usize)>,
        persist_queue_partitions: usize,
        persist_queue_tables_metadata: usize,
        persisted_total: u64,
    }

    let collected: Vec<Collected> = namespaces
        .iter()
        .map(|db_namespace| {
            let queue = db_namespace.persist_markers.get_queue_metrics();

            let mut tables: Vec<(String, my_no_sql_grpc_core::db::DbTableMetrics, usize)> =
                db_namespace
                    .tables
                    .get_tables()
                    .iter()
                    .map(|db_table| {
                        (
                            db_table.name.clone(),
                            db_table.get_metrics(),
                            db_table.get_attributes().schemas.len(),
                        )
                    })
                    .collect();

            tables.sort_by(|left, right| left.0.cmp(&right.0));

            Collected {
                name_space: db_namespace.name.clone(),
                tables,
                persist_queue_partitions: queue.partitions,
                persist_queue_tables_metadata: queue.tables_metadata,
                persisted_total: db_namespace.get_persisted_total(),
            }
        })
        .collect();

    writer.family(
        "mynosql_namespaces",
        "Namespaces this server holds.",
        MetricKind::Gauge,
        |family| {
            family.sample(&[], collected.len());
        },
    );

    writer.family(
        "mynosql_tables",
        "Tables of the namespace.",
        MetricKind::Gauge,
        |family| {
            for itm in collected.iter() {
                family.sample(&[("ns", itm.name_space.as_str())], itm.tables.len());
            }
        },
    );

    writer.family(
        "mynosql_table_schemas",
        "Entity schemas the table's rows were written under. One is the normal answer; anything else is a deploy going through or two entities aimed at one table.",
        MetricKind::Gauge,
        |family| {
            for itm in collected.iter() {
                for (table_name, _, schemas) in itm.tables.iter() {
                    family.sample(
                        &[("ns", itm.name_space.as_str()), ("table", table_name)],
                        *schemas,
                    );
                }
            }
        },
    );

    writer.family(
        "mynosql_table_partitions",
        "Partitions of the table.",
        MetricKind::Gauge,
        |family| {
            for itm in collected.iter() {
                for (table_name, metrics, _) in itm.tables.iter() {
                    family.sample(
                        &[("ns", itm.name_space.as_str()), ("table", table_name)],
                        metrics.partitions_amount,
                    );
                }
            }
        },
    );

    writer.family(
        "mynosql_table_rows",
        "Rows of the table.",
        MetricKind::Gauge,
        |family| {
            for itm in collected.iter() {
                for (table_name, metrics, _) in itm.tables.iter() {
                    family.sample(
                        &[("ns", itm.name_space.as_str()), ("table", table_name)],
                        metrics.rows_amount,
                    );
                }
            }
        },
    );

    writer.family(
        "mynosql_table_content_size_bytes",
        "Stored size of the table's rows.",
        MetricKind::Gauge,
        |family| {
            for itm in collected.iter() {
                for (table_name, metrics, _) in itm.tables.iter() {
                    family.sample(
                        &[("ns", itm.name_space.as_str()), ("table", table_name)],
                        metrics.content_size,
                    );
                }
            }
        },
    );

    writer.family(
        "mynosql_persist_queue_partitions",
        "Partitions waiting to be written to disk.",
        MetricKind::Gauge,
        |family| {
            for itm in collected.iter() {
                family.sample(
                    &[("ns", itm.name_space.as_str())],
                    itm.persist_queue_partitions,
                );
            }
        },
    );

    writer.family(
        "mynosql_persist_queue_tables_metadata",
        "Table attributes waiting to be written to disk.",
        MetricKind::Gauge,
        |family| {
            for itm in collected.iter() {
                family.sample(
                    &[("ns", itm.name_space.as_str())],
                    itm.persist_queue_tables_metadata,
                );
            }
        },
    );

    writer.family(
        "mynosql_persist_tasks_written_total",
        "Persist tasks written since start up. A queue which is not draining has a total which is not moving.",
        MetricKind::Counter,
        |family| {
            for itm in collected.iter() {
                family.sample(&[("ns", itm.name_space.as_str())], itm.persisted_total);
            }
        },
    );
}

/// Readers are grouped by what they *are*, not by which session they happen to
/// hold: a session id is minted per greeting, so labelling by it would leave a
/// dead time series behind every reconnect, forever. What an operator asks is
/// "is this application keeping up", and the group answers it.
fn write_readers(app: &AppContext, writer: &mut MetricsWriter) {
    #[derive(Default)]
    struct Group {
        sessions: usize,
        subscriptions: usize,
        pending_chunks: usize,
        pending_chunks_max: usize,
    }

    let mut groups: AHashMap<(String, String, String), Group> = AHashMap::new();

    for reader in super::readers::collect(app) {
        let group = groups
            .entry((reader.namespace, reader.app_name, reader.version))
            .or_default();

        group.sessions += 1;
        group.subscriptions += reader.tables.len();
        group.pending_chunks += reader.pending_chunks;
        group.pending_chunks_max = group.pending_chunks_max.max(reader.pending_chunks);
    }

    let mut groups: Vec<((String, String, String), Group)> = groups.into_iter().collect();
    groups.sort_by(|left, right| left.0.cmp(&right.0));

    fn labels(key: &(String, String, String)) -> [(&'static str, &str); 3] {
        [
            ("ns", key.0.as_str()),
            ("app", key.1.as_str()),
            ("version", key.2.as_str()),
        ]
    }

    writer.family(
        "mynosql_reader_sessions",
        "Live reader sessions.",
        MetricKind::Gauge,
        |family| {
            for (key, group) in groups.iter() {
                family.sample(&labels(key), group.sessions);
            }
        },
    );

    writer.family(
        "mynosql_reader_subscriptions",
        "Table subscriptions held by those sessions.",
        MetricKind::Gauge,
        |family| {
            for (key, group) in groups.iter() {
                family.sample(&labels(key), group.subscriptions);
            }
        },
    );

    writer.family(
        "mynosql_reader_pending_chunks",
        "Chunks queued for those sessions and not yet collected.",
        MetricKind::Gauge,
        |family| {
            for (key, group) in groups.iter() {
                family.sample(&labels(key), group.pending_chunks);
            }
        },
    );

    writer.family(
        "mynosql_reader_pending_chunks_max",
        "The worst of them. One reader stuck behind a big batch is invisible in a sum.",
        MetricKind::Gauge,
        |family| {
            for (key, group) in groups.iter() {
                family.sample(&labels(key), group.pending_chunks_max);
            }
        },
    );
}
