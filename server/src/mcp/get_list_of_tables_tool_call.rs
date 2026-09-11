use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use serde::*;

use crate::app::AppContext;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct GetListOfTablesInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct TableModel {
    #[property(description = "Table name")]
    pub name: String,
    #[property(description = "Amount of partitions")]
    pub partitions_count: usize,
    #[property(description = "Amount of rows")]
    pub rows_count: usize,
    #[property(
        description = "How many entity versions the table holds rows of. Normally 1. More than one means a deploy is in flight or two entities are aimed at this table - and then a write has to name which schema_id it goes through"
    )]
    pub schemas_count: usize,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct GetListOfTablesResponse {
    #[property(description = "Amount of tables")]
    pub count: usize,
    #[property(description = "The tables of the namespace")]
    pub tables: Vec<TableModel>,
}

pub struct GetListOfTablesToolCallHandler {
    app: Arc<AppContext>,
}

impl GetListOfTablesToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for GetListOfTablesToolCallHandler {
    const FUNC_NAME: &'static str = "get_list_of_tables";

    const DESCRIPTION: &'static str = "Returns the tables of a namespace with their partition, row and schema counts. \
         Nothing here reads a row, so it is cheap on a large table.";
}

#[async_trait::async_trait]
impl McpToolCall<GetListOfTablesInputData, GetListOfTablesResponse>
    for GetListOfTablesToolCallHandler
{
    async fn execute_tool_call(
        &self,
        model: GetListOfTablesInputData,
    ) -> Result<GetListOfTablesResponse, String> {
        let db_namespace = super::get_namespace(&self.app, model.namespace.as_deref())?;

        let mut tables: Vec<TableModel> = db_namespace
            .tables
            .get_tables()
            .iter()
            .map(|db_table| {
                // One read lock per table, taken once: the three numbers asked
                // one at a time are three locks and a picture in which the rows
                // belong to a different moment than the partitions holding them.
                let metrics = db_table.get_metrics();

                TableModel {
                    name: db_table.name.clone(),
                    partitions_count: metrics.partitions_amount,
                    rows_count: metrics.rows_amount,
                    schemas_count: db_table.get_attributes().schemas.len(),
                }
            })
            .collect();

        tables.sort_by(|left, right| left.name.cmp(&right.name));

        Ok(GetListOfTablesResponse {
            count: tables.len(),
            tables,
        })
    }
}
