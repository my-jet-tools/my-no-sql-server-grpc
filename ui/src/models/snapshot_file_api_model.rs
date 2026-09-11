use serde::*;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SnapshotFileApiModel {
    pub name: String,
    pub size: i64,
}
