use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use my_json::json_reader::JsonArrayIterator;
use my_no_sql_grpc_core::db::{BulkWriteMode, DbRow};
use my_no_sql_grpc_core::db_entity::ParsedEntity;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::*;

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct BulkInsertOrReplaceRowsInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
    #[property(description = "Name of the table")]
    pub table_name: String,
    #[property(
        description = "A JSON array of row objects. Each one is shaped like the entity_json of insert_or_replace_row. Rows may belong to different partitions"
    )]
    pub entities_json: String,
    #[property(
        description = "Which entity version to write through. Only needed when the table holds rows of more than one"
    )]
    pub schema_id: Option<u64>,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct BulkInsertOrReplaceRowsResponse {
    #[property(description = "Outcome message")]
    pub status: String,
    #[property(description = "How many rows were written")]
    pub rows_written: usize,
    #[property(description = "How many distinct partitions they landed in")]
    pub partitions_affected: usize,
    #[property(description = "The entity version the rows were written under")]
    pub schema_id: u64,
}

pub struct BulkInsertOrReplaceRowsToolCallHandler {
    app: Arc<AppContext>,
}

impl BulkInsertOrReplaceRowsToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for BulkInsertOrReplaceRowsToolCallHandler {
    const FUNC_NAME: &'static str = "bulk_insert_or_replace_rows";

    const DESCRIPTION: &'static str = "\
Inserts or replaces many rows in ONE entry into the table - subscribers see the \
whole batch at once, which they would not if you called insert_or_replace_row in \
a loop. Pass `entities_json` as a JSON array of row objects; rows may span \
partitions. Every row is turned into protobuf through a schema the table already \
holds, and one row which does not fit it refuses the whole call before anything \
is written. See prompt 'entity_schema_policy'. Requires MCP writes to be open; if \
this fails as SHUT, ask the user to open them - do not retry in a loop. See \
prompt 'mcp_writes_enable_policy'.";
}

#[async_trait::async_trait]
impl McpToolCall<BulkInsertOrReplaceRowsInputData, BulkInsertOrReplaceRowsResponse>
    for BulkInsertOrReplaceRowsToolCallHandler
{
    async fn execute_tool_call(
        &self,
        model: BulkInsertOrReplaceRowsInputData,
    ) -> Result<BulkInsertOrReplaceRowsResponse, String> {
        let db_namespace = super::get_namespace(&self.app, model.namespace.as_deref())?;

        super::ensure_writes_are_open(&self.app)?;

        let db_table = super::get_table(&db_namespace, &model.table_name)?;

        let schema = super::pick_schema(&db_table, model.schema_id)?;
        let index = super::build_index(&self.app, &schema)?;

        let now = DateTimeAsMicroseconds::now();

        // Every row is built before any of them is applied. That is the same
        // rule the streaming write follows: a batch the server could not read to
        // the end leaves the table exactly as it was.
        let mut db_rows = Vec::new();

        let items = JsonArrayIterator::new(model.entities_json.as_bytes())
            .map_err(|err| format!("`entities_json` is not a JSON array: {err:?}"))?;

        while let Some(item) = items.get_next() {
            let item =
                item.map_err(|err| format!("`entities_json` is not a JSON array: {err:?}"))?;

            if !item.is_object() {
                return Err("`entities_json` holds something which is not a row object".to_string());
            }

            let row = crate::json_view::write_row_from_json(item.as_slice(), &index)?;
            let parsed = ParsedEntity::parse(&row).map_err(|err| format!("{err:?}"))?;

            db_rows.push(Arc::new(DbRow::new(parsed, schema.id, now)));
        }

        if db_rows.is_empty() {
            return Err("`entities_json` holds no rows - nothing to write.".to_string());
        }

        let rows_written = db_rows.len();

        let mut partitions: Vec<&str> = db_rows.iter().map(|row| row.get_partition_key()).collect();
        partitions.sort_unstable();
        partitions.dedup();
        let partitions_affected = partitions.len();

        crate::db_operations::write::bulk_write(
            &self.app,
            &db_namespace,
            &db_table,
            BulkWriteMode::InsertOrReplace,
            db_rows,
            DataSyncPeriod::Sec5.get_sync_moment(now),
        );

        Ok(BulkInsertOrReplaceRowsResponse {
            status: "written".to_string(),
            rows_written,
            partitions_affected,
            schema_id: schema.id,
        })
    }
}
