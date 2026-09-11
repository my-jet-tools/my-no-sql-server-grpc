use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;

use crate::app::AppContext;
use crate::db_operations::DbOperationError;
use crate::http_server::{as_json, get_request_namespace_name};
use crate::persist::backup::BackupPartitionContent;

use super::models::GetBackupPartitionsInputContract;

/// The partition keys of one table inside one snapshot.
///
/// The answer is the `{amount, data}` object `/api/Partitions` gives, not a bare
/// array, so that the UI parses one model whether it is looking at a live table
/// or at an archived one - a page which had to tell the two answers apart would
/// be a page with two code paths over the same list.
#[http_route(
    method: "GET",
    route: "/api/Backup/Partitions",
    controller: "Backup",
    description: "Partition keys stored for a table inside a snapshot file, as `amount` - how many the snapshot holds - and `data`, the keys themselves",
    summary: "Returns the partition keys of a table inside a snapshot",
    input_data: "GetBackupPartitionsInputContract",
    result:[
        {status_code: 200, description: "An object: `amount` is how many partitions the snapshot holds for the table, `data` is their keys"},
        {status_code: 412, description: "Backups are not configured, the file is not there, it is not a backup of this server, or it has no such table"},
    ]
)]
pub struct GetBackupPartitionsAction {
    app: Arc<AppContext>,
}

impl GetBackupPartitionsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetBackupPartitionsAction,
    input_data: GetBackupPartitionsInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let name_space = get_request_namespace_name(ctx);

    let content =
        crate::db_operations::backup::inspect(&action.app, name_space, &input_data.file_name)
            .await
            .map_err(DbOperationError::BackupFailed)?;

    // A table missing from an archive is not a table missing from the database,
    // so this is a `BackupFailed` and not a `TableNotFound`: the message names
    // what the snapshot does hold, which is the answer somebody who mistyped a
    // name actually needs - and it is the same refusal the MCP tools give.
    let table =
        crate::mcp::find_backup_table(&content, &input_data.file_name, &input_data.table_name)
            .map_err(DbOperationError::BackupFailed)?;

    as_json(render(&table.partitions)).into_ok_result(true)
}

/// The answer itself, kept apart from the request so the shape can be exercised
/// without one.
///
/// `amount` and `data` are the names `/api/Partitions` uses and the UI
/// deserialises. `amount` counts what the snapshot holds for this table, which
/// here is also what `data` carries - an archive is read whole to be read at
/// all, so there is no window to name and nothing a second request could fetch.
fn render(partitions: &[BackupPartitionContent]) -> String {
    JsonObjectWriter::new()
        .write("amount", partitions.len())
        .write_json_array("data", |mut data| {
            for partition in partitions {
                data = data.write(partition.partition_key.as_str());
            }

            data
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partitions(partition_keys: &[&str]) -> Vec<BackupPartitionContent> {
        partition_keys
            .iter()
            .map(|partition_key| BackupPartitionContent {
                partition_key: partition_key.to_string(),
                blob: Vec::new(),
            })
            .collect()
    }

    /// `amount` and `data`, spelled exactly so and in the shape
    /// `/api/Partitions` answers in: the UI parses one model off both routes,
    /// and a snapshot answering differently is a blank list on one of the two
    /// pages with nothing in the log to explain it.
    #[test]
    fn the_answer_names_the_count_and_the_keys() {
        assert_eq!(
            render(&partitions(&["acc-1", "acc-2"])),
            r#"{"amount":2,"data":["acc-1","acc-2"]}"#
        );
    }

    /// A table the archive carries no partitions for answers the same shape
    /// rather than nothing - the page has to render it as an empty table, not as
    /// a failure.
    #[test]
    fn a_table_with_no_partitions_answers_the_same_shape() {
        assert_eq!(render(&[]), r#"{"amount":0,"data":[]}"#);
    }

    /// The keys come back as the archive stored them, escaping included: a
    /// partition key is a client's string, and a quote in one must not end the
    /// JSON string early.
    #[test]
    fn a_key_that_needs_escaping_is_escaped() {
        assert_eq!(
            render(&partitions(&[r#"acc "one""#])),
            r#"{"amount":1,"data":["acc \"one\""]}"#
        );
    }
}
