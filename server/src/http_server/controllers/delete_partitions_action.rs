use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::get_request_namespace;

use super::models::DeletePartitionsInputContract;

/// A key the table never held changes nothing and is not an error: what the
/// caller asked for - that these partitions are gone - is true either way.
#[http_route(
    method: "DELETE",
    route: "/api/Partitions",
    controller: "Partitions",
    description: "Drops whole partitions. A key the table does not hold is not an error",
    summary: "Drops whole partitions",
    input_data: "DeletePartitionsInputContract",
    result:[
        {status_code: 200, description: "The partitions are gone"},
        {status_code: 404, description: "Namespace or table is not found"},
    ]
)]
pub struct DeletePartitionsAction {
    app: Arc<AppContext>,
}

impl DeletePartitionsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &DeletePartitionsAction,
    input_data: DeletePartitionsInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let db_table = crate::db_operations::read::get_table(&db_namespace, &input_data.table_name)?;

    crate::db_operations::write::delete_partitions(
        &action.app,
        &db_namespace,
        &db_table,
        &input_data.get_partition_keys(),
        input_data
            .sync_period
            .get_sync_moment(DateTimeAsMicroseconds::now()),
    );

    HttpOutput::Empty.into_ok_result(true)
}
