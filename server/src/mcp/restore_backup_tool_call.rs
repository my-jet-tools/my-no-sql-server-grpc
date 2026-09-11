use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::*;

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;
use crate::db_operations::backup::RestoreOne;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct RestoreBackupInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
    #[property(description = "Snapshot file name (as returned by get_list_of_backups)")]
    pub file_name: String,
    #[property(
        description = "Restore one partition only. Both table_name and partition_key have to be given together; leave both out to restore the whole snapshot"
    )]
    pub table_name: Option<String>,
    #[property(
        description = "The partition of table_name to restore. Only meaningful together with it"
    )]
    pub partition_key: Option<String>,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct RestoreBackupResponse {
    #[property(description = "Outcome message")]
    pub status: String,
    #[property(description = "How many partitions were put back")]
    pub partitions_restored: usize,
}

pub struct RestoreBackupToolCallHandler {
    app: Arc<AppContext>,
}

impl RestoreBackupToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for RestoreBackupToolCallHandler {
    const FUNC_NAME: &'static str = "restore_backup";

    const DESCRIPTION: &'static str = "\
Puts a snapshot back into its namespace. What is restored REPLACES what is there: \
a restored partition holds the archive's rows and nothing else. Leave table_name \
and partition_key out to restore the whole snapshot, or give both to put back one \
partition. Look inside first with get_backup_tables / get_backup_partitions / \
get_backup_rows, and show the user what is about to be replaced. Requires MCP \
writes to be open; if this fails as SHUT, ask the user to open them - do not retry \
in a loop. See prompt 'mcp_writes_enable_policy'.";
}

#[async_trait::async_trait]
impl McpToolCall<RestoreBackupInputData, RestoreBackupResponse> for RestoreBackupToolCallHandler {
    async fn execute_tool_call(
        &self,
        model: RestoreBackupInputData,
    ) -> Result<RestoreBackupResponse, String> {
        super::check_if_initialized(&self.app)?;
        super::ensure_writes_are_open(&self.app)?;

        // Half a name is a request nobody can act on: naming a table without a
        // partition would read as "restore this table", which is not what the
        // operation does.
        let only = match (model.table_name.as_ref(), model.partition_key.as_ref()) {
            (None, None) => None,
            (Some(table_name), Some(partition_key)) => Some(RestoreOne {
                table_name: table_name.clone(),
                partition_key: partition_key.clone(),
            }),
            _ => {
                return Err(
                    "Naming one partition takes both `table_name` and `partition_key`. Leave both \
                     out to restore the whole snapshot."
                        .to_string(),
                );
            }
        };

        let name_space = super::backup_namespace_name(model.namespace.as_deref());
        let now = DateTimeAsMicroseconds::now();

        let partitions_restored = crate::db_operations::backup::restore(
            &self.app,
            &name_space,
            &model.file_name,
            only,
            DataSyncPeriod::Sec5.get_sync_moment(now),
        )
        .await
        .map_err(|err| err.to_string())?;

        Ok(RestoreBackupResponse {
            status: format!(
                "Restored from snapshot '{}' into namespace '{name_space}'",
                model.file_name
            ),
            partitions_restored,
        })
    }
}
