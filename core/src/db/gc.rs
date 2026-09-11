use super::PartitionRowKeys;

/// What one garbage collection pass took out of a table.
///
/// Rows which went out with a partition are **not** listed again: the subscriber
/// is told the partition is gone, and a row of a partition it no longer has is
/// not something it can be told about twice.
#[derive(Default)]
pub struct GcResult {
    /// Partitions dropped whole - what `CleanPartitions` names.
    pub partitions_removed: Vec<String>,
    /// Rows dropped out of partitions which stayed.
    pub rows_removed: Vec<PartitionRowKeys>,
    /// Every partition whose content on disk is no longer what the table holds.
    pub partitions_to_persist: Vec<String>,
}

impl GcResult {
    pub fn is_empty(&self) -> bool {
        self.partitions_removed.is_empty() && self.rows_removed.is_empty()
    }
}
