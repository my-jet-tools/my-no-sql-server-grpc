use my_json::json_writer::JsonObjectWriter;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;

/// One reader session, flattened out of the registry.
///
/// Everything is copied out first and rendered afterwards: the session's own
/// mutex is the one the long poll pops from and every write pushes into, so
/// nothing that formats JSON may still be holding it.
pub struct ReaderView {
    pub id: String,
    pub app_name: String,
    pub version: String,
    pub namespace: String,
    pub ip: String,
    pub connected: DateTimeAsMicroseconds,
    pub last_incoming: DateTimeAsMicroseconds,
    pub tables: Vec<String>,
    pub pending_chunks: usize,
}

/// Every live reader session, in a stable order.
///
/// A session can be collected between the snapshot and the answer - the rows
/// hold no lock on it and one that died a microsecond ago is indistinguishable
/// from one that dies a microsecond later, so there is nothing here to close.
pub fn collect(app: &AppContext) -> Vec<ReaderView> {
    let mut result: Vec<ReaderView> = app
        .reader_sessions
        .get_all()
        .into_iter()
        .map(|session| {
            let (tables, pending_chunks) = session.get_monitoring_snapshot();

            ReaderView {
                id: session.id.clone(),
                app_name: session.app_name.clone(),
                version: session.version.clone(),
                namespace: session.namespace.clone(),
                ip: session.ip.clone(),
                connected: session.connected,
                last_incoming: session.get_last_incoming(),
                tables,
                pending_chunks,
            }
        })
        .collect();

    // The registry is a hash map, so without this two samples taken a second
    // apart would be in different orders and impossible to compare.
    result.sort_by(|left, right| {
        (&left.namespace, &left.app_name, &left.id).cmp(&(
            &right.namespace,
            &right.app_name,
            &right.id,
        ))
    });

    result
}

/// One row, the same shape wherever it is shown.
pub fn write(
    writer: JsonObjectWriter,
    reader: &ReaderView,
    now: DateTimeAsMicroseconds,
) -> JsonObjectWriter {
    writer
        .write("id", reader.id.as_str())
        .write("name", reader.app_name.as_str())
        .write("version", reader.version.as_str())
        .write("namespace", reader.namespace.as_str())
        .write("ip", reader.ip.as_str())
        .write("connectedAt", reader.connected.to_rfc3339())
        // A number of seconds rather than a rendered duration: the question is
        // "is this reader still asking", and a caller has to be able to compare
        // the answer against a threshold.
        .write(
            "lastIncomingSecsAgo",
            super::secs_ago(now, reader.last_incoming),
        )
        .write("pendingChunks", reader.pending_chunks)
        .write_json_array("tables", |tables| {
            let mut tables = tables;

            for table_name in reader.tables.iter() {
                tables = tables.write(table_name.as_str());
            }

            tables
        })
}
