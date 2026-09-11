use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use my_json::json_writer::JsonArrayWriter;
use my_no_sql_grpc_core::db::GetRowsFilter;

use crate::app::AppContext;
use crate::http_server::get_request_namespace;
use crate::json_view::SchemaIndex;

use super::models::DownloadRowsInputContract;

/// One partition's rows as a file the browser saves.
///
/// The same rendering `GET /api/Row` answers with - a row is protobuf, and it is
/// shown only through the schema it was written with - handed over as an
/// attachment instead of a body. It is the one read whose namespace arrives as
/// `?ns=`: a download is a top level navigation, an `<a href>` with nowhere to
/// put a header, which is the case the query-parameter fallback in
/// [`get_request_namespace`] exists for.
#[http_route(
    method: "GET",
    route: "/api/Row/Download",
    controller: "Row",
    description: "One partition's rows as a JSON file, rendered through the schema each row was written with. The namespace goes in `?ns=` here - a download is a top level navigation and can not carry the `ns` header",
    summary: "Downloads the rows of one partition as a JSON file",
    input_data: "DownloadRowsInputContract",
    result:[
        {status_code: 200, description: "A JSON file of the partition's rows"},
        {status_code: 404, description: "Namespace or table is not found"},
    ]
)]
pub struct DownloadRowsAction {
    app: Arc<AppContext>,
}

impl DownloadRowsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &DownloadRowsAction,
    input_data: DownloadRowsInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let db_table = crate::db_operations::read::get_table(&db_namespace, &input_data.table_name)?;

    let db_rows = crate::db_operations::read::get_rows(
        &db_table,
        &GetRowsFilter {
            partition_key: Some(input_data.partition_key.as_str()),
            row_key: None,
            skip: None,
            limit: None,
        },
    );

    // The schema is looked up per row, not per table: after a client changes its
    // entity, old and new rows sit side by side in the same partition, and each
    // one has to be rendered with the schema it was written with.
    let rows: Vec<_> = db_rows
        .iter()
        .map(|db_row| {
            let schema = db_table
                .get_schema(db_row.get_schema_id())
                .and_then(|schema| action.app.json_schemas.get_or_build(&schema));

            (db_row.to_vec(), schema)
        })
        .collect();

    let content = render(
        rows.iter()
            .map(|(row_bytes, schema)| (row_bytes.as_slice(), schema.as_deref())),
    );

    HttpOutput::as_file(
        file_name(&input_data.table_name, &input_data.partition_key),
        content.into_bytes(),
    )
    .into_ok_result(true)
}

/// The rows as one JSON array - the same array `GET /api/Row` answers with, so
/// what the file holds is what the screen showed.
///
/// Kept apart from the request, and taking the bytes and the resolved schema
/// rather than the table, so the file can be exercised without a server.
fn render<'s>(rows: impl Iterator<Item = (&'s [u8], Option<&'s SchemaIndex>)>) -> String {
    let mut writer = JsonArrayWriter::new();

    for (row_bytes, schema) in rows {
        writer = writer
            .write_json_object(|row| crate::json_view::write_row_as_json(row, row_bytes, schema));
    }

    writer.build()
}

/// How much of the table name and of the partition key the file name carries.
///
/// A partition key has no length limit worth relying on, and a file name does:
/// a name past 255 bytes is refused by the file systems the browser saves onto,
/// and the save fails with nothing to explain it.
const MAX_NAME_PART_LEN: usize = 60;

/// The name the browser saves the file under - the table and the partition, so
/// a folder of downloads still says which is which.
///
/// It goes into `Content-Disposition` as it is, so everything but the few
/// characters a file name and a header can both carry is replaced: a partition
/// key is caller data, it may hold a quote, a slash or a line break, and a
/// header assembled around one of those is a header the response builder
/// refuses to build.
///
/// The `.json` tail is not decoration - the content type of the answer is
/// detected from it.
fn file_name(table_name: &str, partition_key: &str) -> String {
    format!(
        "{}-{}.json",
        as_name_part(table_name),
        as_name_part(partition_key)
    )
}

fn as_name_part(value: &str) -> String {
    let mut result = String::with_capacity(value.len());

    for char in value.chars().take(MAX_NAME_PART_LEN) {
        if char.is_ascii_alphanumeric() || matches!(char, '-' | '_' | '.') {
            result.push(char);
        } else {
            result.push('_');
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::json_view::tests::{build_row, build_schema, expected_json};

    fn schema() -> SchemaIndex {
        SchemaIndex::build(&build_schema()).unwrap()
    }

    fn rows_of(rows: &[(Vec<u8>, Option<&SchemaIndex>)]) -> String {
        render(
            rows.iter()
                .map(|(bytes, schema)| (bytes.as_slice(), *schema)),
        )
    }

    /// The file is the rows rendered through their schema - the same array the
    /// screen was showing, not the bytes underneath it.
    #[test]
    fn the_file_holds_the_rows_rendered_through_their_schema() {
        let schema = schema();

        assert_eq!(
            rows_of(&[(build_row(), Some(&schema))]),
            format!("[{}]", expected_json())
        );
    }

    #[test]
    fn every_row_of_the_partition_is_in_the_file() {
        let schema = schema();

        assert_eq!(
            rows_of(&[(build_row(), Some(&schema)), (build_row(), Some(&schema))]),
            format!("[{},{}]", expected_json(), expected_json())
        );
    }

    /// A row whose schema the server does not have is still handed over, by
    /// field number: a download the server refuses because it can not name the
    /// fields is a download of the one row somebody needed to look at.
    #[test]
    fn a_row_without_a_schema_is_still_in_the_file() {
        let json = rows_of(&[(build_row(), None)]);

        assert!(json.starts_with(r#"[{"1":"acc-1""#), "{}", json);
    }

    /// An empty partition is an empty array, not a 404: the same shape whatever
    /// was found, which is the rule `GET /api/Row` already answers by.
    #[test]
    fn an_empty_partition_is_an_empty_array() {
        assert_eq!(rows_of(&[]), "[]");
    }

    #[test]
    fn the_file_is_named_after_the_table_and_the_partition() {
        assert_eq!(file_name("traders", "acc-1"), "traders-acc-1.json");
    }

    /// The name ends up in a header, so what a header can not carry can not be
    /// in it - a partition key is whatever the writer chose.
    #[test]
    fn the_name_carries_nothing_a_header_could_not() {
        assert_eq!(file_name("traders", "acc/1\"\r\n"), "traders-acc_1___.json");
    }

    /// A key long enough to push the name past what a file system accepts is cut
    /// instead of failing the save.
    #[test]
    fn a_long_partition_key_is_cut() {
        assert_eq!(
            file_name("traders", &"x".repeat(200)),
            format!("traders-{}.json", "x".repeat(MAX_NAME_PART_LEN))
        );
    }
}
