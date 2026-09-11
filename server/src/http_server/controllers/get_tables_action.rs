use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonArrayWriter;

use crate::app::AppContext;
use crate::http_server::{as_json, get_request_namespace};

use super::models::GetTablesInputContract;

#[http_route(
    method: "GET",
    route: "/api/Tables/List",
    controller: "Tables",
    description: "Tables of the namespace with their attributes",
    summary: "Returns tables of the namespace with their attributes",
    input_data: "GetTablesInputContract",
    result:[
        {status_code: 200, description: "Tables"},
    ]
)]
pub struct GetTablesAction {
    app: Arc<AppContext>,
}

impl GetTablesAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetTablesAction,
    _input_data: GetTablesInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let mut writer = JsonArrayWriter::new();

    for db_table in db_namespace.tables.get_tables().iter() {
        let attributes = db_table.get_attributes();
        // One acquisition of the read lock for the three numbers: asked one at a
        // time they are three locks and a view in which the rows can belong to a
        // different moment than the partitions holding them.
        let metrics = db_table.get_metrics();

        writer = writer.write_json_object(|table| {
            table
                .write("name", db_table.name.as_str())
                .write("persist", attributes.persist)
                .write("partitionsCount", metrics.partitions_amount)
                .write("rowsCount", metrics.rows_amount)
                .write("dataSize", metrics.content_size)
                .write_if_some("maxPartitionsAmount", attributes.max_partitions_amount)
                .write_if_some(
                    "maxRowsPerPartitionAmount",
                    attributes.max_rows_per_partition_amount,
                )
                .write("created", attributes.created.to_rfc3339())
        });
    }

    as_json(writer.build()).into_ok_result(true)
}
