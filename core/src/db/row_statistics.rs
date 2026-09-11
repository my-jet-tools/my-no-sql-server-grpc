use rust_extensions::date_time::DateTimeAsMicroseconds;

/// What the table knows about one row and the partition holding it.
///
/// Every number is one the database keeps for its own sake - eviction sorts by
/// the last-read marks and expiry works off `Expires` - so reporting them costs
/// nothing to maintain and says exactly why a row is still there or about to go.
pub struct RowStatistics {
    pub partition_last_read_access: DateTimeAsMicroseconds,
    pub partition_expires: Option<DateTimeAsMicroseconds>,
    pub partition_rows_count: usize,
    pub partition_content_size: usize,
    pub row_time_stamp: DateTimeAsMicroseconds,
    pub row_last_read_access: DateTimeAsMicroseconds,
    pub row_expires: Option<DateTimeAsMicroseconds>,
    /// Counted the way the partition counts it - the stored size, without
    /// `Expires`. Reporting the row in the emit unit would give two numbers that
    /// do not add up.
    pub row_stored_size: usize,
}

/// Which of the two is missing is the whole value of the answer: somebody asking
/// where their row went is told whether the partition is gone as well.
pub enum GetRowStatisticsResult {
    PartitionNotFound,
    RowNotFound,
    Found(RowStatistics),
}
