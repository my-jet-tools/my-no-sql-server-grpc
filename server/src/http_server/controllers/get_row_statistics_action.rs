use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;

use crate::app::AppContext;
use crate::http_server::{as_json, get_request_namespace};

use super::models::GetRowStatisticsInputContract;

/// Why a row is still here, or about to go.
///
/// Every number it reports is one the database keeps for its own sake - eviction
/// sorts partitions and rows by their last-read marks, expiry works off
/// `Expires` - so it costs nothing to maintain and answers the questions an
/// operator actually has about a row that vanished or refuses to.
#[http_route(
    method: "GET",
    route: "/api/Row/Statistics",
    controller: "Row",
    description: "What the table knows about one row and the partition holding it: the last-read marks eviction sorts by, both expiry moments and both sizes",
    summary: "Returns the statistics of one row",
    input_data: "GetRowStatisticsInputContract",
    result:[
        {status_code: 200, description: "Statistics"},
        {status_code: 404, description: "Namespace, table, partition or row is not found"},
    ]
)]
pub struct GetRowStatisticsAction {
    app: Arc<AppContext>,
}

impl GetRowStatisticsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetRowStatisticsAction,
    input_data: GetRowStatisticsInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let db_table = crate::db_operations::read::get_table(&db_namespace, &input_data.table_name)?;

    let statistics = crate::db_operations::read::get_row_statistics(
        &db_table,
        &input_data.partition_key,
        &input_data.row_key,
    )?;

    let json = JsonObjectWriter::new()
        .write(
            "partitionLastReadTime",
            statistics.partition_last_read_access.to_rfc3339(),
        )
        // Absent, never zero: "never expires" and "expires at the epoch" are
        // different things, and this server has spent a whole contract decision
        // on keeping them apart.
        .write_if_some(
            "partitionExpires",
            statistics
                .partition_expires
                .map(|moment| moment.to_rfc3339()),
        )
        .write("partitionRowsCount", statistics.partition_rows_count)
        .write("partitionDataSize", statistics.partition_content_size)
        // `TimeStamp`, not "write time": it is a named field of the entity
        // contract here, and calling it a write moment would claim server
        // bookkeeping which does not exist per row.
        .write("rowTimeStamp", statistics.row_time_stamp.to_rfc3339())
        // Equal to `rowTimeStamp` means the row has never been read - that is
        // what a row is seeded with, not evidence of a read.
        .write(
            "rowLastReadTime",
            statistics.row_last_read_access.to_rfc3339(),
        )
        .write_if_some(
            "rowExpires",
            statistics.row_expires.map(|moment| moment.to_rfc3339()),
        )
        .write("rowDataSize", statistics.row_stored_size)
        .build();

    as_json(json).into_ok_result(false)
}
