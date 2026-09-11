use rust_extensions::date_time::DateTimeAsMicroseconds;

/// What a reader tells the server about one partition it looked at.
///
/// Two different things at once, and they are two halves of the same idea. The
/// last-read marks are what keeps the eviction of `max_partitions_amount` from
/// throwing out a partition everybody reads but nobody rewrites. The expiry is
/// sliding expiration: a reader which keeps asking for a row is what keeps the
/// row alive, so the TTL is pushed forward by the reading itself.
///
/// The two `Option<Option<..>>` are not an accident: the outer one is "leave it
/// alone", the inner one is "set it to never". A single `Option` could not tell
/// those apart, and they are opposite instructions.
#[derive(Default)]
pub struct UpdateReadStatistics {
    pub update_partition_last_read: bool,
    pub update_rows_last_read: bool,
    pub set_partition_expires: Option<Option<DateTimeAsMicroseconds>>,
    pub set_rows_expires: Option<Option<DateTimeAsMicroseconds>>,
}

impl UpdateReadStatistics {
    /// Nothing to do at all, which is what most of these are - a reader that
    /// only wants the changes sends an empty list.
    pub fn is_empty(&self) -> bool {
        !self.update_partition_last_read
            && !self.update_rows_last_read
            && self.set_partition_expires.is_none()
            && self.set_rows_expires.is_none()
    }

    /// Whether it changes something the disk knows about. A row's `Expires` is
    /// part of the stored row; a partition's is not, and neither are the
    /// last-read marks - so only this is worth a write.
    pub fn touches_the_disk(&self) -> bool {
        self.set_rows_expires.is_some()
    }
}
