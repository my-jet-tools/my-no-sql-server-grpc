use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use my_json::json_reader::JsonFirstLineIterator;
use my_no_sql_grpc_core::db::PartitionRowKeys;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::*;

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct BulkDeleteRowsInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
    #[property(description = "Name of the table")]
    pub table_name: String,
    #[property(
        description = "What to delete, as a JSON object of partition key -> row keys: {\"acc-1\": [\"rk-1\", \"rk-2\"], \"acc-2\": [\"rk-3\"]}. As many partitions as you like"
    )]
    pub rows_json: String,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct BulkDeleteRowsResponse {
    #[property(description = "Outcome message")]
    pub status: String,
    #[property(
        description = "How many rows were actually there. Keys which were not are not an error, which is why this can be lower than what you named"
    )]
    pub rows_deleted: usize,
}

pub struct BulkDeleteRowsToolCallHandler {
    app: Arc<AppContext>,
}

impl BulkDeleteRowsToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for BulkDeleteRowsToolCallHandler {
    const FUNC_NAME: &'static str = "bulk_delete_rows";

    const DESCRIPTION: &'static str = "\
Deletes rows by key across as many partitions as you name, in ONE entry into the \
table. Prefer it over calling delete_row in a loop - subscribers see one change \
instead of N, and the count comes back as how many rows were actually there. \
Before a large or a wide delete, show the user the list you are about to pass \
and let them confirm it: this call is not undoable and the rows do not come back \
from a reader's cache. Requires MCP writes to be open; if this fails as SHUT, ask \
the user to open them - do not retry in a loop. See prompt \
'mcp_writes_enable_policy'.";
}

#[async_trait::async_trait]
impl McpToolCall<BulkDeleteRowsInputData, BulkDeleteRowsResponse>
    for BulkDeleteRowsToolCallHandler
{
    async fn execute_tool_call(
        &self,
        model: BulkDeleteRowsInputData,
    ) -> Result<BulkDeleteRowsResponse, String> {
        let db_namespace = super::get_namespace(&self.app, model.namespace.as_deref())?;

        super::ensure_writes_are_open(&self.app)?;

        let db_table = super::get_table(&db_namespace, &model.table_name)?;

        // Read whole before anything is touched: a body the server can not read
        // leaves the table as it was.
        let partitions = parse(&model.rows_json)?;

        if partitions.is_empty() {
            return Err("`rows_json` names no partitions - nothing to delete.".to_string());
        }

        let now = DateTimeAsMicroseconds::now();

        let rows_deleted = crate::db_operations::write::bulk_delete(
            &self.app,
            &db_namespace,
            &db_table,
            partitions,
            DataSyncPeriod::Sec5.get_sync_moment(now),
        );

        Ok(BulkDeleteRowsResponse {
            status: "ok".to_string(),
            rows_deleted,
        })
    }
}

/// The same shape `POST /api/Rows/BulkDelete` takes, and read the same way: an
/// object rather than a list of pairs, because the keys of a map do not repeat
/// and the shape should say so.
fn parse(body: &str) -> Result<Vec<PartitionRowKeys>, String> {
    let mut result = Vec::new();

    let reader = JsonFirstLineIterator::new(body.as_bytes());

    while let Some(next) = reader.get_next() {
        let (partition_key, row_keys) = next.map_err(bad_body)?;

        let partition_key = partition_key.as_str().map_err(bad_body)?.to_string();

        let items = row_keys
            .unwrap_as_array()
            .map_err(|_| format!("the row keys of partition '{partition_key}' are not an array"))?;

        let mut keys = Vec::new();

        while let Some(item) = items.get_next() {
            let item = item.map_err(bad_body)?;

            // `as_str` is happy to hand back a number as its text, so the type
            // is checked rather than the conversion: a row key which arrived as
            // `1` is a caller bug, not a key called "1".
            if !item.is_string() {
                return Err(format!(
                    "a row key of partition '{partition_key}' is not a string"
                ));
            }

            let Some(row_key) = item.as_str() else {
                return Err(format!(
                    "a row key of partition '{partition_key}' can not be read"
                ));
            };

            keys.push(row_key.to_string());
        }

        result.push(PartitionRowKeys {
            partition_key,
            row_keys: keys,
        });
    }

    Ok(result)
}

fn bad_body(err: impl std::fmt::Debug) -> String {
    format!("`rows_json` is not a JSON object of partition key -> row keys: {err:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_becomes_the_keys_to_delete() {
        let parsed = parse(r#"{"acc-1":["rk-1","rk-2"],"acc-2":["rk-3"]}"#).unwrap();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].partition_key, "acc-1");
        assert_eq!(parsed[0].row_keys, vec!["rk-1", "rk-2"]);
        assert_eq!(parsed[1].partition_key, "acc-2");
        assert_eq!(parsed[1].row_keys, vec!["rk-3"]);
    }

    #[test]
    fn a_body_which_is_not_that_shape_is_refused() {
        assert!(parse("[]").is_err());
        assert!(parse(r#"{"acc-1":"rk-1"}"#).is_err());
        assert!(parse(r#"{"acc-1":[1,2]}"#).is_err());
        assert!(parse("not json at all").is_err());
    }
}
