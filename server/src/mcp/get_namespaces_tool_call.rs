use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use serde::*;

use crate::app::AppContext;

/// The JSON version has no tool for this, and it does not need one: it serves a
/// UI where the namespaces are on the screen. This server serves none, so every
/// other tool here takes a `namespace` the caller would otherwise have to guess.
#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct GetNamespacesInputData {}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct NamespaceModel {
    #[property(
        description = "Namespace name. 'default' is where callers which name no namespace land"
    )]
    pub name: String,
    #[property(description = "How many tables it holds")]
    pub tables_count: usize,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct GetNamespacesResponse {
    #[property(description = "Amount of namespaces")]
    pub count: usize,
    #[property(description = "Every namespace this server holds")]
    pub namespaces: Vec<NamespaceModel>,
}

pub struct GetNamespacesToolCallHandler {
    app: Arc<AppContext>,
}

impl GetNamespacesToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for GetNamespacesToolCallHandler {
    const FUNC_NAME: &'static str = "get_namespaces";

    const DESCRIPTION: &'static str = "Returns every namespace on this server. A namespace holds its own tables and its own \
         folder on disk; 'default' is where callers which name no namespace land. Call this \
         first when a request mentions a namespace you have not seen.";
}

#[async_trait::async_trait]
impl McpToolCall<GetNamespacesInputData, GetNamespacesResponse> for GetNamespacesToolCallHandler {
    async fn execute_tool_call(
        &self,
        _model: GetNamespacesInputData,
    ) -> Result<GetNamespacesResponse, String> {
        super::check_if_initialized(&self.app)?;

        let mut namespaces: Vec<NamespaceModel> = self
            .app
            .namespaces
            .get_all()
            .into_iter()
            .map(|db_namespace| NamespaceModel {
                name: db_namespace.name.clone(),
                tables_count: db_namespace.tables.get_tables().len(),
            })
            .collect();

        // The map behind them has no order of its own, and a list which comes
        // back shuffled between two calls reads as a server changing under you.
        namespaces.sort_by(|left, right| left.name.cmp(&right.name));

        Ok(GetNamespacesResponse {
            count: namespaces.len(),
            namespaces,
        })
    }
}
