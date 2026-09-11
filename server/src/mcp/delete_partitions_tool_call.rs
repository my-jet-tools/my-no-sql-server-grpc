use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::*;

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct DeletePartitionsInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
    #[property(description = "Name of the table")]
    pub table_name: String,
    #[property(description = "Partition keys to remove. Every row of each of them goes with it")]
    pub partition_keys: Vec<String>,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct DeletePartitionsResponse {
    #[property(description = "Outcome message")]
    pub status: String,
    #[property(description = "How many partitions were named")]
    pub partitions_submitted: usize,
}

pub struct DeletePartitionsToolCallHandler {
    app: Arc<AppContext>,
}

impl DeletePartitionsToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for DeletePartitionsToolCallHandler {
    const FUNC_NAME: &'static str = "delete_partitions";

    const DESCRIPTION: &'static str = "\
Deletes whole partitions of a table - every row they hold goes with them - in one \
entry into the table. A key the table does not have is not an error. Show the \
user the list before calling: this is not undoable. Requires MCP writes to be \
open; if this fails as SHUT, ask the user to open them - do not retry in a loop. \
See prompt 'mcp_writes_enable_policy'.";
}

#[async_trait::async_trait]
impl McpToolCall<DeletePartitionsInputData, DeletePartitionsResponse>
    for DeletePartitionsToolCallHandler
{
    async fn execute_tool_call(
        &self,
        model: DeletePartitionsInputData,
    ) -> Result<DeletePartitionsResponse, String> {
        let db_namespace = super::get_namespace(&self.app, model.namespace.as_deref())?;

        if model.partition_keys.is_empty() {
            return Err("`partition_keys` is empty - nothing to delete.".to_string());
        }

        super::ensure_writes_are_open(&self.app)?;

        let db_table = super::get_table(&db_namespace, &model.table_name)?;

        let partitions_submitted = model.partition_keys.len();
        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::delete_partitions(
            &self.app,
            &db_namespace,
            &db_table,
            &model.partition_keys,
            DataSyncPeriod::Sec5.get_sync_moment(now),
        );

        Ok(DeletePartitionsResponse {
            status: "ok".to_string(),
            partitions_submitted,
        })
    }
}
