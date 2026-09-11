use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::*;

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct CleanTableInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
    #[property(description = "Name of the table to empty. Every row in every partition goes")]
    pub table_name: String,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct CleanTableResponse {
    #[property(description = "Outcome message")]
    pub status: String,
    #[property(description = "How many rows the table held when it was emptied")]
    pub rows_removed: usize,
}

pub struct CleanTableToolCallHandler {
    app: Arc<AppContext>,
}

impl CleanTableToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for CleanTableToolCallHandler {
    const FUNC_NAME: &'static str = "clean_table";

    const DESCRIPTION: &'static str = "\
Removes ALL rows of a table. The table itself stays, with its attributes and the \
schemas it has learned. This is destructive and there is no undo - say how many \
rows the table holds (get_list_of_tables) and get the user's word before calling \
it. Requires MCP writes to be open; if this fails as SHUT, ask the user to open \
them - do not retry in a loop. See prompt 'mcp_writes_enable_policy'.";
}

#[async_trait::async_trait]
impl McpToolCall<CleanTableInputData, CleanTableResponse> for CleanTableToolCallHandler {
    async fn execute_tool_call(
        &self,
        model: CleanTableInputData,
    ) -> Result<CleanTableResponse, String> {
        let db_namespace = super::get_namespace(&self.app, model.namespace.as_deref())?;

        super::ensure_writes_are_open(&self.app)?;

        let db_table = super::get_table(&db_namespace, &model.table_name)?;

        // Read before the clean, which is the only moment it is still true.
        let rows_removed = db_table.get_metrics().rows_amount;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::clean_table(
            &self.app,
            &db_namespace,
            &db_table,
            DataSyncPeriod::Sec5.get_sync_moment(now),
        );

        Ok(CleanTableResponse {
            status: format!("Table '{}' is empty", model.table_name),
            rows_removed,
        })
    }
}
