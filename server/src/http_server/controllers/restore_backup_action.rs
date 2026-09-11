use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;
use crate::http_server::{as_json, get_request_namespace_name};

use super::models::RestoreBackupInputContract;

/// Puts a whole namespace back from one of its snapshots.
///
/// A backup is a zip of one namespace, so restoring one is one file and the
/// route needs nothing but its name. What comes back **replaces** what is
/// there - a restored partition holds the archive's rows and nothing else -
/// and the schemas ride along in the table attributes, which are merged rather
/// than replaced, so a table keeps what it has learned since the archive was
/// taken.
#[http_route(
    method: "POST",
    route: "/api/Backup/RestoreFromBackup",
    controller: "Backup",
    description: "Restores the whole namespace from one of its snapshots. Every partition the archive holds replaces the one in the table, and the archive's schemas are merged into the table attributes",
    summary: "Restores the whole namespace from a snapshot",
    input_data: "RestoreBackupInputContract",
    result:[
        {status_code: 200, description: "How many partitions were put back"},
        {status_code: 403, description: "The UI write window is shut"},
        {status_code: 412, description: "Backups are not configured, or there is no such snapshot, or it is not a snapshot of this server"},
    ]
)]
pub struct RestoreBackupAction {
    app: Arc<AppContext>,
}

impl RestoreBackupAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &RestoreBackupAction,
    input_data: RestoreBackupInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    if !action.app.ui_writes.is_open() {
        return Err(HttpFailResult::as_forbidden(Some(
            "UI writes are shut. Open them with `POST /api/Settings/UiWrites?enabled=true` - \
             Settings -> Write access in the UI - which opens them for 10 minutes."
                .to_string(),
        )));
    }

    // The namespace is taken off the request and **not** resolved through the
    // live ones: an archive outlives the namespace it was taken of, and putting
    // a deleted namespace back is exactly what this route is for.
    // `backup::restore` brings it into existence itself.
    let name_space = get_request_namespace_name(ctx);

    let now = DateTimeAsMicroseconds::now();

    // Five seconds, not a `syncPeriod` the caller picks. A restore is a bulk
    // write of a whole namespace, and the one thing nobody wants to ask for is
    // all of it on the disk before the call answers; the MCP restore tool fixes
    // it at the same period for the same reason.
    let partitions_restored = crate::db_operations::backup::restore(
        &action.app,
        name_space,
        &input_data.file_name,
        None,
        DataSyncPeriod::Sec5.get_sync_moment(now),
    )
    .await?;

    as_json(render(partitions_restored)).into_ok_result(true)
}

/// What actually came back, kept apart from the request so the shape can be
/// exercised without one. The same shape `/api/Backup/RestorePartition`
/// answers with: the caller of either route is asking one question, and two
/// spellings of the answer would be parsed twice.
fn render(partitions_restored: usize) -> String {
    JsonObjectWriter::new()
        .write("partitionsRestored", partitions_restored)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_how_many_partitions_came_back() {
        assert_eq!(render(3), r#"{"partitionsRestored":3}"#);
    }

    /// An archive with nothing in it answers a number rather than an error:
    /// the caller named a file, the file was read, and that it holds no
    /// partition is the archive's fact and not a wrong request. This server
    /// never writes such an archive - an empty namespace is skipped - but an
    /// uploaded one can be, and hiding the zero would report a restore that
    /// restored nothing.
    #[test]
    fn nothing_in_the_archive_is_still_an_answer() {
        assert_eq!(render(0), r#"{"partitionsRestored":0}"#);
    }
}
