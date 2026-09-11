use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonArrayWriter;

use crate::app::AppContext;
use crate::db_operations::DbOperationError;
use crate::http_server::{as_json, get_request_namespace_name};
use crate::persist::backup::BackupOnDisk;

use super::models::GetBackupsInputContract;

/// The snapshots a namespace has, which is where the page that browses one
/// starts.
///
/// The namespace is **not** resolved through the live ones, which is why this is
/// the one read on this surface that does not call `get_request_namespace`: an
/// archive outlives the namespace it was taken of, and answering 404 for the
/// snapshots of a namespace somebody has just deleted would hide the only copy
/// left of it.
#[http_route(
    method: "GET",
    route: "/api/Backup/List",
    controller: "Backup",
    description: "The snapshot files the namespace has, oldest first - the name to address one by, and its size in bytes",
    summary: "Returns the snapshots of the namespace",
    input_data: "GetBackupsInputContract",
    result:[
        {status_code: 200, description: "The snapshots as an array of `name` and `size`"},
        {status_code: 412, description: "Backups are not configured, or their folder can not be listed"},
    ]
)]
pub struct GetBackupsAction {
    app: Arc<AppContext>,
}

impl GetBackupsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetBackupsAction,
    _input_data: GetBackupsInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let name_space = get_request_namespace_name(ctx);

    // Nothing backed up yet is an empty list, and nowhere to put backups at all
    // is a refusal. The operation already tells the two apart - answering the
    // second with an empty array would read as the first, and an operator who
    // never set `BackupsDest` would go on believing the timer is running.
    let backups = crate::db_operations::backup::get_all(&action.app, name_space)
        .await
        .map_err(DbOperationError::BackupFailed)?;

    as_json(render(&backups)).into_ok_result(true)
}

/// The answer itself, kept apart from the request so the shape can be exercised
/// without one.
///
/// `name` and `size` are the two names the UI's `SnapshotFileApiModel`
/// deserialises, and `name` is also what every other backup route addresses a
/// file by - so the spelling is as much of a contract as the route.
fn render(backups: &[BackupOnDisk]) -> String {
    let mut writer = JsonArrayWriter::new();

    for backup in backups {
        writer = writer.write_json_object(|file| {
            file.write("name", backup.name.as_str())
                .write("size", backup.size)
        });
    }

    writer.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backup(name: &str, size: u64) -> BackupOnDisk {
        BackupOnDisk {
            name: name.to_string(),
            size,
        }
    }

    /// `name` and `size`, spelled exactly so: the UI deserialises those two
    /// names off this route, and renaming either of them is an empty snapshots
    /// page with nothing in the log to explain it.
    #[test]
    fn every_snapshot_is_named_and_measured() {
        assert_eq!(
            render(&[
                backup("20260810T071415.zip", 4096),
                backup("20260810T071415_02.zip", 512),
            ]),
            r#"[{"name":"20260810T071415.zip","size":4096},{"name":"20260810T071415_02.zip","size":512}]"#
        );
    }

    /// A namespace nobody has backed up yet answers the array the UI parses,
    /// empty. This is the one empty answer this route may give - backups being
    /// unconfigured is a refusal instead, which is what keeps the two apart.
    #[test]
    fn a_namespace_with_no_snapshots_is_an_empty_array() {
        assert_eq!(render(&[]), "[]");
    }

    /// The order is the operation's, oldest first, because a snapshot name is
    /// the moment it was taken - the list must not reshuffle it into something
    /// that no longer reads as a history.
    #[test]
    fn the_order_is_the_one_the_operation_handed_over() {
        let rendered = render(&[
            backup("20260810T071415.zip", 1),
            backup("20260811T071415.zip", 1),
        ]);

        let older = rendered.find("20260810T071415.zip").unwrap();
        let newer = rendered.find("20260811T071415.zip").unwrap();

        assert!(older < newer);
    }
}
