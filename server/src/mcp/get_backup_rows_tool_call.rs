use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use serde::*;

use crate::app::AppContext;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct GetBackupRowsInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
    #[property(description = "Snapshot file name (as returned by get_list_of_backups)")]
    pub file_name: String,
    #[property(description = "Table name inside the snapshot (as returned by get_backup_tables)")]
    pub table_name: String,
    #[property(
        description = "Partition key inside the table (as returned by get_backup_partitions)"
    )]
    pub partition_key: String,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct GetBackupRowsResponse {
    #[property(description = "Amount of rows in the partition")]
    pub count: usize,
    #[property(
        description = "The rows. Each item is a JSON object encoded as a string, rendered through the schema the snapshot carries for the table"
    )]
    pub rows: Vec<String>,
}

pub struct GetBackupRowsToolCallHandler {
    app: Arc<AppContext>,
}

impl GetBackupRowsToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for GetBackupRowsToolCallHandler {
    const FUNC_NAME: &'static str = "get_backup_rows";

    const DESCRIPTION: &'static str = "Returns the rows of one partition stored inside a snapshot (backup) file, without \
         restoring anything. Use get_backup_partitions first to find the partition_key. The rows \
         are rendered through the schemas the snapshot carries with the table, so a backup taken \
         of an entity version nobody runs any more is still readable.";
}

#[async_trait::async_trait]
impl McpToolCall<GetBackupRowsInputData, GetBackupRowsResponse> for GetBackupRowsToolCallHandler {
    async fn execute_tool_call(
        &self,
        model: GetBackupRowsInputData,
    ) -> Result<GetBackupRowsResponse, String> {
        let content =
            super::read_backup(&self.app, model.namespace.as_deref(), &model.file_name).await?;

        let table = super::find_backup_table(&content, &model.file_name, &model.table_name)?;

        let Some(partition) = table
            .partitions
            .iter()
            .find(|itm| itm.partition_key == model.partition_key)
        else {
            return Err(format!(
                "The backup '{}' has no partition '{}' of table '{}'.",
                model.file_name, model.partition_key, model.table_name
            ));
        };

        // The schemas travel with the table's attributes, which is what makes a
        // partition inside an archive showable at all: the shape a row was
        // written under may be one no live table holds any more.
        let schemas = table.attributes.as_ref().map(|itm| itm.schemas.clone());

        let rows = crate::persist::partition_blob::deserialize(&partition.blob)?
            .into_iter()
            .map(|persisted| {
                let schema = schemas
                    .as_ref()
                    .and_then(|schemas| schemas.get(&persisted.schema_id))
                    .and_then(|schema| self.app.json_schemas.get_or_build(schema));

                crate::json_view::write_row_as_json(
                    my_json::json_writer::JsonObjectWriter::new(),
                    &persisted.row,
                    schema.as_deref(),
                )
                .build()
            })
            .collect::<Vec<String>>();

        Ok(GetBackupRowsResponse {
            count: rows.len(),
            rows,
        })
    }
}
