use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::get_request_namespace;

use super::models::TableInputContract;

/// Emptying the table, and the table stays.
#[http_route(
    method: "POST",
    route: "/api/Tables/Clean",
    controller: "Tables",
    description: "Empties the table and keeps the table itself",
    summary: "Empties the table",
    input_data: "TableInputContract",
    result:[
        {status_code: 200, description: "The table is empty"},
        {status_code: 404, description: "Namespace or table is not found"},
    ]
)]
pub struct CleanTableAction {
    app: Arc<AppContext>,
}

impl CleanTableAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &CleanTableAction,
    input_data: TableInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let db_table = crate::db_operations::read::get_table(&db_namespace, &input_data.table_name)?;

    crate::db_operations::write::clean_table(
        &action.app,
        &db_namespace,
        &db_table,
        input_data
            .sync_period
            .get_sync_moment(DateTimeAsMicroseconds::now()),
    );

    HttpOutput::Empty.into_ok_result(true)
}
