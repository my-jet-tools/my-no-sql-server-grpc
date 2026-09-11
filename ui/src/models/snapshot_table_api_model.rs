use serde::*;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SnapshotTableApiModel {
    pub name: String,
    #[serde(rename = "partitionsCount")]
    pub partitions_count: u64,
}
