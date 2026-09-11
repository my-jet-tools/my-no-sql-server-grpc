use serde::{Deserialize, Serialize};

/// One entry of `GET /api/Tables/List`.
///
/// Carries its metrics, unlike the JSON version's list - so the data page does
/// not have to borrow them from the status poll.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TableListItemApiModel {
    pub name: String,
    #[serde(default)]
    pub persist: bool,
    #[serde(rename = "partitionsCount", default)]
    pub partitions_count: u64,
    #[serde(rename = "rowsCount", default)]
    pub rows_count: u64,
    #[serde(rename = "dataSize", default)]
    pub data_size: u64,
    #[serde(rename = "maxPartitionsAmount", default)]
    pub max_partitions_amount: Option<u64>,
    #[serde(rename = "maxRowsPerPartitionAmount", default)]
    pub max_rows_per_partition_amount: Option<u64>,
    #[serde(default)]
    pub created: String,
}

/// One entry of `GET /api/Partitions/Details`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PartitionMetricApiModel {
    #[serde(rename = "partitionKey")]
    pub partition_key: String,
    #[serde(rename = "recordsCount", default)]
    pub records_count: u64,
    #[serde(rename = "dataSize", default)]
    pub data_size: u64,
}

/// The `{amount, data}` envelope the paging endpoints answer with: `amount` is
/// the whole table, `data` the window that was asked for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PagedApiModel<TItem> {
    #[serde(default)]
    pub amount: u64,
    #[serde(default = "Vec::new")]
    pub data: Vec<TItem>,
}
