use std::sync::Arc;

use my_http_server::{HttpOutput, WebContentType};
use my_json::json_writer::JsonArrayWriter;
use my_no_sql_grpc_core::db::{DbRow, DbTable};

use crate::app::AppContext;

/// Renders rows into a JSON array, resolving each row's own schema.
///
/// The lookup is per row on purpose: after a client changes its entity, old and
/// new rows sit side by side in the same partition, and each has to be rendered
/// with the schema it was written with.
pub fn rows_as_json(
    app: &Arc<AppContext>,
    db_table: &DbTable,
    db_rows: &[Arc<DbRow>],
) -> HttpOutput {
    let mut writer = JsonArrayWriter::new();

    for db_row in db_rows {
        let schema = db_table
            .get_schema(db_row.get_schema_id())
            .and_then(|schema| app.json_schemas.get_or_build(&schema));

        let row_bytes = db_row.to_vec();

        writer = writer.write_json_object(|row| {
            crate::json_view::write_row_as_json(row, &row_bytes, schema.as_deref())
        });
    }

    as_json(writer.build())
}

pub fn as_json(json: String) -> HttpOutput {
    HttpOutput::Content {
        headers: WebContentType::Json.into(),
        content: json.into_bytes(),
        status_code: 200,
    }
}
