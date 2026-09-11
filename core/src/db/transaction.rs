use std::sync::Arc;

use super::{DbRow, PartitionRowKeys};

/// One thing a transaction asks the table to do.
///
/// A transaction is the only place where writes of different kinds land
/// together, and the list is applied in the order the client built it: a row
/// deleted and then written again is not the same as a row written and then
/// deleted, so nothing here may be reordered.
pub enum TransactionAction {
    CleanTable,
    DeletePartitions(Vec<String>),
    DeleteRows(PartitionRowKeys),
    InsertOrReplace(Vec<Arc<DbRow>>),
}

/// One thing a transaction actually did - the same list, with what changed
/// nothing left out and neighbours of the same kind merged.
///
/// Merging only ever joins changes which are next to each other, for the same
/// reason the actions may not be reordered.
pub enum AppliedChange {
    TableCleaned,
    PartitionsDeleted(Vec<String>),
    RowsDeleted(Vec<PartitionRowKeys>),
    RowsWritten(Vec<Arc<DbRow>>),
}

pub struct TransactionResult {
    /// What the subscribers have to be told, in the order it happened.
    pub changes: Vec<AppliedChange>,
    /// Every partition whose content on disk is no longer what the table holds.
    pub partitions_to_persist: Vec<String>,
}
