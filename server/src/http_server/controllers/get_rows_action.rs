use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_no_sql_grpc_core::db::GetRowsFilter;

use crate::app::AppContext;
use crate::http_server::{get_request_namespace, rows_as_json};

use super::models::GetRowsInputContract;

#[http_route(
    method: "GET",
    route: "/api/Row",
    controller: "Row",
    description: "Rows rendered as JSON through the schema they were written with",
    summary: "Returns rows rendered as JSON. Always an array, even for a single row",
    input_data: "GetRowsInputContract",
    result:[
        {status_code: 200, description: "Rows as a JSON array"},
        {status_code: 404, description: "Namespace or table not found"},
    ]
)]
pub struct GetRowsAction {
    app: Arc<AppContext>,
}

impl GetRowsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetRowsAction,
    input_data: GetRowsInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let db_table = crate::db_operations::read::get_table(&db_namespace, &input_data.table_name)?;

    let db_rows = crate::db_operations::read::get_rows(
        &db_table,
        &GetRowsFilter {
            partition_key: input_data.partition_key.as_deref(),
            row_key: input_data.row_key.as_deref(),
            skip: input_data.skip,
            limit: input_data.limit,
        },
    );

    rows_as_json(&action.app, &db_table, &db_rows).into_ok_result(true)
}
