use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::db_operations::DbOperationError;
use crate::http_server::{as_json, get_request_namespace};

use super::models::MakeBackupInputContract;

/// Takes a snapshot of one namespace now, without waiting for the timer.
///
/// A backup here is a zip of **one** namespace, so the namespace the request
/// names is the whole of what the caller decides - and it is also the only
/// namespace touched. The timer takes every namespace because that is its job;
/// a button on a page showing one namespace must not spend another namespace's
/// `MaxBackups` slot.
///
/// The namespace has to be a live one: a snapshot is taken from memory, so there
/// is nothing to snapshot in a name nobody has written to. That is why this is
/// the one backup route which resolves through the live namespaces rather than
/// straight off the request - the restore routes deliberately do not, because an
/// archive outlives the namespace it was taken of.
#[http_route(
    method: "POST",
    route: "/api/Backup/MakeBackup",
    controller: "Backup",
    description: "Takes a snapshot of the namespace now, without waiting for the backup timer, and answers with the file it wrote - `name` and `size`, the same shape `/api/Backup/List` returns",
    summary: "Takes a snapshot of the namespace now",
    input_data: "MakeBackupInputContract",
    result:[
        {status_code: 200, description: "The snapshot that was taken: `name` and `size`"},
        {status_code: 403, description: "The UI write window is shut"},
        {status_code: 404, description: "Namespace is not found"},
        {status_code: 412, description: "Backups are not configured, the namespace holds no tables, or the file could not be written"},
    ]
)]
pub struct MakeBackupAction {
    app: Arc<AppContext>,
}

impl MakeBackupAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &MakeBackupAction,
    _input_data: MakeBackupInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    // A snapshot writes a file and, once the timer next tidies up, costs the
    // oldest snapshot of this namespace its place. That puts it behind the same
    // window every other UI write is behind.
    if !action.app.ui_writes.is_open() {
        return Err(HttpFailResult::as_forbidden(Some(
            "UI writes are shut. Open them with `POST /api/Settings/UiWrites?enabled=true` - \
             Settings -> Write access in the UI - which opens them for 10 minutes."
                .to_string(),
        )));
    }

    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let taken = crate::db_operations::backup::make_one(
        &action.app,
        &db_namespace,
        DateTimeAsMicroseconds::now(),
    )
    .await
    .map_err(DbOperationError::BackupFailed)?;

    as_json(render(&taken.name, taken.size)).into_ok_result(true)
}

/// The row the UI puts straight into its snapshot table, in the shape
/// `/api/Backup/List` answers with: `SnapshotFileApiModel` reads `name` and
/// `size`, and a page which has just taken a snapshot should not have to reload
/// the list to show it. Kept apart from the request so the shape can be
/// exercised without one.
fn render(name: &str, size: u64) -> String {
    JsonObjectWriter::new()
        .write("name", name)
        .write("size", size)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `name` and `size`, spelled exactly so: `SnapshotFileApiModel` in the UI
    /// deserialises those two names off both this route and the list, and
    /// renaming either of them is a row that never appears.
    #[test]
    fn the_answer_is_the_row_the_snapshot_list_shows() {
        assert_eq!(
            render("20260911T120000.zip", 40_960),
            r#"{"name":"20260911T120000.zip","size":40960}"#
        );
    }

    /// The size is a JSON number, not a string: the UI field is an `i64`, and a
    /// quoted number fails to deserialise - which takes the whole answer with
    /// it, not just the size. An archive past four gigabytes is still one
    /// number, so nothing here may narrow it to a `u32`.
    #[test]
    fn the_size_is_a_number_whatever_it_is() {
        assert_eq!(render("a.zip", 0), r#"{"name":"a.zip","size":0}"#);
        assert_eq!(
            render("a.zip", 5_000_000_000),
            r#"{"name":"a.zip","size":5000000000}"#
        );
    }
}
