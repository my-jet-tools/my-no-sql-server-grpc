use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::get_request_namespace;

use super::models::TableAttributesInputContract;
use super::table_attributes::to_attributes;

/// What a table does with itself: its limits, and whether it is persisted at
/// all. Everything not named goes back to its default - this sets the
/// attributes, it does not patch them, so a caller always knows what the table
/// ends up with.
#[http_route(
    method: "PUT",
    route: "/api/Tables/Attributes",
    controller: "Tables",
    description: "Sets the attributes of a table. This replaces them rather than patching them: anything not named goes back to its default",
    summary: "Sets the attributes of a table",
    input_data: "TableAttributesInputContract",
    result:[
        {status_code: 200, description: "The attributes are set"},
        {status_code: 404, description: "Namespace or table is not found"},
    ]
)]
pub struct SetTableAttributesAction {
    app: Arc<AppContext>,
}

impl SetTableAttributesAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &SetTableAttributesAction,
    input_data: TableAttributesInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let db_table = crate::db_operations::read::get_table(&db_namespace, &input_data.table_name)?;

    let now = DateTimeAsMicroseconds::now();

    crate::db_operations::write::set_table_attributes(
        &action.app,
        &db_namespace,
        &db_table,
        to_attributes(&input_data, now),
        input_data.sync_period.get_sync_moment(now),
    );

    HttpOutput::Empty.into_ok_result(true)
}
