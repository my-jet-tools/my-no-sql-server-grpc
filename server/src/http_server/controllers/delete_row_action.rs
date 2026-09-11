use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::get_request_namespace;

use super::models::DeleteRowInputContract;

/// One of the writes that carry no entity, and therefore live here rather than
/// on gRPC: it names keys and nothing else, so there is no protobuf to build and
/// no schema to build it with.
#[http_route(
    method: "DELETE",
    route: "/api/Row",
    controller: "Row",
    description: "Deletes one row by its keys",
    summary: "Deletes one row",
    input_data: "DeleteRowInputContract",
    result:[
        {status_code: 200, description: "The row is deleted, or there was no such row"},
        {status_code: 404, description: "Namespace or table is not found"},
    ]
)]
pub struct DeleteRowAction {
    app: Arc<AppContext>,
}

impl DeleteRowAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &DeleteRowAction,
    input_data: DeleteRowInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let db_table = crate::db_operations::read::get_table(&db_namespace, &input_data.table_name)?;

    // A key which is not there answers OK, exactly as it does over gRPC and
    // inside a BulkDelete: what the caller asked for is that the row is gone.
    crate::db_operations::write::delete_row(
        &action.app,
        &db_namespace,
        &db_table,
        &input_data.partition_key,
        &input_data.row_key,
        input_data
            .sync_period
            .get_sync_moment(DateTimeAsMicroseconds::now()),
    );

    HttpOutput::Empty.into_ok_result(true)
}
