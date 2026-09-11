/// Rows named by key, grouped by the partition they live in - what a delete
/// talks about, on the way in and on the way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionRowKeys {
    pub partition_key: String,
    pub row_keys: Vec<String>,
}
