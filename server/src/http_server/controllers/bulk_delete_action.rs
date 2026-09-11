use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_reader::JsonFirstLineIterator;
use my_json::json_writer::JsonObjectWriter;
use my_no_sql_grpc_core::db::PartitionRowKeys;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::{as_json, get_request_namespace};

use super::models::BulkDeleteInputContract;

/// Deleting rows by key across several partitions, in one entry into the table.
///
/// `POST` rather than `DELETE` because it carries a body, and a body on a
/// `DELETE` is the kind of thing proxies drop.
#[http_route(
    method: "POST",
    route: "/api/Rows/BulkDelete",
    controller: "Rows",
    description: "Deletes rows named by key across several partitions, in one entry into the table. The body is a JSON object of partition key -> row keys",
    summary: "Deletes rows by key in several partitions at once",
    input_data: "BulkDeleteInputContract",
    result:[
        {status_code: 200, description: "How many rows were actually there"},
        {status_code: 400, description: "The body is not an object of partition key -> row keys"},
        {status_code: 404, description: "Namespace or table is not found"},
    ]
)]
pub struct BulkDeleteAction {
    app: Arc<AppContext>,
}

impl BulkDeleteAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &BulkDeleteAction,
    input_data: BulkDeleteInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let db_table = crate::db_operations::read::get_table(&db_namespace, &input_data.table_name)?;

    // Parsed before anything is touched: a body the server can not read leaves
    // the table exactly as it was, the same rule the streaming writes follow.
    let partitions = parse(&input_data.body).map_err(HttpFailResult::as_validation_error)?;

    let deleted = crate::db_operations::write::bulk_delete(
        &action.app,
        &db_namespace,
        &db_table,
        partitions,
        input_data
            .sync_period
            .get_sync_moment(DateTimeAsMicroseconds::now()),
    );

    // How many were there, not how many were named - the difference is the
    // reason to answer with a number at all.
    let json = JsonObjectWriter::new()
        .write("rowsDeleted", deleted)
        .build();

    as_json(json).into_ok_result(true)
}

/// The body, as the table wants it.
///
/// It answers with a plain message rather than an `HttpFailResult`: reading a
/// JSON object is not an HTTP concern, and keeping it out of the signature keeps
/// this testable without a request.
fn parse(body: &[u8]) -> Result<Vec<PartitionRowKeys>, String> {
    let mut result = Vec::new();

    let reader = JsonFirstLineIterator::new(body);

    while let Some(next) = reader.get_next() {
        let (partition_key, row_keys) = next.map_err(bad_body)?;

        let partition_key = partition_key.as_str().map_err(bad_body)?.to_string();

        let items = row_keys
            .unwrap_as_array()
            .map_err(|_| format!("the row keys of partition '{partition_key}' are not an array"))?;

        let mut keys = Vec::new();

        while let Some(item) = items.get_next() {
            let item = item.map_err(bad_body)?;

            // `as_str` is happy to hand back a number as its text, so the
            // type is checked rather than the conversion: a row key which
            // arrived as `1` is a caller bug, not a key called "1".
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
    format!("the body is not a JSON object of partition key -> row keys: {err:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_becomes_the_keys_to_delete() {
        let parsed = parse(br#"{"acc-1":["rk-1","rk-2"],"acc-2":["rk-3"]}"#).unwrap();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].partition_key, "acc-1");
        assert_eq!(parsed[0].row_keys, vec!["rk-1", "rk-2"]);
        assert_eq!(parsed[1].partition_key, "acc-2");
        assert_eq!(parsed[1].row_keys, vec!["rk-3"]);
    }

    /// A partition named with nothing to delete in it is not an error - it asks
    /// for nothing and nothing is what it gets.
    #[test]
    fn an_empty_list_of_row_keys_is_allowed() {
        let parsed = parse(br#"{"acc-1":[]}"#).unwrap();

        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].row_keys.is_empty());
    }

    #[test]
    fn an_empty_body_asks_for_nothing() {
        assert!(parse(b"{}").unwrap().is_empty());
    }

    /// Refused rather than half-applied: the table is entered once, after the
    /// whole body has been read.
    #[test]
    fn a_body_which_is_not_that_shape_is_refused() {
        assert!(parse(b"[]").is_err());
        assert!(parse(br#"{"acc-1":"rk-1"}"#).is_err());
        assert!(parse(br#"{"acc-1":[1,2]}"#).is_err());
        assert!(parse(b"not json at all").is_err());
    }
}
