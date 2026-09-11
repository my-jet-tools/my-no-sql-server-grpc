use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonArrayWriter;

use crate::app::AppContext;
use crate::db_operations::DbOperationError;
use crate::http_server::{as_json, get_request_namespace_name};
use crate::persist::backup::BackupTableContent;

use super::models::GetBackupTablesInputContract;

/// The tables inside one snapshot, without restoring any of it.
///
/// The archive is read whole to answer this - a zip has no index this server
/// keeps separately - which is also why the route exists at all: the alternative
/// for somebody wanting to know what a snapshot holds is restoring it and
/// looking, and a restore replaces live data.
#[http_route(
    method: "GET",
    route: "/api/Backup/Tables",
    controller: "Backup",
    description: "The tables stored inside a snapshot file, without restoring any of it",
    summary: "Returns the tables inside a snapshot",
    input_data: "GetBackupTablesInputContract",
    result:[
        {status_code: 200, description: "The tables as an array of `name` and `partitionsCount`"},
        {status_code: 412, description: "Backups are not configured, the file is not there, or it is not a backup of this server"},
    ]
)]
pub struct GetBackupTablesAction {
    app: Arc<AppContext>,
}

impl GetBackupTablesAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetBackupTablesAction,
    input_data: GetBackupTablesInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let name_space = get_request_namespace_name(ctx);

    let content =
        crate::db_operations::backup::inspect(&action.app, name_space, &input_data.file_name)
            .await
            .map_err(DbOperationError::BackupFailed)?;

    as_json(render(&content.tables)).into_ok_result(true)
}

/// The answer itself, kept apart from the request so the shape can be exercised
/// without one.
///
/// `name` and `partitionsCount` are what the UI's `SnapshotTableApiModel`
/// deserialises. The count is of the partitions **the archive carries**, not of
/// the ones the live table has now: it is how the snapshots page decides whether
/// there is anything to open, and the two numbers drift apart the moment
/// anything is written after a backup.
fn render(tables: &[BackupTableContent]) -> String {
    let mut writer = JsonArrayWriter::new();

    for table in tables {
        writer = writer.write_json_object(|itm| {
            itm.write("name", table.table_name.as_str())
                .write("partitionsCount", table.partitions.len())
        });
    }

    writer.build()
}

#[cfg(test)]
mod tests {
    use crate::persist::backup::BackupPartitionContent;

    use super::*;

    fn partition(partition_key: &str) -> BackupPartitionContent {
        BackupPartitionContent {
            partition_key: partition_key.to_string(),
            blob: Vec::new(),
        }
    }

    /// The attributes are not read here on purpose: a table whose attributes the
    /// archive did not carry is still a table with partitions in it, and the
    /// snapshots page has to list it.
    fn table(table_name: &str, partition_keys: &[&str]) -> BackupTableContent {
        BackupTableContent {
            table_name: table_name.to_string(),
            attributes: None,
            partitions: partition_keys.iter().copied().map(partition).collect(),
        }
    }

    /// `name` and `partitionsCount`, spelled exactly so: the UI deserialises
    /// those two names off this route, and renaming either of them is an empty
    /// table list with nothing in the log to explain it.
    #[test]
    fn every_table_is_named_and_counted() {
        assert_eq!(
            render(&[
                table("traders", &["acc-1", "acc-2"]),
                table("instruments", &["all"]),
            ]),
            r#"[{"name":"traders","partitionsCount":2},{"name":"instruments","partitionsCount":1}]"#
        );
    }

    /// A backup of a namespace whose tables were all empty carries no tables,
    /// and the page still parses one model off this route.
    #[test]
    fn a_snapshot_with_no_tables_is_an_empty_array() {
        assert_eq!(render(&[]), "[]");
    }

    /// A table the backup carries no partitions for counts zero rather than
    /// being left out: it is in the archive, and a list that silently drops it
    /// says the snapshot is missing a table it actually has.
    #[test]
    fn a_table_with_no_partitions_is_still_a_table() {
        assert_eq!(
            render(&[table("traders", &[])]),
            r#"[{"name":"traders","partitionsCount":0}]"#
        );
    }
}
