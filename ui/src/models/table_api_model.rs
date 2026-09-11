use serde::*;

/// One table as `GET /api/Status` reports it, inside its namespace.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct TableApiModel {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub persist: bool,
    #[serde(rename = "maxPartitionsAmount", default)]
    pub max_partitions_amount: Option<u64>,
    #[serde(rename = "maxRowsPerPartitionAmount", default)]
    pub max_rows_per_partition_amount: Option<u64>,
    #[serde(rename = "partitionsCount", default)]
    pub partitions_count: u64,
    #[serde(rename = "rowsCount", default)]
    pub rows_count: u64,
    /// How many entity schemas this table has been written with. One is the
    /// normal case, so anything else is worth seeing: either a deploy going
    /// through or two different entities aimed at one table.
    #[serde(rename = "schemasCount", default)]
    pub schemas_count: u64,
    #[serde(rename = "dataSize", default)]
    pub data_size: u64,
    #[serde(default)]
    pub created: String,
    /// Absent means nothing has written to it since the process started - the
    /// moment is not kept on disk.
    #[serde(rename = "lastWriteAt", default)]
    pub last_write_at: Option<String>,
}
