use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::get_request_namespace;

use super::models::TableInputContract;

/// The table itself.
#[http_route(
    method: "DELETE",
    route: "/api/Tables",
    controller: "Tables",
    description: "Removes the table and everything in it",
    summary: "Removes the table",
    input_data: "TableInputContract",
    result:[
        {status_code: 200, description: "The table is gone"},
        {status_code: 404, description: "Namespace or table is not found"},
    ]
)]
pub struct DeleteTableAction {
    app: Arc<AppContext>,
}

impl DeleteTableAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &DeleteTableAction,
    input_data: TableInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    crate::db_operations::write::delete_table(
        &action.app,
        &db_namespace,
        &input_data.table_name,
        input_data
            .sync_period
            .get_sync_moment(DateTimeAsMicroseconds::now()),
    )?;

    HttpOutput::Empty.into_ok_result(true)
}
