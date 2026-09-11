use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::get_request_namespace_name;

use super::models::TableAttributesInputContract;
use super::table_attributes::to_attributes;

/// Two routes rather than one with a flag, because they are two operations: one
/// says "this must not exist yet" and the other says "I do not care". A flag
/// which changes what a failure means is a flag that gets defaulted wrong.
#[http_route(
    method: "POST",
    route: "/api/Tables/CreateIfNotExists",
    controller: "Tables",
    description: "Creates a table unless it is already there. The attributes of a table which is already there are left alone",
    summary: "Makes sure a table exists",
    input_data: "TableAttributesInputContract",
    result:[
        {status_code: 200, description: "The table exists"},
    ]
)]
pub struct CreateTableIfNotExistsAction {
    app: Arc<AppContext>,
}

impl CreateTableIfNotExistsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &CreateTableIfNotExistsAction,
    input_data: TableAttributesInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = action
        .app
        .namespaces
        .get_or_create(get_request_namespace_name(ctx), &action.app.settings)
        .await?;

    let now = DateTimeAsMicroseconds::now();

    crate::db_operations::write::create_table_if_not_exists(
        &action.app,
        &db_namespace,
        &input_data.table_name,
        to_attributes(&input_data, now),
        input_data.sync_period.get_sync_moment(now),
    );

    HttpOutput::Empty.into_ok_result(true)
}
