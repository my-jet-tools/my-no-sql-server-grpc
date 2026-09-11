use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use my_no_sql_grpc_core::db::DbRow;
use my_no_sql_grpc_core::db_entity::ParsedEntity;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::*;

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct InsertOrReplaceRowInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
    #[property(description = "Name of the table")]
    pub table_name: String,
    #[property(
        description = "The whole row as a JSON object. Field names are the ones get_rows shows. 'PartitionKey' and 'RowKey' are required; 'Expires' is optional (RFC3339, or null for never); 'TimeStamp' is ignored - the server stamps its own"
    )]
    pub entity_json: String,
    #[property(
        description = "Which entity version to write through. Only needed when the table holds rows of more than one - get_list_of_tables says how many it has"
    )]
    pub schema_id: Option<u64>,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct InsertOrReplaceRowResponse {
    #[property(description = "Outcome message")]
    pub status: String,
    #[property(description = "The entity version the row was written under")]
    pub schema_id: u64,
}

pub struct InsertOrReplaceRowToolCallHandler {
    app: Arc<AppContext>,
}

impl InsertOrReplaceRowToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for InsertOrReplaceRowToolCallHandler {
    const FUNC_NAME: &'static str = "insert_or_replace_row";

    const DESCRIPTION: &'static str = "\
Inserts a row, or replaces the one with the same PartitionKey + RowKey. The row \
is given as JSON and turned into protobuf through a schema the table ALREADY \
holds - so the table has to have been written to at least once by the service \
which owns the entity, and a field name the schema does not have is refused \
rather than dropped. See prompt 'entity_schema_policy'. Requires MCP writes to \
be open; if this fails as SHUT, ask the user to open them - do not retry in a \
loop. See prompt 'mcp_writes_enable_policy'.";
}

#[async_trait::async_trait]
impl McpToolCall<InsertOrReplaceRowInputData, InsertOrReplaceRowResponse>
    for InsertOrReplaceRowToolCallHandler
{
    async fn execute_tool_call(
        &self,
        model: InsertOrReplaceRowInputData,
    ) -> Result<InsertOrReplaceRowResponse, String> {
        let db_namespace = super::get_namespace(&self.app, model.namespace.as_deref())?;

        super::ensure_writes_are_open(&self.app)?;

        let db_table = super::get_table(&db_namespace, &model.table_name)?;

        let schema = super::pick_schema(&db_table, model.schema_id)?;
        let index = super::build_index(&self.app, &schema)?;

        let row = crate::json_view::write_row_from_json(model.entity_json.as_bytes(), &index)?;

        // Parsed the way every other write path parses one: the keys have to be
        // there, the moments have to be moments, and the field numbers have to
        // be ones a protobuf decoder would hand back.
        let parsed = ParsedEntity::parse(&row).map_err(|err| format!("{err:?}"))?;

        let now = DateTimeAsMicroseconds::now();

        // The server's clock, like on every other write. What the caller called
        // `TimeStamp` was thrown away when the row was built.
        let db_row = Arc::new(DbRow::new(parsed, schema.id, now));

        crate::db_operations::write::insert_or_replace(
            &self.app,
            &db_namespace,
            &db_table,
            db_row,
            DataSyncPeriod::Sec5.get_sync_moment(now),
        );

        Ok(InsertOrReplaceRowResponse {
            status: "written".to_string(),
            schema_id: schema.id,
        })
    }
}
