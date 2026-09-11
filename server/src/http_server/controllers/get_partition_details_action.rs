use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;
use my_no_sql_grpc_core::db::{DbTable, GetRowsFilter};

use crate::app::AppContext;
use crate::http_server::{as_json, get_request_namespace};

use super::models::GetPartitionsInputContract;

/// What each partition of a table holds and what it costs, a window at a time.
///
/// The envelope is the one `/api/Partitions` answers in - `amount` for the whole
/// table, `data` for the window named by `skip` and `limit` - because this is
/// the same question with the numbers attached, and a route the UI pages has to
/// say how many pages there are. The names inside `data` are the ones
/// `PartitionMetricApiModel` in the UI deserialises.
///
/// The inputs are `/api/Partitions`' own contract: the two routes take the same
/// four things and both count partitions, so a second declaration of them would
/// be a second spelling to keep in step.
#[http_route(
    method: "GET",
    route: "/api/Partitions/Details",
    controller: "Partitions",
    description: "Per-partition metrics of the table as `amount` - how many partitions the whole table holds - and `data`, the window named by `skip` and `limit`, each entry carrying the partition key, its records count and its data size in bytes",
    summary: "Returns how many partitions the table holds and the metrics of a window of them",
    input_data: "GetPartitionsInputContract",
    result:[
        {status_code: 200, description: "An object: `amount` is the whole table's partition count, `data` is the requested window of per-partition metrics"},
        {status_code: 404, description: "Namespace or table is not found"},
    ]
)]
pub struct GetPartitionDetailsAction {
    app: Arc<AppContext>,
}

impl GetPartitionDetailsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetPartitionDetailsAction,
    input_data: GetPartitionsInputContract,
    ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let db_namespace = get_request_namespace(&action.app, ctx)?;

    let db_table = crate::db_operations::read::get_table(&db_namespace, &input_data.table_name)?;

    let partition_keys = db_table.get_partition_keys();

    // The window is applied **before** the numbers are collected, and not inside
    // the render the way `/api/Partitions` does it: a key costs nothing to hand
    // back, while measuring a partition is a lock of its own, so measuring a
    // million of them to show ten is exactly what the window is here to prevent.
    let metrics: Vec<PartitionMetric> = window(&partition_keys, input_data.skip, input_data.limit)
        .iter()
        .map(|partition_key| collect(&db_table, partition_key))
        .collect();

    // `false` is "do not write telemetry": the open page re-polls this every few
    // seconds to keep its counters live, and a poll has no business filling the
    // log.
    as_json(render(partition_keys.len(), &metrics)).into_ok_result(false)
}

/// One partition, collected before anything is formatted.
struct PartitionMetric {
    partition_key: String,
    records_count: usize,
    data_size: usize,
}

/// The keys the caller asked for. Kept apart from the request because this is
/// the half which has to happen before the measuring does.
fn window(partition_keys: &[String], skip: Option<usize>, limit: Option<usize>) -> &[String] {
    let from = skip.unwrap_or(0).min(partition_keys.len());

    // Saturating: no `limit` is "to the end", which as a number is `usize::MAX`,
    // and `from + MAX` overflows - a panic in a debug build and a nonsense
    // window in a release one.
    let to = from
        .saturating_add(limit.unwrap_or(usize::MAX))
        .min(partition_keys.len());

    &partition_keys[from..to]
}

/// The two numbers a partition keeps for its own sake - how many rows it holds
/// and what they add up to - taken from the same place `/api/Row/Statistics`
/// reports them from, so the page and the row view can not disagree.
///
/// They are reachable through a row because that is what the table exposes them
/// through: the statistics of any row of the partition carry the partition's
/// own accounting, and reading them moves no last-read mark.
fn collect(db_table: &DbTable, partition_key: &str) -> PartitionMetric {
    let statistics = first_row_key(db_table, partition_key).and_then(|row_key| {
        crate::db_operations::read::get_row_statistics(db_table, partition_key, &row_key).ok()
    });

    // Both refusals mean one thing here: the partition is no longer the one
    // whose key was collected a moment ago - somebody deleted or emptied it in
    // between. Zeros keep the window the length the caller asked for, and the
    // next poll will not mention the key at all.
    let (records_count, data_size) = match statistics {
        Some(statistics) => (
            statistics.partition_rows_count,
            statistics.partition_content_size,
        ),
        None => (0, 0),
    };

    PartitionMetric {
        partition_key: partition_key.to_string(),
        records_count,
        data_size,
    }
}

/// Any row key of the partition, which is all the statistics need to answer
/// about the partition holding it.
///
/// The table's own `get_rows` and not [`crate::db_operations::read::get_rows`]:
/// that one moves the last-read mark of every row it hands back, and this route
/// is polled for as long as somebody has the page open - a metrics view which
/// touched the marks would rescue from eviction precisely the cold partitions it
/// is displaying.
fn first_row_key(db_table: &DbTable, partition_key: &str) -> Option<String> {
    let rows = db_table.get_rows(&GetRowsFilter {
        partition_key: Some(partition_key),
        row_key: None,
        skip: None,
        // One row is enough: the numbers being collected belong to the
        // partition, and every row of it carries the same ones.
        limit: Some(1),
    });

    Some(rows.first()?.get_row_key().to_string())
}

/// The answer itself, kept apart from the request so the shape can be exercised
/// without one - the same reason the sibling route's render sits beside its
/// action.
fn render(amount: usize, metrics: &[PartitionMetric]) -> String {
    JsonObjectWriter::new()
        // `amount` is the whole table, not the length of the window: it is what
        // tells the UI how many pages there are, and a count of what it was just
        // handed would tell it nothing it can not see.
        .write("amount", amount)
        .write_json_array("data", |mut data| {
            for metric in metrics {
                data = data.write_json_object(|partition| {
                    partition
                        .write("partitionKey", metric.partition_key.as_str())
                        .write("recordsCount", metric.records_count)
                        .write("dataSize", metric.data_size)
                });
            }

            data
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(entries: &[(&str, usize, usize)]) -> Vec<PartitionMetric> {
        entries
            .iter()
            .map(
                |(partition_key, records_count, data_size)| PartitionMetric {
                    partition_key: partition_key.to_string(),
                    records_count: *records_count,
                    data_size: *data_size,
                },
            )
            .collect()
    }

    fn partition_keys(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| key.to_string()).collect()
    }

    /// `amount`, `data`, and inside it `partitionKey`, `recordsCount` and
    /// `dataSize`, spelled exactly so: the UI deserialises those names off this
    /// route into `PartitionMetricApiModel`, and renaming any of them leaves the
    /// data page blank with nothing in the log to explain it.
    #[test]
    fn the_answer_names_the_count_and_the_metrics_of_each_partition() {
        assert_eq!(
            render(2, &metrics(&[("acc-1", 3, 128), ("acc-2", 1, 64)])),
            r#"{"amount":2,"data":[{"partitionKey":"acc-1","recordsCount":3,"dataSize":128},{"partitionKey":"acc-2","recordsCount":1,"dataSize":64}]}"#
        );
    }

    /// `amount` counts the whole table while `data` carries the window, which is
    /// how the UI knows there is another page to ask for.
    #[test]
    fn the_count_is_the_whole_table_and_the_data_is_the_window() {
        assert_eq!(
            render(9, &metrics(&[("acc-4", 2, 32)])),
            r#"{"amount":9,"data":[{"partitionKey":"acc-4","recordsCount":2,"dataSize":32}]}"#
        );
    }

    /// A window past the end still says how big the table is - that is what the
    /// caller needs in order to ask for a page which exists.
    #[test]
    fn an_empty_window_still_counts_the_table() {
        assert_eq!(render(4, &[]), r#"{"amount":4,"data":[]}"#);
    }

    /// An empty table answers the same shape rather than nothing: the UI parses
    /// one model off this route, and it has to hold for a table nobody has
    /// written to yet.
    #[test]
    fn an_empty_table_answers_the_same_shape() {
        assert_eq!(render(0, &[]), r#"{"amount":0,"data":[]}"#);
    }

    /// The window, which decides what gets measured at all.
    #[test]
    fn the_window_is_the_slice_the_caller_named() {
        let keys = partition_keys(&["acc-1", "acc-2", "acc-3", "acc-4"]);

        assert_eq!(window(&keys, None, None), &keys[..]);
        assert_eq!(window(&keys, Some(1), Some(2)), &keys[1..3]);
        assert_eq!(window(&keys, None, Some(2)), &keys[..2]);
        assert_eq!(window(&keys, Some(3), None), &keys[3..]);
    }

    /// A window past the end is an empty one, not an out-of-range slice: the
    /// page a caller asks for after the table shrank is a race, not a mistake.
    /// A `limit` of its own would overflow the sum here if it were not
    /// saturating.
    #[test]
    fn a_window_past_the_end_is_empty_rather_than_a_panic() {
        let keys = partition_keys(&["acc-1", "acc-2"]);

        assert!(window(&keys, Some(10), Some(2)).is_empty());
        assert!(window(&keys, Some(2), None).is_empty());
        assert_eq!(window(&keys, Some(1), Some(usize::MAX)), &keys[1..]);
        assert!(window(&[], None, None).is_empty());
    }
}
