use std::sync::Arc;

use mcp_server_middleware::*;
use my_ai_agent::macros::ApplyJsonSchema;
use my_no_sql_grpc_core::db::GetRowsFilter;
use serde::*;

use crate::app::AppContext;

/// How many rows come back when the caller does not say.
///
/// The JSON version has no window here and hands back the table. That is a whole
/// table in a context window, and the answer to "what is in traders" is the
/// first page of it - `has_more` says whether to ask again.
const DEFAULT_LIMIT: usize = 100;

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct GetRowsInputData {
    #[property(description = "Optional namespace. Empty means the default namespace")]
    pub namespace: Option<String>,
    #[property(description = "Name of the table to query")]
    pub table_name: String,
    #[property(description = "Optional partition key filter")]
    pub partition_key: Option<String>,
    #[property(description = "Optional row key filter")]
    pub row_key: Option<String>,
    #[property(description = "Amount of rows to skip. Use it with limit to page through a table")]
    pub skip: Option<usize>,
    #[property(description = "Maximum amount of rows to return. 100 when not given")]
    pub limit: Option<usize>,
}

#[derive(ApplyJsonSchema, Debug, Serialize, Deserialize)]
pub struct GetRowsResponse {
    #[property(description = "Amount of rows in this answer")]
    pub count: usize,
    #[property(
        description = "True when the filter matched more rows than were returned. Raise skip by count and ask again"
    )]
    pub has_more: bool,
    #[property(
        description = "The rows. Each item is a JSON object encoded as a string, rendered through the schema the row was written under"
    )]
    pub rows: Vec<String>,
}

pub struct GetRowsToolCallHandler {
    app: Arc<AppContext>,
}

impl GetRowsToolCallHandler {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

impl ToolDefinition for GetRowsToolCallHandler {
    const FUNC_NAME: &'static str = "get_rows";

    const DESCRIPTION: &'static str = "Returns rows of a table as JSON. Filter by partition_key and/or row_key (both optional) \
         and page with skip/limit - 100 rows come back when no limit is given, and has_more says \
         whether there are further ones. Rows are stored as protobuf and rendered through the \
         schema they were written under; a row whose schema this server does not have is shown \
         with field numbers for names. See prompt 'entity_schema_policy'.";
}

#[async_trait::async_trait]
impl McpToolCall<GetRowsInputData, GetRowsResponse> for GetRowsToolCallHandler {
    async fn execute_tool_call(&self, model: GetRowsInputData) -> Result<GetRowsResponse, String> {
        let db_namespace = super::get_namespace(&self.app, model.namespace.as_deref())?;
        let db_table = super::get_table(&db_namespace, &model.table_name)?;

        let limit = model.limit.unwrap_or(DEFAULT_LIMIT);

        // One row past the window, which is what answers "is there more" without
        // walking the table a second time to count it.
        let wanted = limit.saturating_add(1);

        // `DbTable::get_rows` rather than `db_operations::read::get_rows`, which
        // is the same read with the last-read marks moved. Looking at rows here
        // must not keep them alive: eviction goes by which rows nobody has read,
        // and an agent browsing a table would rescue exactly the cold rows
        // somebody went to look at. The same argument `Row/Statistics` stands on.
        let mut db_rows = db_table.get_rows(&GetRowsFilter {
            partition_key: model.partition_key.as_deref(),
            row_key: model.row_key.as_deref(),
            skip: model.skip,
            limit: Some(wanted),
        });

        let has_more = db_rows.len() > limit;
        db_rows.truncate(limit);

        // Per row, not per call: after a client changes its entity, rows of the
        // old and the new version sit in the same partition and each has to be
        // shown through the schema it was written with.
        let rows = db_rows
            .iter()
            .map(|db_row| {
                let schema = db_table
                    .get_schema(db_row.get_schema_id())
                    .and_then(|schema| self.app.json_schemas.get_or_build(&schema));

                crate::json_view::write_row_as_json(
                    my_json::json_writer::JsonObjectWriter::new(),
                    &db_row.to_vec(),
                    schema.as_deref(),
                )
                .build()
            })
            .collect::<Vec<String>>();

        Ok(GetRowsResponse {
            count: rows.len(),
            has_more,
            rows,
        })
    }
}
