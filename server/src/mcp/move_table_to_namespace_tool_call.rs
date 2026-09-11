use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::*;

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct MoveTableToNamespaceInputData {
    #[property(description = "Name of the table to move")]
    pub table_name: String,
    #[property(
        description = "Namespace the table is in right now. Empty means the default namespace"
    )]
    pub from_namespace: Option<String>,
    #[property(
        description = "Namespace to move it into. Empty means the default namespace. Created if it does not exist yet"
    )]
    pub to_namespace: Option<String>,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct MoveTableToNamespaceResponse {
    #[property(description = "Outcome message")]
    pub status: String,
    #[property(description = "Namespace the table was taken from")]
    pub from_namespace: String,
    #[property(description = "Namespace the table now lives in")]
    pub to_namespace: String,
}

pub struct MoveTableToNamespaceToolCallHandler {
    app: Arc<AppContext>,
}

impl MoveTableToNamespaceToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for MoveTableToNamespaceToolCallHandler {
    const FUNC_NAME: &'static str = "move_table_to_namespace";

    const DESCRIPTION: &'static str = "\
Moves a whole table, rows and attributes and schemas, from one namespace into \
another. It stops existing in the source: readers subscribed to it there are told \
it is gone, and readers of the destination get it initialized. Refused when the \
destination already has a table of that name - nothing is overwritten. Requires \
MCP writes to be open; if this fails as SHUT, ask the user to open them - do not \
retry in a loop. See prompt 'mcp_writes_enable_policy'.";
}

#[async_trait::async_trait]
impl McpToolCall<MoveTableToNamespaceInputData, MoveTableToNamespaceResponse>
    for MoveTableToNamespaceToolCallHandler
{
    async fn execute_tool_call(
        &self,
        model: MoveTableToNamespaceInputData,
    ) -> Result<MoveTableToNamespaceResponse, String> {
        super::ensure_writes_are_open(&self.app)?;

        // The source has to exist - there is nothing to move out of a namespace
        // nobody ever wrote to. The destination may well be new: moving a table
        // into a fresh namespace is the point of the call.
        let from = super::get_namespace(&self.app, model.from_namespace.as_deref())?;
        let to = super::get_or_create_namespace(&self.app, model.to_namespace.as_deref()).await?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::move_table_to_namespace(
            &self.app,
            &from,
            &model.table_name,
            &to,
            DataSyncPeriod::Sec5.get_sync_moment(now),
        )
        .await
        .map_err(|err| err.to_string())?;

        Ok(MoveTableToNamespaceResponse {
            status: format!(
                "Table '{}' moved from '{}' to '{}'",
                model.table_name, from.name, to.name
            ),
            from_namespace: from.name.clone(),
            to_namespace: to.name.clone(),
        })
    }
}
