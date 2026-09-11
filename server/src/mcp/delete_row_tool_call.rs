use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::*;

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct DeleteRowInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
    #[property(description = "Name of the table")]
    pub table_name: String,
    #[property(description = "Partition key")]
    pub partition_key: String,
    #[property(description = "Row key")]
    pub row_key: String,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct DeleteRowResponse {
    #[property(description = "Outcome message")]
    pub status: String,
    #[property(
        description = "Whether there was a row to delete. False is not a failure - the key simply was not there"
    )]
    pub deleted: bool,
}

pub struct DeleteRowToolCallHandler {
    app: Arc<AppContext>,
}

impl DeleteRowToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for DeleteRowToolCallHandler {
    const FUNC_NAME: &'static str = "delete_row";

    const DESCRIPTION: &'static str = "\
Deletes one row by partition_key + row_key. A key which is not there is not an \
error - the answer says whether there was a row. To delete more than one, use \
bulk_delete_rows: it takes as many partitions as you like in one entry into the \
table. Requires MCP writes to be open; if this fails as SHUT, ask the user to \
open them - do not retry in a loop. See prompt 'mcp_writes_enable_policy'.";
}

#[async_trait::async_trait]
impl McpToolCall<DeleteRowInputData, DeleteRowResponse> for DeleteRowToolCallHandler {
    async fn execute_tool_call(
        &self,
        model: DeleteRowInputData,
    ) -> Result<DeleteRowResponse, String> {
        let db_namespace = super::get_namespace(&self.app, model.namespace.as_deref())?;

        super::ensure_writes_are_open(&self.app)?;

        let db_table = super::get_table(&db_namespace, &model.table_name)?;

        let now = DateTimeAsMicroseconds::now();

        let deleted = crate::db_operations::write::delete_row(
            &self.app,
            &db_namespace,
            &db_table,
            &model.partition_key,
            &model.row_key,
            DataSyncPeriod::Sec5.get_sync_moment(now),
        );

        Ok(DeleteRowResponse {
            status: if deleted {
                "deleted".to_string()
            } else {
                "there was no such row".to_string()
            },
            deleted,
        })
    }
}
