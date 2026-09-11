//! The input contracts.
//!
//! Every one of them declares the `ns` header, and **none of them is read for
//! it**: the namespace is resolved off the request by
//! [`crate::http_server::get_request_namespace`], which is the only place the
//! `?ns=` fallback lives. The declarations are here because swagger is built
//! from these contracts and a header read straight off the request would be
//! invisible in it - so they are documentation, and reading them instead is
//! exactly how the fallback goes missing.

use my_http_server::macros::*;
use serde::{Deserialize, Serialize};

use crate::data_sync_period::DataSyncPeriod;

#[derive(Serialize, Deserialize, Debug, MyHttpObjectStructure)]
pub struct IsAliveResponse {
    pub name: String,
    pub version: String,
    pub time: String,
}

#[derive(MyHttpInput)]
pub struct GetTablesInputContract {
    #[http_header(name = "ns"; description = "Namespace to work in. Empty or absent means the default namespace")]
    pub namespace: Option<String>,
}

#[derive(MyHttpInput)]
pub struct GetPartitionsInputContract {
    #[http_header(name = "ns"; description = "Namespace to work in. Empty or absent means the default namespace")]
    pub namespace: Option<String>,

    #[http_query(name = "tableName"; description = "Name of a table")]
    pub table_name: String,

    // A table can hold a million partitions, and this route exists for the UI to
    // look at them - a page at a time, with `amount` telling it how many pages
    // there are. Without the window the answer is the whole key set on every
    // poll, which is the one shape a browser can not use.
    #[http_query(name = "skip"; description = "Amount of partition keys to skip before collecting them")]
    pub skip: Option<usize>,

    #[http_query(name = "limit"; description = "Maximum amount of partition keys to return")]
    pub limit: Option<usize>,
}

/// The namespace of a write is resolved **without** creating it, everywhere
/// except table creation: emptying, deleting or setting attributes on a table in
/// a namespace which does not exist is a mistake, and resolving it into being
/// would leave a folder on disk behind every one of those mistakes.
#[derive(MyHttpInput)]
pub struct DeleteRowInputContract {
    #[http_header(name = "ns"; description = "Namespace to work in. Empty or absent means the default namespace")]
    pub namespace: Option<String>,

    #[http_query(name = "tableName"; description = "Name of a table")]
    pub table_name: String,

    #[http_query(name = "partitionKey"; description = "Partition Key")]
    pub partition_key: String,

    #[http_query(name = "rowKey"; description = "Row Key")]
    pub row_key: String,

    #[http_query(name = "syncPeriod"; description = "How long the change may wait before it is written to disk"; default)]
    pub sync_period: DataSyncPeriod,
}

#[derive(MyHttpInput)]
pub struct DeletePartitionsInputContract {
    #[http_header(name = "ns"; description = "Namespace to work in. Empty or absent means the default namespace")]
    pub namespace: Option<String>,

    #[http_query(name = "tableName"; description = "Name of a table")]
    pub table_name: String,

    #[http_query(name = "partitionKey"; description = "Partition Keys to delete. Repeat the parameter for each one")]
    pub partition_keys: Vec<String>,

    #[http_query(name = "syncPeriod"; description = "How long the change may wait before it is written to disk"; default)]
    pub sync_period: DataSyncPeriod,
}

impl DeletePartitionsInputContract {
    /// Everything the caller named.
    pub fn get_partition_keys(&self) -> Vec<String> {
        self.partition_keys.clone()
    }
}

#[derive(MyHttpInput)]
pub struct BulkDeleteInputContract {
    #[http_header(name = "ns"; description = "Namespace to work in. Empty or absent means the default namespace")]
    pub namespace: Option<String>,

    #[http_query(name = "tableName"; description = "Name of a table")]
    pub table_name: String,

    /// A partition key to its row keys: `{"acc-1": ["rk-1", "rk-2"], "acc-2": ["rk-3"]}`.
    /// An object rather than a list of pairs because that is what it is - the
    /// keys of a map can not repeat, and the shape says so.
    #[http_body_raw(description = "Partition key -> row keys, as a JSON object")]
    pub body: Vec<u8>,

    #[http_query(name = "syncPeriod"; description = "How long the change may wait before it is written to disk"; default)]
    pub sync_period: DataSyncPeriod,
}

#[derive(MyHttpInput)]
pub struct TableInputContract {
    #[http_header(name = "ns"; description = "Namespace to work in. Empty or absent means the default namespace")]
    pub namespace: Option<String>,

    #[http_query(name = "tableName"; description = "Name of a table")]
    pub table_name: String,

    #[http_query(name = "syncPeriod"; description = "How long the change may wait before it is written to disk"; default)]
    pub sync_period: DataSyncPeriod,
}

/// What a table does with itself. The same shape creates one and changes one -
/// they are the same set of decisions, made at different moments.
#[derive(MyHttpInput)]
pub struct TableAttributesInputContract {
    #[http_header(name = "ns"; description = "Namespace to work in. Empty or absent means the default namespace")]
    pub namespace: Option<String>,

    #[http_query(name = "tableName"; description = "Name of a table")]
    pub table_name: String,

    #[http_query(name = "persist"; description = "Whether the table is written to disk at all"; default = true)]
    pub persist: bool,

    #[http_query(name = "maxPartitionsAmount"; description = "Evict the least recently read partition past this many. Absent or 0 - no limit")]
    pub max_partitions_amount: Option<i64>,

    #[http_query(name = "maxRowsPerPartitionAmount"; description = "Evict the least recently read row of a partition past this many. Absent or 0 - no limit")]
    pub max_rows_per_partition_amount: Option<i64>,

    #[http_query(name = "syncPeriod"; description = "How long the change may wait before it is written to disk"; default)]
    pub sync_period: DataSyncPeriod,
}

#[derive(MyHttpInput)]
pub struct GetRowStatisticsInputContract {
    #[http_header(name = "ns"; description = "Namespace to work in. Empty or absent means the default namespace")]
    pub namespace: Option<String>,

    #[http_query(name = "tableName"; description = "Name of a table")]
    pub table_name: String,

    #[http_query(name = "partitionKey"; description = "Partition Key")]
    pub partition_key: String,

    #[http_query(name = "rowKey"; description = "Row Key")]
    pub row_key: String,
}

/// The one contract here which names no namespace: the MCP write window is a
/// property of the server, the way a backup or a flush is, and a switch thrown
/// per namespace would be a switch somebody forgets to throw back.
#[derive(MyHttpInput)]
pub struct McpWritesInputContract {
    #[http_query(name = "enabled"; description = "true - open the MCP write tools for 10 minutes. false - shut them now")]
    pub enabled: bool,
}

#[derive(MyHttpInput)]
pub struct GetRowsInputContract {
    #[http_header(name = "ns"; description = "Namespace to work in. Empty or absent means the default namespace")]
    pub namespace: Option<String>,

    #[http_query(name = "tableName"; description = "Name of a table")]
    pub table_name: String,

    #[http_query(name = "partitionKey"; description = "Partition Key. Absent - every partition")]
    pub partition_key: Option<String>,

    #[http_query(name = "rowKey"; description = "Row Key. Absent - every row")]
    pub row_key: Option<String>,

    #[http_query(name = "skip"; description = "Amount of rows to skip before collecting them")]
    pub skip: Option<usize>,

    #[http_query(name = "limit"; description = "Maximum amount of rows to return")]
    pub limit: Option<usize>,
}
