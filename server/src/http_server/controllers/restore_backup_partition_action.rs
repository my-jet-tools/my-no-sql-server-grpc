use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;
use crate::db_operations::backup::RestoreOne;
use crate::http_server::{as_json, get_request_namespace_name};

use super::models::RestoreBackupPartitionInputContract;

/// Puts one partition of one table back from a snapshot.
///
/// Both names are required and neither means anything alone: a table without a
/// partition would read as "restore this table", which is not what this does -
/// the whole archive is the other route. What comes back **replaces** the
/// partition that is there, and the archive's schemas ride along with the
/// table's attributes, because a partition without the schema it was written
/// under is a partition nobody can show.
#[http_route(
    method: "POST",
    route: "/api/Backup/RestorePartition",
    controller: "Backup",
    description: "Restores one partition of one table from a snapshot. The partition replaces the one in the table, and the archive's schemas are merged into the table attributes",
    summary: "Restores one partition from a snapshot",
    input_data: "RestoreBackupPartitionInputContract",
    result:[
        {status_code: 200, description: "The partition is back"},
        {status_code: 403, description: "The UI write window is shut"},
        {status_code: 404, description: "The snapshot holds no such partition of that table"},
        {status_code: 412, description: "Backups are not configured, or there is no such snapshot, or it is not a snapshot of this server"},
    ]
)]
pub struct RestoreBackupPartitionAction {
    app: Arc<AppContext>,
}

impl RestoreBackupPartitionAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &RestoreBackupPartitionAction,
    input_data: RestoreBackupPartitionInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    if !action.app.ui_writes.is_open() {
        return Err(HttpFailResult::as_forbidden(Some(
            "UI writes are shut. Open them with `POST /api/Settings/UiWrites?enabled=true` - \
             Settings -> Write access in the UI - which opens them for 10 minutes."
                .to_string(),
        )));
    }

    // Off the request and not through the live namespaces, the same as the
    // whole-namespace restore: the archive is what is being read, and
    // `backup::restore` resolves the destination itself.
    let name_space = get_request_namespace_name(ctx);

    let now = DateTimeAsMicroseconds::now();

    let restored = crate::db_operations::backup::restore(
        &action.app,
        name_space,
        &input_data.file_name,
        Some(RestoreOne {
            table_name: input_data.table_name.clone(),
            partition_key: input_data.partition_key.clone(),
        }),
        DataSyncPeriod::Sec5.get_sync_moment(now),
    )
    .await?;

    let json = render(
        restored,
        &input_data.file_name,
        &input_data.table_name,
        &input_data.partition_key,
    )
    .map_err(|err| HttpFailResult::as_not_found(err, false))?;

    as_json(json).into_ok_result(true)
}

/// The answer, or the message saying the archive does not hold what was named.
///
/// A restore which put nothing back is not a quiet success here, which is the
/// one place this route differs from `DELETE /api/Row`: deleting a key that is
/// not there leaves the caller with what they wanted, while naming a partition
/// to bring back and getting none leaves them believing they have rows they do
/// not have. The page this is called from lists what the archive holds, so a
/// name that is not in it is a stale page or a typo either way.
///
/// It answers with a plain message rather than an `HttpFailResult` for the same
/// reason the bulk-delete body parser does: what the archive holds is not an
/// HTTP concern, and keeping it out of the signature keeps this testable
/// without a request.
fn render(
    restored: usize,
    file_name: &str,
    table_name: &str,
    partition_key: &str,
) -> Result<String, String> {
    if restored == 0 {
        return Err(format!(
            "The backup '{file_name}' has no partition '{partition_key}' of table '{table_name}'"
        ));
    }

    // The same field the whole-namespace restore answers with, so one model
    // reads both routes.
    Ok(JsonObjectWriter::new()
        .write("partitionsRestored", restored)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_the_same_field_the_whole_restore_answers_with() {
        assert_eq!(
            render(1, "20260911T120000.zip", "traders", "acc-1"),
            Ok(r#"{"partitionsRestored":1}"#.to_string())
        );
    }

    /// The reason this function exists: nothing restored has to reach the caller
    /// as a refusal, and the message has to name all three things they gave -
    /// which of them is wrong is exactly what they can not see from here.
    #[test]
    fn a_partition_the_archive_does_not_hold_is_a_refusal() {
        assert_eq!(
            render(0, "20260911T120000.zip", "traders", "acc-1"),
            Err(
                "The backup '20260911T120000.zip' has no partition 'acc-1' of table 'traders'"
                    .to_string()
            )
        );
    }
}
