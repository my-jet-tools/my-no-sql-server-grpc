use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ahash::{AHashMap, AHashSet};
use arc_swap::ArcSwap;
use my_no_sql_grpc_core::db::{DbRow, PartitionRowKeys, TransactionAction};
use my_no_sql_grpc_core::db::{DbTable, DbTableAttributes, GetRowsFilter};
use my_no_sql_grpc_core::db_entity::ParsedEntity;
use my_no_sql_grpc_core::rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::MyNoSqlReaderError;

/// The in-process image of one table.
///
/// It is a `DbTable` - the very structure the server keeps - because that is
/// exactly what a reader's cache is: partitions of rows sorted by key. Reusing
/// it means the two sides of the contract can not disagree about what "the
/// partition now holds these rows" does.
pub struct TableCache {
    pub table_name: String,
    data: DbTable,
    /// `false` until the first snapshot has been applied. A read before that
    /// would answer "no rows" and look exactly like an empty table.
    initialized: AtomicBool,
    /// The wake up for whoever is waiting for that first snapshot, and a
    /// `watch` rather than a `Notify` because a watch remembers the version a
    /// waiter started from: the snapshot lands once per session, so a wake up
    /// which happens while somebody is only starting to wait has to still
    /// count. The flag beside it is not a second truth - it is the same one,
    /// kept where reading it costs no lock at all.
    initialized_watch: tokio::sync::watch::Sender<bool>,
    /// What to tell the server about what was read, waiting for the next
    /// `GetChange` to carry it. It rides along with a call the reader was going
    /// to make anyway, so saying "I am still using this" costs no round trip.
    ///
    /// Keyed by partition, and that is what bounds it: nothing is taken out of
    /// here until a call succeeds, while the application goes on reading at its
    /// own rate. One entry per `report_read` would grow with that rate for the
    /// whole length of an outage; coalesced, it grows with the number of
    /// distinct partitions instead. The JSON version does the same.
    pending_statistics: parking_lot::Mutex<AHashMap<String, PendingReadStatistics>>,
    /// What the server last said the table does with itself. Read far more
    /// often than it changes, so it sits behind an `ArcSwap`.
    attributes: ArcSwap<DbTableAttributes>,
}

/// What a reader says about one partition it looked at.
///
/// The last-read marks feed the eviction of what nobody reads. `expires` is the
/// other half of it - sliding expiration, where the reading itself is what keeps
/// a row alive. Both are opt-in: an untouched field says "leave it alone", which
/// is not the same as "set it to never".
#[derive(Clone)]
pub struct ReadStatistics {
    pub partition_key: String,
    pub row_keys: Vec<String>,
    pub update_partition_last_read: bool,
    pub update_rows_last_read: bool,
    pub set_partition_expires: Option<Option<DateTimeAsMicroseconds>>,
    pub set_rows_expires: Option<Option<DateTimeAsMicroseconds>>,
}

impl ReadStatistics {
    /// Moves the last-read marks of a partition and of the rows named, which is
    /// what keeps them from being evicted as unread.
    pub fn read(partition_key: &str, row_keys: Vec<String>) -> Self {
        Self {
            partition_key: partition_key.to_string(),
            row_keys,
            update_partition_last_read: true,
            update_rows_last_read: true,
            set_partition_expires: None,
            set_rows_expires: None,
        }
    }

    /// Also pushes the rows' expiry to a new moment - `None` meaning never.
    pub fn keeping_rows_alive(mut self, expires: Option<DateTimeAsMicroseconds>) -> Self {
        self.set_rows_expires = Some(expires);
        self
    }

    /// The same for the partition as a whole.
    pub fn keeping_the_partition_alive(mut self, expires: Option<DateTimeAsMicroseconds>) -> Self {
        self.set_partition_expires = Some(expires);
        self
    }
}

/// Everything said about one partition since the last `GetChange`, merged into
/// the single instruction they add up to.
#[derive(Default)]
struct PendingReadStatistics {
    /// A set: an application which keeps reading the same rows names them again
    /// on every read, and the server does the same thing for one mention of a
    /// key as for a thousand.
    row_keys: AHashSet<String>,
    update_partition_last_read: bool,
    update_rows_last_read: bool,
    set_partition_expires: Option<Option<DateTimeAsMicroseconds>>,
    set_rows_expires: Option<Option<DateTimeAsMicroseconds>>,
}

impl PendingReadStatistics {
    fn merge(&mut self, src: ReadStatistics) {
        self.row_keys.extend(src.row_keys);
        self.update_partition_last_read |= src.update_partition_last_read;
        self.update_rows_last_read |= src.update_rows_last_read;

        // The last expiry said is the one which travels - a sliding expiration
        // is about the newest moment, not the first. An untouched field says
        // "leave it alone", though, so it must not undo an instruction which is
        // already waiting to go.
        if src.set_partition_expires.is_some() {
            self.set_partition_expires = src.set_partition_expires;
        }

        if src.set_rows_expires.is_some() {
            self.set_rows_expires = src.set_rows_expires;
        }
    }

    fn into_statistics(self, partition_key: String) -> ReadStatistics {
        let mut row_keys: Vec<String> = self.row_keys.into_iter().collect();
        // A hash set hands its members over in a different order every time,
        // and two calls which said the same thing should look the same.
        row_keys.sort_unstable();

        ReadStatistics {
            partition_key,
            row_keys,
            update_partition_last_read: self.update_partition_last_read,
            update_rows_last_read: self.update_rows_last_read,
            set_partition_expires: self.set_partition_expires,
            set_rows_expires: self.set_rows_expires,
        }
    }
}

impl TableCache {
    pub fn new(table_name: String) -> Self {
        Self {
            data: DbTable::new(table_name.clone(), DbTableAttributes::create_default()),
            table_name,
            initialized: AtomicBool::new(false),
            initialized_watch: tokio::sync::watch::Sender::new(false),
            pending_statistics: parking_lot::Mutex::new(AHashMap::new()),
            attributes: ArcSwap::from_pointee(DbTableAttributes::create_default()),
        }
    }

    pub fn push_statistics(&self, statistics: ReadStatistics) {
        let mut pending = self.pending_statistics.lock();

        if let Some(existing) = pending.get_mut(&statistics.partition_key) {
            existing.merge(statistics);
            return;
        }

        let partition_key = statistics.partition_key.clone();
        pending.entry(partition_key).or_default().merge(statistics);
    }

    /// Handed to the next `GetChange` and forgotten. Losing a batch of these
    /// with a failed call costs nothing: they are what the reader read, not what
    /// it holds, and the next read says it again.
    pub fn take_statistics(&self) -> Vec<ReadStatistics> {
        let pending = std::mem::take(&mut *self.pending_statistics.lock());

        let mut result: Vec<ReadStatistics> = pending
            .into_iter()
            .map(|(partition_key, itm)| itm.into_statistics(partition_key))
            .collect();

        // Same reason the row keys are sorted: what leaves here should not
        // depend on the order a hash map happens to hold its entries in.
        result.sort_unstable_by(|left, right| left.partition_key.cmp(&right.partition_key));

        result
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Relaxed)
    }

    pub async fn wait_until_initialized(&self) {
        let mut receiver = self.subscribe_initialized();

        while !self.is_initialized() {
            // Only ever an error when the sender is gone, and the sender is a
            // field of the very cache this is borrowing.
            receiver
                .changed()
                .await
                .expect("the sender lives in the cache");
        }
    }

    /// What the wait does before it reads the flag, and the order is the whole
    /// point: a snapshot which lands between the two bumps a version this
    /// receiver has not seen, so the wait ends at once instead of sleeping
    /// through the one announcement there is going to be.
    ///
    /// Split out of the wait so a test can hold it and let the snapshot land in
    /// the middle.
    fn subscribe_initialized(&self) -> tokio::sync::watch::Receiver<bool> {
        self.initialized_watch.subscribe()
    }

    fn set_initialized(&self) {
        self.initialized.store(true, Ordering::Relaxed);
        // `send_replace` rather than `send`: nobody may be waiting, and an
        // announcement with no audience is not a failure here.
        self.initialized_watch.send_replace(true);
    }

    // ---- what the session applies ----------------------------------------

    /// The whole image of the table, as `Subscribe` streamed it back. It
    /// replaces everything: a snapshot is the truth, not an update of it.
    pub fn apply_snapshot(&self, rows: Vec<Arc<DbRow>>) {
        self.data.bulk_write(
            my_no_sql_grpc_core::db::BulkWriteMode::CleanTableAndInsert,
            rows,
        );

        self.set_initialized();
    }

    pub fn get_attributes(&self) -> Arc<DbTableAttributes> {
        self.attributes.load_full()
    }

    pub fn set_attributes(&self, attributes: DbTableAttributes) {
        self.attributes.store(Arc::new(attributes));
    }

    pub fn clean(&self) {
        self.data.clean();
    }

    pub fn delete_partitions(&self, partition_keys: &[String]) {
        self.data.delete_partitions(partition_keys);
    }

    /// A finished `UpdateRows` batch.
    pub fn apply_updated_rows(&self, rows: Vec<Arc<DbRow>>) {
        self.data.bulk_write(
            my_no_sql_grpc_core::db::BulkWriteMode::InsertOrReplace,
            rows,
        );
    }

    /// A finished `InitPartitions` batch: the partitions it names end up holding
    /// exactly these rows, and the ones it does not name are untouched.
    pub fn apply_init_partitions(&self, rows: Vec<Arc<DbRow>>) {
        self.data.bulk_write(
            my_no_sql_grpc_core::db::BulkWriteMode::CleanPartitionsAndInsert,
            rows,
        );
    }

    /// A finished `DeleteRows` batch, applied in one visit to the table.
    pub fn apply_deleted_rows(&self, partitions: Vec<PartitionRowKeys>) {
        self.data.apply_transaction(
            partitions
                .into_iter()
                .map(TransactionAction::DeleteRows)
                .collect(),
        );
    }

    // ---- what the application reads --------------------------------------

    pub fn get_row(&self, partition_key: &str, row_key: &str) -> Option<Arc<DbRow>> {
        self.data.get_row(partition_key, row_key)
    }

    pub fn get_by_partition_key(&self, partition_key: &str) -> Vec<Arc<DbRow>> {
        self.data.get_rows(&GetRowsFilter {
            partition_key: Some(partition_key),
            row_key: None,
            skip: None,
            limit: None,
        })
    }

    pub fn get_all(&self) -> Vec<Arc<DbRow>> {
        self.data.get_rows(&GetRowsFilter::all())
    }

    pub fn get_partition_keys(&self) -> Vec<String> {
        self.data.get_partition_keys()
    }

    pub fn get_rows_amount(&self) -> usize {
        self.data.get_rows_amount()
    }
}

/// Turns what arrived on the wire into a row of the cache.
///
/// `schema_id` is `0` on this side and stays unused: the schema is what *shows*
/// a row, and an application which is reading its own entity already knows how
/// to decode it.
pub fn to_db_row(row: &[u8]) -> Result<Arc<DbRow>, MyNoSqlReaderError> {
    let parsed = ParsedEntity::parse(row)
        .map_err(|err| MyNoSqlReaderError::CanNotParseRow(err.to_string()))?;

    // A row always leaves the server in its emit form, so it carries a
    // TimeStamp. Falling back to now for one which somehow does not keeps the
    // row rather than dropping it.
    let time_stamp = match parsed.time_stamp {
        Some(time_stamp) => DateTimeAsMicroseconds::new(time_stamp),
        None => DateTimeAsMicroseconds::now(),
    };

    Ok(Arc::new(DbRow::new(parsed, 0, time_stamp)))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn reading_the_same_partition_over_and_over_leaves_one_thing_to_report() {
        let cache = TableCache::new("traders".to_string());

        for _ in 0..1000 {
            cache.push_statistics(ReadStatistics::read("acc-1", vec!["a".to_string()]));
        }

        cache.push_statistics(ReadStatistics::read("acc-1", vec!["b".to_string()]));
        cache.push_statistics(ReadStatistics::read("acc-2", vec!["a".to_string()]));

        let taken = cache.take_statistics();

        // A thousand reads of one partition are one instruction, not a thousand.
        assert_eq!(taken.len(), 2);

        assert_eq!(taken[0].partition_key, "acc-1");
        assert_eq!(taken[0].row_keys, vec!["a".to_string(), "b".to_string()]);
        assert!(taken[0].update_partition_last_read);
        assert!(taken[0].update_rows_last_read);

        assert_eq!(taken[1].partition_key, "acc-2");

        // And taking them is what empties the queue.
        assert!(cache.take_statistics().is_empty());
    }

    #[test]
    fn the_last_expiry_said_is_the_one_which_travels() {
        let cache = TableCache::new("traders".to_string());

        cache.push_statistics(
            ReadStatistics::read("acc-1", vec!["a".to_string()])
                .keeping_rows_alive(Some(DateTimeAsMicroseconds::new(1))),
        );

        // A plain read says nothing about the expiry, so it must not undo the
        // instruction which is already waiting to go.
        cache.push_statistics(ReadStatistics::read("acc-1", vec!["a".to_string()]));

        cache.push_statistics(
            ReadStatistics::read("acc-1", vec!["a".to_string()])
                .keeping_rows_alive(Some(DateTimeAsMicroseconds::new(2)))
                .keeping_the_partition_alive(None),
        );

        let taken = cache.take_statistics();

        assert_eq!(taken.len(), 1);
        assert_eq!(
            taken[0]
                .set_rows_expires
                .unwrap()
                .unwrap()
                .unix_microseconds,
            2
        );
        // "Never" is a value like any other, and it survives the merge as one.
        assert!(taken[0].set_partition_expires.unwrap().is_none());
    }

    /// The snapshot lands once per session, so a waiter which starts looking
    /// a moment before it must not sleep through it. This is that moment,
    /// stopped: the receiver the wait takes is held here, the snapshot lands
    /// while it is held, and the wait still ends.
    #[tokio::test]
    async fn a_snapshot_which_lands_while_somebody_starts_waiting_still_wakes_them() {
        let cache = TableCache::new("traders".to_string());

        // The first thing `wait_until_initialized` does, done here instead.
        let mut receiver = cache.subscribe_initialized();
        assert!(!cache.is_initialized());

        // And the whole session arrives in what would otherwise be the gap
        // between that and the flag being read.
        cache.apply_snapshot(Vec::new());

        tokio::time::timeout(Duration::from_secs(5), receiver.changed())
            .await
            .expect("the wait slept through the snapshot")
            .unwrap();

        assert!(cache.is_initialized());

        // And the wait as a whole ends, rather than going round again.
        tokio::time::timeout(Duration::from_secs(5), cache.wait_until_initialized())
            .await
            .unwrap();
    }
}
