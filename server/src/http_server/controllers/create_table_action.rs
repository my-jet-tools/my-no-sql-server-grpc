use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::get_request_namespace_name;

use super::models::TableAttributesInputContract;
use super::table_attributes::to_attributes;

/// The one write on this surface which **creates** its namespace: bringing
/// things into being is what it is for. Every other one resolves without
/// creating, so a typo in `ns` does not leave a folder on disk.
#[http_route(
    method: "POST",
    route: "/api/Tables/Create",
    controller: "Tables",
    description: "Creates a table. Fails with 409 when it is already there - use CreateIfNotExists to be sure it exists without caring",
    summary: "Creates a table",
    input_data: "TableAttributesInputContract",
    result:[
        {status_code: 200, description: "The table is created"},
        {status_code: 409, description: "The table is already there"},
    ]
)]
pub struct CreateTableAction {
    app: Arc<AppContext>,
}

impl CreateTableAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &CreateTableAction,
    input_data: TableAttributesInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = action
        .app
        .namespaces
        .get_or_create(get_request_namespace_name(ctx), &action.app.settings)
        .await?;

    let now = DateTimeAsMicroseconds::now();

    crate::db_operations::write::create_table(
        &db_namespace,
        &input_data.table_name,
        to_attributes(&input_data, now),
        input_data.sync_period.get_sync_moment(now),
    )?;

    HttpOutput::Empty.into_ok_result(true)
}
