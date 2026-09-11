use std::sync::Arc;

use ahash::AHashMap;
use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonArrayWriter;
use my_no_sql_grpc_abstractions::schemas::EntitySchema;

use crate::app::AppContext;
use crate::db_operations::DbOperationError;
use crate::http_server::{as_json, get_request_namespace_name};
use crate::json_view::JsonSchemasCache;
use crate::persist::partition_blob::PersistedRow;

use super::models::GetBackupRowsInputContract;

/// The rows of one partition inside one snapshot, as JSON.
///
/// Rendered through the schemas **the archive carries**, not through the live
/// table's: that is what makes looking inside a backup possible at all. A
/// snapshot taken of an entity version nobody deploys any more would otherwise
/// come back as field numbers, or not at all if the table it was taken of has
/// since been dropped.
///
/// A bare array, like `/api/Row` - the count is what `/api/Backup/Partitions`
/// is for, and a shape that changed with the number of rows found would make the
/// page parse it twice.
#[http_route(
    method: "GET",
    route: "/api/Backup/Rows",
    controller: "Backup",
    description: "Rows of one partition inside a snapshot file, rendered as JSON through the schemas the snapshot carries. Always an array, even for a single row",
    summary: "Returns the rows of a partition inside a snapshot",
    input_data: "GetBackupRowsInputContract",
    result:[
        {status_code: 200, description: "The rows as a JSON array"},
        {status_code: 412, description: "Backups are not configured, the file is not there, it is not a backup of this server, or it has no such table or partition"},
    ]
)]
pub struct GetBackupRowsAction {
    app: Arc<AppContext>,
}

impl GetBackupRowsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetBackupRowsAction,
    input_data: GetBackupRowsInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let name_space = get_request_namespace_name(ctx);

    let content =
        crate::db_operations::backup::inspect(&action.app, name_space, &input_data.file_name)
            .await
            .map_err(DbOperationError::BackupFailed)?;

    // A name missing from an archive is not a name missing from the database, so
    // both of these refusals are `BackupFailed` rather than `TableNotFound` -
    // and the table one names what the snapshot does hold, which is what
    // somebody who mistyped a name needs to see.
    let table =
        crate::mcp::find_backup_table(&content, &input_data.file_name, &input_data.table_name)
            .map_err(DbOperationError::BackupFailed)?;

    let Some(partition) = table
        .partitions
        .iter()
        .find(|itm| itm.partition_key == input_data.partition_key)
    else {
        return Err(DbOperationError::BackupFailed(format!(
            "The backup '{}' has no partition '{}' of table '{}'",
            input_data.file_name, input_data.partition_key, input_data.table_name
        ))
        .into());
    };

    let rows = crate::persist::partition_blob::deserialize(&partition.blob)
        .map_err(DbOperationError::BackupFailed)?;

    // The schemas travel with the table's attributes inside the archive, which
    // is the whole reason a partition in one can be shown. A table the backup
    // carried no attributes for renders by field number rather than not at all.
    let schemas: Option<&AHashMap<u64, Arc<EntitySchema>>> = table
        .attributes
        .as_ref()
        .map(|attributes| attributes.schemas.as_ref());

    as_json(render(
        &rows,
        schemas,
        &action.app.json_schemas,
        input_data.skip,
        input_data.limit,
    ))
    .into_ok_result(true)
}

/// The answer itself, kept apart from the request so the shape can be exercised
/// without one - the schemas cache is passed in for the same reason.
fn render(
    rows: &[PersistedRow],
    schemas: Option<&AHashMap<u64, Arc<EntitySchema>>>,
    json_schemas: &JsonSchemasCache,
    skip: Option<usize>,
    limit: Option<usize>,
) -> String {
    let skip = skip.unwrap_or(0);
    let limit = limit.unwrap_or(usize::MAX);

    let mut writer = JsonArrayWriter::new();

    for row in rows.iter().skip(skip).take(limit) {
        // Resolved per row, not per partition: a client which changed its entity
        // leaves rows of both versions side by side in one partition, and each
        // has to be shown under the schema it was written with.
        let schema = schemas
            .and_then(|schemas| schemas.get(&row.schema_id))
            .and_then(|schema| json_schemas.get_or_build(schema));

        writer = writer.write_json_object(|itm| {
            crate::json_view::write_row_as_json(itm, &row.row, schema.as_deref())
        });
    }

    writer.build()
}

#[cfg(test)]
mod tests {
    use my_no_sql_grpc_abstractions::schemas::{DeclaredField, Scalar, SchemaBuilder};

    use super::*;

    const SCHEMA_ID: u64 = 7;

    /// One length-delimited string field, spelled onto the wire by hand: what
    /// this route renders is a stored row, and a stored row is bytes. Field
    /// numbers stay under 16 and values under 128 bytes, so tag and length are
    /// one byte each.
    fn string_field(dest: &mut Vec<u8>, field_no: u8, value: &str) {
        dest.push(field_no << 3 | 2);
        dest.push(value.len() as u8);
        dest.extend_from_slice(value.as_bytes());
    }

    fn row(partition_key: &str, row_key: &str) -> PersistedRow {
        let mut bytes = Vec::new();
        string_field(&mut bytes, 1, partition_key);
        string_field(&mut bytes, 2, row_key);

        PersistedRow {
            schema_id: SCHEMA_ID,
            row: bytes,
        }
    }

    /// The shape the archive carries for the table, built through the same
    /// `SchemaBuilder` the client's entity macro expands into.
    fn schemas() -> AHashMap<u64, Arc<EntitySchema>> {
        let schema = SchemaBuilder::new("TraderEntity")
            .add_field(DeclaredField::scalar(
                "PartitionKey",
                1,
                Scalar::String,
                false,
            ))
            .add_field(DeclaredField::scalar("RowKey", 2, Scalar::String, false))
            .build()
            .serialize();

        let mut result = AHashMap::new();
        result.insert(
            SCHEMA_ID,
            Arc::new(EntitySchema {
                id: SCHEMA_ID,
                schema,
            }),
        );

        result
    }

    /// The point of the route: the rows come out under the field names the
    /// snapshot's own schema gives them, which is the only place those names
    /// still exist once the entity version that wrote them is gone.
    #[test]
    fn the_rows_are_rendered_through_the_schema_the_snapshot_carries() {
        let schemas = schemas();

        assert_eq!(
            render(
                &[row("acc-1", "rk-1"), row("acc-1", "rk-2")],
                Some(&schemas),
                &JsonSchemasCache::new(),
                None,
                None
            ),
            r#"[{"PartitionKey":"acc-1","RowKey":"rk-1"},{"PartitionKey":"acc-1","RowKey":"rk-2"}]"#
        );
    }

    /// A table the archive carried no attributes for - and therefore no schemas -
    /// is still shown, by field number. Refusing to show a row we can not name
    /// is worse than showing it plainly, and a backup is exactly where the
    /// naming is most likely to be missing.
    #[test]
    fn a_snapshot_without_schemas_still_shows_its_rows() {
        assert_eq!(
            render(
                &[row("acc-1", "rk-1")],
                None,
                &JsonSchemasCache::new(),
                None,
                None
            ),
            r#"[{"1":"acc-1","2":"rk-1"}]"#
        );
    }

    /// A row written under a schema id this snapshot does not carry falls back
    /// to field numbers rather than being dropped: the archive is what it is,
    /// and a partition half shown is still the partition.
    #[test]
    fn a_row_whose_schema_the_snapshot_lacks_is_shown_by_field_number() {
        let schemas = schemas();

        let unknown = PersistedRow {
            schema_id: SCHEMA_ID + 1,
            row: row("acc-1", "rk-1").row,
        };

        assert_eq!(
            render(
                &[unknown],
                Some(&schemas),
                &JsonSchemasCache::new(),
                None,
                None
            ),
            r#"[{"1":"acc-1","2":"rk-1"}]"#
        );
    }

    /// `skip` and `limit` window the partition, because a partition in an
    /// archive is as big as a partition anywhere else and a browser has to ask
    /// for it a page at a time.
    #[test]
    fn the_window_is_named_by_skip_and_limit() {
        let schemas = schemas();
        let cache = JsonSchemasCache::new();
        let rows = [
            row("acc-1", "rk-1"),
            row("acc-1", "rk-2"),
            row("acc-1", "rk-3"),
        ];

        assert_eq!(
            render(&rows, Some(&schemas), &cache, Some(1), Some(1)),
            r#"[{"PartitionKey":"acc-1","RowKey":"rk-2"}]"#
        );
        assert_eq!(
            render(&rows, Some(&schemas), &cache, None, Some(1)),
            r#"[{"PartitionKey":"acc-1","RowKey":"rk-1"}]"#
        );
        assert_eq!(
            render(&rows, Some(&schemas), &cache, Some(2), None),
            r#"[{"PartitionKey":"acc-1","RowKey":"rk-3"}]"#
        );
    }

    /// A window past the end is an empty array, not an error, and so is a
    /// partition whose blob held nothing: the page after the one that was there
    /// is a race, not a mistake.
    #[test]
    fn nothing_to_show_is_still_an_array() {
        let cache = JsonSchemasCache::new();

        assert_eq!(
            render(&[row("acc-1", "rk-1")], None, &cache, Some(10), None),
            "[]"
        );
        assert_eq!(render(&[], None, &cache, None, None), "[]");
    }
}
