use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use serde::*;

use crate::app::AppContext;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct GetListOfBackupsInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct BackupFileModel {
    #[property(description = "Snapshot file name. Use it to navigate into the backup")]
    pub file_name: String,
    #[property(description = "Size of the snapshot file in bytes")]
    pub size: u64,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct GetListOfBackupsResponse {
    #[property(description = "Amount of snapshot files")]
    pub count: usize,
    #[property(description = "The snapshots of this namespace, oldest first")]
    pub files: Vec<BackupFileModel>,
}

pub struct GetListOfBackupsToolCallHandler {
    app: Arc<AppContext>,
}

impl GetListOfBackupsToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for GetListOfBackupsToolCallHandler {
    const FUNC_NAME: &'static str = "get_list_of_backups";

    const DESCRIPTION: &'static str = "Returns the snapshot (backup) files of a namespace. A backup here is a zip of ONE \
         namespace, so the namespace is part of which backups you are looking at. Use the \
         returned file_name to inspect the tables, partitions and rows inside it.";
}

#[async_trait::async_trait]
impl McpToolCall<GetListOfBackupsInputData, GetListOfBackupsResponse>
    for GetListOfBackupsToolCallHandler
{
    async fn execute_tool_call(
        &self,
        model: GetListOfBackupsInputData,
    ) -> Result<GetListOfBackupsResponse, String> {
        super::check_if_initialized(&self.app)?;

        let name_space = super::backup_namespace_name(model.namespace.as_deref());

        let files = crate::db_operations::backup::get_all(&self.app, &name_space).await?;

        let files: Vec<BackupFileModel> = files
            .into_iter()
            .map(|file| BackupFileModel {
                file_name: file.name,
                size: file.size,
            })
            .collect();

        Ok(GetListOfBackupsResponse {
            count: files.len(),
            files,
        })
    }
}
