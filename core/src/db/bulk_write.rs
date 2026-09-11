use std::sync::Arc;

use super::DbRow;

/// What a batch does to the table it lands in.
///
/// The mode is decided by the caller once per batch, never per row: the whole
/// point of a bulk write is that the table goes from one state to the next in a
/// single step, so a batch which cleaned for some of its rows and not for others
/// would have no state to describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkWriteMode {
    /// Every row replaces whatever was stored under its keys.
    InsertOrReplace,
    /// The stored row is kept unless the incoming one is strictly newer. What is
    /// compared is the `TimeStamp` the rows carry, so the caller has to build
    /// them from the client's own value.
    InsertOrReplaceIfNew,
    /// The partitions the batch names are emptied first, so what is left in them
    /// is exactly the batch. Partitions the batch does not name are untouched.
    CleanPartitionsAndInsert,
    /// The whole table is emptied first - an empty batch included, which is the
    /// same thing as cleaning the table.
    CleanTableAndInsert,
}

/// What a bulk write actually changed - everything the rest of the server needs
/// in order to tell the disk and the subscribers about it.
pub struct BulkWriteResult {
    /// The rows which landed, in the order the batch carried them.
    ///
    /// For `InsertOrReplaceIfNew` this is only the accepted ones: a rejected row
    /// is older than what is stored, so handing it to a subscriber would push
    /// that subscriber's cache backwards.
    pub written: Vec<Arc<DbRow>>,
    /// Every partition whose content on disk is no longer what the table holds:
    /// the ones the batch wrote into, and the ones a cleaning mode emptied.
    pub partitions_to_persist: Vec<String>,
}

/// The distinct partition keys of a batch, sorted - the form both the cleaning
/// modes and the persist marks want.
pub(super) fn distinct_partition_keys(rows: &[Arc<DbRow>]) -> Vec<String> {
    let mut result: Vec<String> = rows
        .iter()
        .map(|db_row| db_row.get_partition_key().to_string())
        .collect();

    result.sort_unstable();
    result.dedup();

    result
}
