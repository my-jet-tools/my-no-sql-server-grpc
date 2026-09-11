//! The MCP surface: the same set of tools the JSON version exposes, meant for
//! the same job - a person asking about the data in words, and an agent looking.
//!
//! It sits on the HTTP port, behind the same ApiKey as everything else there,
//! and it registers exactly the calls listed in [`build_middleware`]. That list
//! is the whole of what an MCP client can do: a route which is not a tool here
//! is a route the model has no way to reach, which is what puts the write
//! window - `POST /api/Mcp/Writes` - out of its own hands.

mod backups;
pub use backups::*;
mod context;
pub use context::*;

mod get_backup_partitions_tool_call;
pub use get_backup_partitions_tool_call::*;
mod get_backup_rows_tool_call;
pub use get_backup_rows_tool_call::*;
mod get_backup_tables_tool_call;
pub use get_backup_tables_tool_call::*;
mod get_list_of_backups_tool_call;
pub use get_list_of_backups_tool_call::*;
mod get_list_of_tables_tool_call;
pub use get_list_of_tables_tool_call::*;
mod get_namespaces_tool_call;
pub use get_namespaces_tool_call::*;
mod get_rows_tool_call;
pub use get_rows_tool_call::*;

mod bulk_delete_rows_tool_call;
pub use bulk_delete_rows_tool_call::*;
mod bulk_insert_or_replace_rows_tool_call;
pub use bulk_insert_or_replace_rows_tool_call::*;
mod clean_table_tool_call;
pub use clean_table_tool_call::*;
mod delete_partitions_tool_call;
pub use delete_partitions_tool_call::*;
mod delete_row_tool_call;
pub use delete_row_tool_call::*;
mod insert_or_replace_row_tool_call;
pub use insert_or_replace_row_tool_call::*;
mod move_table_to_namespace_tool_call;
pub use move_table_to_namespace_tool_call::*;
mod restore_backup_tool_call;
pub use restore_backup_tool_call::*;

mod entity_schema_policy_prompt;
pub use entity_schema_policy_prompt::*;
mod mcp_writes_enable_policy_prompt;
pub use mcp_writes_enable_policy_prompt::*;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use mcp_server_middleware::McpMiddleware;

use crate::app::AppContext;

/// Where an MCP client points itself.
pub const MCP_PATH: &str = "/mcp";

/// The tools and prompts this server offers, in one place.
///
/// The reads come first and the writes after them, which is also the order they
/// are listed to the client - and the order somebody working through a table
/// would take them in.
pub fn build_middleware(app: &Arc<AppContext>) -> McpMiddleware {
    let mut mcp = McpMiddleware::new(
        MCP_PATH,
        crate::app::APP_NAME,
        crate::app::APP_VERSION,
        "MyNoSqlServer with protobuf entities. Rows are stored as protobuf and shown as JSON \
         through the schema their own client registered - read the 'entity_schema_policy' prompt \
         before writing one. Write tools are shut until a person opens a 10-minute window; the \
         'mcp_writes_enable_policy' prompt says how.",
    );

    mcp.register_tool_call(Arc::new(GetNamespacesToolCallHandler::new(app.clone())));
    mcp.register_tool_call(Arc::new(GetListOfTablesToolCallHandler::new(app.clone())));
    mcp.register_tool_call(Arc::new(GetRowsToolCallHandler::new(app.clone())));

    mcp.register_tool_call(Arc::new(GetListOfBackupsToolCallHandler::new(app.clone())));
    mcp.register_tool_call(Arc::new(GetBackupTablesToolCallHandler::new(app.clone())));
    mcp.register_tool_call(Arc::new(GetBackupPartitionsToolCallHandler::new(
        app.clone(),
    )));
    mcp.register_tool_call(Arc::new(GetBackupRowsToolCallHandler::new(app.clone())));

    mcp.register_tool_call(Arc::new(InsertOrReplaceRowToolCallHandler::new(
        app.clone(),
    )));
    mcp.register_tool_call(Arc::new(BulkInsertOrReplaceRowsToolCallHandler::new(
        app.clone(),
    )));
    mcp.register_tool_call(Arc::new(DeleteRowToolCallHandler::new(app.clone())));
    mcp.register_tool_call(Arc::new(BulkDeleteRowsToolCallHandler::new(app.clone())));
    mcp.register_tool_call(Arc::new(DeletePartitionsToolCallHandler::new(app.clone())));
    mcp.register_tool_call(Arc::new(CleanTableToolCallHandler::new(app.clone())));
    mcp.register_tool_call(Arc::new(MoveTableToNamespaceToolCallHandler::new(
        app.clone(),
    )));
    mcp.register_tool_call(Arc::new(RestoreBackupToolCallHandler::new(app.clone())));

    mcp.register_prompt(Arc::new(EntitySchemaPolicyPromptHandler));
    mcp.register_prompt(Arc::new(McpWritesEnablePolicyPromptHandler));

    mcp
}
