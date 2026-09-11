use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;

use crate::app::AppContext;
use crate::http_server::{as_json, get_request_namespace};

use super::models::GetPartitionsInputContract;

/// The partition keys of a table, a window at a time.
///
/// The answer is an object rather than the bare array it once was, because the
/// UI is who reads it: it needs the count of the whole table to know how many
/// pages there are, and a bare array can only say how big the page it was handed
/// is. The two field names are the ones the UI deserialises, so they are as much
/// of a contract as the route itself.
#[http_route(
    method: "GET",
    route: "/api/Partitions",
    controller: "Partitions",
    description: "Partition keys of the table as `amount` - how many partitions the whole table holds - and `data`, the window of keys named by `skip` and `limit`",
    summary: "Returns how many partitions the table holds and a window of their keys",
    input_data: "GetPartitionsInputContract",
    result:[
        {status_code: 200, description: "An object: `amount` is the whole table's partition count, `data` is the requested window of keys"},
        {status_code: 404, description: "Namespace or table is not found"},
    ]
)]
pub struct GetPartitionsAction {
    app: Arc<AppContext>,
}

impl GetPartitionsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetPartitionsAction,
    input_data: GetPartitionsInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let db_table = crate::db_operations::read::get_table(&db_namespace, &input_data.table_name)?;

    let partition_keys = db_table.get_partition_keys();

    as_json(render(&partition_keys, input_data.skip, input_data.limit)).into_ok_result(true)
}

/// The answer itself, kept apart from the request so the shape can be exercised
/// without one - the same reason the bulk-delete body parser sits beside its
/// action.
fn render(partition_keys: &[String], skip: Option<usize>, limit: Option<usize>) -> String {
    // `amount` is the whole table, not the size of the window: it is what tells
    // the UI how many pages there are, and a count of what it was just handed
    // would tell it nothing it can not see.
    let amount = partition_keys.len();

    let skip = skip.unwrap_or(0);
    let limit = limit.unwrap_or(usize::MAX);

    JsonObjectWriter::new()
        .write("amount", amount)
        .write_json_array("data", |mut data| {
            for partition_key in partition_keys.iter().skip(skip).take(limit) {
                data = data.write(partition_key.as_str());
            }

            data
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partition_keys(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| key.to_string()).collect()
    }

    /// `amount` and `data`, spelled exactly so: the UI deserialises those two
    /// names off this route, and renaming either of them is a blank screen with
    /// nothing in the log to explain it.
    #[test]
    fn the_answer_names_the_count_and_the_keys() {
        assert_eq!(
            render(&partition_keys(&["acc-1", "acc-2"]), None, None),
            r#"{"amount":2,"data":["acc-1","acc-2"]}"#
        );
    }

    /// The window moves and `amount` does not: it counts the whole table, which
    /// is how the UI knows there is another page to ask for.
    #[test]
    fn the_window_moves_and_the_count_stays_the_whole_table() {
        let keys = partition_keys(&["acc-1", "acc-2", "acc-3", "acc-4"]);

        assert_eq!(
            render(&keys, Some(1), Some(2)),
            r#"{"amount":4,"data":["acc-2","acc-3"]}"#
        );
        assert_eq!(
            render(&keys, None, Some(2)),
            r#"{"amount":4,"data":["acc-1","acc-2"]}"#
        );
        assert_eq!(
            render(&keys, Some(3), None),
            r#"{"amount":4,"data":["acc-4"]}"#
        );
    }

    /// A window past the end is an empty one, not an error: the page a caller
    /// asks for after the table shrank is a race, not a mistake.
    #[test]
    fn a_window_past_the_end_is_empty_and_still_counts_the_table() {
        let keys = partition_keys(&["acc-1", "acc-2"]);

        assert_eq!(
            render(&keys, Some(10), Some(2)),
            r#"{"amount":2,"data":[]}"#
        );
    }

    /// An empty table answers the same shape rather than nothing: the UI parses
    /// one model off this route, and it has to hold for a table nobody has
    /// written to yet.
    #[test]
    fn an_empty_table_answers_the_same_shape() {
        assert_eq!(render(&[], None, None), r#"{"amount":0,"data":[]}"#);
    }
}
