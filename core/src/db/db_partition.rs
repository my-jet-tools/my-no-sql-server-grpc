use std::sync::Arc;

use rust_extensions::date_time::{AtomicDateTimeAsMicroseconds, DateTimeAsMicroseconds};
use rust_extensions::sorted_vec::{EntityWithStrKey, SortedVecOfArcWithStrKey};

use super::DbRow;

/// Rows sharing one `PartitionKey`, kept sorted by `RowKey`.
///
/// A partition is also the unit of persistence: one partition is one slot on
/// disk, which is why the accumulated content size is tracked here rather than
/// recomputed by walking the rows. What is accumulated is the part of a row
/// which can not change - see [`DbRow::get_stored_size`].
pub struct DbPartition {
    partition_key: String,
    rows: SortedVecOfArcWithStrKey<DbRow>,
    content_size: usize,
    last_read_access: AtomicDateTimeAsMicroseconds,
    /// `0` means the partition never expires.
    ///
    /// Unlike a row's, this one is **not** written to disk. It is set by the
    /// readers which care about the partition, on every call they make, so a
    /// restart loses at most the time until their next one - and persisting it
    /// would mean a format the partition file does not have, for a value that is
    /// re-asserted seconds later anyway.
    expires: AtomicDateTimeAsMicroseconds,
}

impl DbPartition {
    pub fn new(partition_key: String) -> Self {
        Self {
            partition_key,
            rows: SortedVecOfArcWithStrKey::new(),
            content_size: 0,
            last_read_access: AtomicDateTimeAsMicroseconds::now(),
            expires: AtomicDateTimeAsMicroseconds::new(0),
        }
    }

    pub fn get_expires(&self) -> Option<DateTimeAsMicroseconds> {
        let result = self.expires.as_date_time();

        if result.unix_microseconds == 0 {
            return None;
        }

        Some(result)
    }

    pub fn update_expires(&self, expires: Option<DateTimeAsMicroseconds>) {
        match expires {
            Some(expires) => self.expires.update(expires),
            None => self.expires.update(DateTimeAsMicroseconds::new(0)),
        }
    }

    pub fn get_partition_key(&self) -> &str {
        &self.partition_key
    }

    pub fn rows_count(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn get_content_size(&self) -> usize {
        self.content_size
    }

    pub fn get_last_read_access(&self) -> DateTimeAsMicroseconds {
        self.last_read_access.as_date_time()
    }

    pub fn update_last_read_access(&self, value: DateTimeAsMicroseconds) {
        self.last_read_access.update(value);
    }

    /// Returns the row which was replaced, if there was one.
    pub fn insert_or_replace(&mut self, db_row: Arc<DbRow>) -> Option<Arc<DbRow>> {
        self.content_size += db_row.get_stored_size();

        let (_, removed) = self.rows.insert_or_replace(db_row);

        if let Some(removed) = removed.as_ref() {
            self.content_size -= removed.get_stored_size();
        }

        removed
    }

    /// `false` - a row with this `RowKey` is already there and nothing was written.
    pub fn insert_if_not_exists(&mut self, db_row: Arc<DbRow>) -> bool {
        if self.rows.contains(db_row.get_row_key()) {
            return false;
        }

        self.insert_or_replace(db_row);
        true
    }

    /// `false` - there is no row with this `RowKey`, so there was nothing to replace.
    pub fn replace_if_exists(&mut self, db_row: Arc<DbRow>) -> bool {
        if !self.rows.contains(db_row.get_row_key()) {
            return false;
        }

        self.insert_or_replace(db_row);
        true
    }

    pub fn get_row(&self, row_key: &str) -> Option<&Arc<DbRow>> {
        self.rows.get(row_key)
    }

    pub fn remove_row(&mut self, row_key: &str) -> Option<Arc<DbRow>> {
        let removed = self.rows.remove(row_key);

        if let Some(removed) = removed.as_ref() {
            self.content_size -= removed.get_stored_size();
        }

        removed
    }

    pub fn get_all_rows(&self) -> &[Arc<DbRow>] {
        self.rows.as_slice()
    }
}

impl EntityWithStrKey for DbPartition {
    fn get_key(&self) -> &str {
        &self.partition_key
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db_entity::{ParsedEntity, consts, write_varint};

    fn build_row(partition_key: &str, row_key: &str, payload: &str) -> Arc<DbRow> {
        let mut src = Vec::new();

        for (field_no, value) in [(1u32, partition_key), (2, row_key), (5, payload)] {
            write_varint(
                &mut src,
                u64::from(field_no) << 3 | u64::from(consts::WIRE_TYPE_LEN),
            );
            write_varint(&mut src, value.len() as u64);
            src.extend_from_slice(value.as_bytes());
        }

        let parsed = ParsedEntity::parse(&src).unwrap();
        Arc::new(DbRow::new(parsed, 1, DateTimeAsMicroseconds::new(1)))
    }

    #[test]
    fn content_size_follows_inserts_replaces_and_removes() {
        let mut partition = DbPartition::new("pk".to_string());
        assert_eq!(partition.get_content_size(), 0);

        let first = build_row("pk", "rk-1", "short");
        let first_size = first.get_stored_size();
        partition.insert_or_replace(first);
        assert_eq!(partition.get_content_size(), first_size);

        let bigger = build_row("pk", "rk-1", "a much longer payload");
        let bigger_size = bigger.get_stored_size();
        let replaced = partition.insert_or_replace(bigger);
        assert!(replaced.is_some());
        assert_eq!(partition.rows_count(), 1);
        assert_eq!(partition.get_content_size(), bigger_size);

        partition.remove_row("rk-1");
        assert_eq!(partition.get_content_size(), 0);
        assert!(partition.is_empty());
    }

    /// A row's `Expires` is an atomic a reader pushes forward on every call, so
    /// a size which counted it would be a size the partition subtracts a
    /// different number for when the row leaves. That underflows.
    #[test]
    fn the_accounted_size_does_not_move_when_an_expiry_does() {
        let mut partition = DbPartition::new("pk".to_string());

        let db_row = build_row("pk", "rk", "x");
        partition.insert_or_replace(db_row.clone());
        let before = partition.get_content_size();

        // A far away moment takes more bytes on the wire than a near one.
        db_row.update_expires(Some(DateTimeAsMicroseconds::new(1_800_000_000_000_000)));
        assert_eq!(partition.get_content_size(), before);

        partition.remove_row("rk");
        assert_eq!(partition.get_content_size(), 0);
    }

    #[test]
    fn insert_if_not_exists_keeps_the_stored_row() {
        let mut partition = DbPartition::new("pk".to_string());

        assert!(partition.insert_if_not_exists(build_row("pk", "rk", "first")));
        assert!(!partition.insert_if_not_exists(build_row("pk", "rk", "second")));

        let stored = partition.get_row("rk").unwrap().to_vec();
        assert!(String::from_utf8_lossy(&stored).contains("first"));
    }

    #[test]
    fn replace_if_exists_needs_a_stored_row() {
        let mut partition = DbPartition::new("pk".to_string());

        assert!(!partition.replace_if_exists(build_row("pk", "rk", "nope")));

        partition.insert_or_replace(build_row("pk", "rk", "first"));
        assert!(partition.replace_if_exists(build_row("pk", "rk", "second")));
        assert_eq!(partition.rows_count(), 1);
    }

    #[test]
    fn rows_are_kept_sorted_by_row_key() {
        let mut partition = DbPartition::new("pk".to_string());
        partition.insert_or_replace(build_row("pk", "c", "x"));
        partition.insert_or_replace(build_row("pk", "a", "x"));
        partition.insert_or_replace(build_row("pk", "b", "x"));

        let keys: Vec<&str> = partition
            .get_all_rows()
            .iter()
            .map(|itm| itm.get_row_key())
            .collect();

        assert_eq!(keys, vec!["a", "b", "c"]);
    }
}
