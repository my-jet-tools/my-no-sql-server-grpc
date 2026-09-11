use std::sync::Arc;

use ahash::AHashMap;
use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use rust_extensions::date_time::{AtomicDateTimeAsMicroseconds, DateTimeAsMicroseconds};
use rust_extensions::sorted_vec::EntityWithStrKey;

use crate::schemas::EntitySchema;

use super::db_table_inner::DbTableInner;
use super::{
    BulkWriteMode, BulkWriteResult, DbPartition, DbRow, DbTableAttributes, GcResult,
    GetRowStatisticsResult, TransactionAction, TransactionResult, UpdateReadStatistics,
};

/// What [`DbTable::get_rows`] selects. Both keys absent means the whole table.
pub struct GetRowsFilter<'s> {
    pub partition_key: Option<&'s str>,
    pub row_key: Option<&'s str>,
    pub skip: Option<usize>,
    pub limit: Option<usize>,
}

impl<'s> GetRowsFilter<'s> {
    pub fn all() -> Self {
        Self {
            partition_key: None,
            row_key: None,
            skip: None,
            limit: None,
        }
    }
}

/// The three numbers a table is asked its size by, read together.
///
/// They come out of one acquisition of the read lock on purpose: taken one at a
/// time they are three locks and a torn view, in which the rows can belong to a
/// different moment than the partitions holding them.
pub struct DbTableMetrics {
    pub partitions_amount: usize,
    pub rows_amount: usize,
    pub content_size: usize,
}

/// What [`DbTable::replace_if_version_matches`] ran into. The two refusals are
/// told apart because they are different answers to the caller: one says the row
/// was never there, the other that somebody else has rewritten it since it was
/// read - and only the second one is worth retrying.
pub enum ReplaceIfVersionMatchesResult {
    Replaced,
    RowNotFound,
    VersionMismatch,
}

/// A table. Attributes are read on every write and changed almost never, so they
/// sit behind an `ArcSwap`; the data itself is a single `RwLock` over the whole
/// inner state, so a write never leaves the partitions and their rows out of
/// step with each other.
///
/// The lock is a `parking_lot` one on purpose: nothing under it ever awaits -
/// callers collect what they need and do their I/O after the guard is gone.
pub struct DbTable {
    pub name: String,
    attributes: ArcSwap<DbTableAttributes>,
    /// Held by everything that *changes* the attributes, and by nothing that
    /// reads them. An `ArcSwap` gives no read-modify-write, and the schemas
    /// inside the attributes are written by concurrent writers - two of them
    /// registering two new schemas at once would otherwise keep whichever
    /// stored last.
    attributes_write_lock: Mutex<()>,
    inner: RwLock<DbTableInner>,
    /// When the table last changed, `0` meaning "not since this process
    /// started". An atomic outside the lock, stored after the guard is dropped:
    /// a moment is not worth a longer write lock, and it is one store per entry
    /// into the table rather than one per row.
    last_write_moment: AtomicDateTimeAsMicroseconds,
    /// When [`Self::gc_schemas`] last walked the rows of this table.
    last_schemas_gc_moment: AtomicDateTimeAsMicroseconds,
}

impl DbTable {
    pub fn new(name: String, attributes: DbTableAttributes) -> Self {
        Self {
            name,
            attributes: ArcSwap::from_pointee(attributes),
            attributes_write_lock: Mutex::new(()),
            inner: RwLock::new(DbTableInner::new()),
            last_write_moment: AtomicDateTimeAsMicroseconds::new(0),
            last_schemas_gc_moment: AtomicDateTimeAsMicroseconds::new(0),
        }
    }

    /// Everything one look at the size of a table answers, under one read lock.
    pub fn get_metrics(&self) -> DbTableMetrics {
        let inner = self.inner.read();

        DbTableMetrics {
            partitions_amount: inner.get_partitions_amount(),
            rows_amount: inner.get_rows_amount(),
            content_size: inner.get_content_size(),
        }
    }

    /// `None` - nothing has written to this table since the process started.
    /// The moment is not persisted: what is on disk is the data, and a moment
    /// restored from a file would say when the table was written last time the
    /// server ran, which is a different thing that nobody asked for.
    pub fn get_last_write_moment(&self) -> Option<DateTimeAsMicroseconds> {
        let result = self.last_write_moment.as_date_time();

        if result.unix_microseconds == 0 {
            return None;
        }

        Some(result)
    }

    /// Marked by whatever actually changed something - a call which found
    /// nothing to do did not write, and saying it did would make the number
    /// useless for the one question it answers.
    fn written(&self) {
        self.last_write_moment.update(DateTimeAsMicroseconds::now());
    }

    pub fn get_attributes(&self) -> Arc<DbTableAttributes> {
        self.attributes.load_full()
    }

    /// Replaces what a caller is allowed to say about the table and **merges**
    /// what only the table itself knows.
    ///
    /// The schemas are merged rather than taken because they are not something a
    /// caller asks for: they are what the stored rows were written under, and a
    /// `PUT /api/Tables/Attributes` - which replaces rather than patches - would
    /// otherwise leave every row of the table unshowable, because a request
    /// naming a limit carries no schemas at all. Merging is also what lets a
    /// restore hand back the schemas its archive carried in the same call, and
    /// the first writer of an id still wins, exactly as on the write path.
    pub fn set_attributes(&self, attributes: DbTableAttributes) {
        let _guard = self.attributes_write_lock.lock();

        let mut attributes = attributes;
        attributes.schemas = merged(&self.attributes.load().schemas, &attributes.schemas);

        self.attributes.store(Arc::new(attributes));
    }

    /// The schema a stored row names, or nothing when this table has never been
    /// written under it.
    pub fn get_schema(&self, schema_id: u64) -> Option<Arc<EntitySchema>> {
        self.attributes.load().schemas.get(&schema_id).cloned()
    }

    /// `false` - the id was already there and the table was left alone, which is
    /// the normal case: a schema travels with every single entity. That the
    /// schema behind a known id is the same one is checked before this is
    /// called, by comparing the bytes; here the first writer simply wins.
    ///
    /// `true` means the table's metadata no longer says what the table holds,
    /// so the caller owes it a write.
    pub fn register_schema(&self, schema: EntitySchema) -> bool {
        if self.attributes.load().schemas.contains_key(&schema.id) {
            return false;
        }

        let _guard = self.attributes_write_lock.lock();

        let current = self.attributes.load_full();

        if current.schemas.contains_key(&schema.id) {
            return false;
        }

        let mut schemas = current.schemas.as_ref().clone();
        schemas.insert(schema.id, Arc::new(schema));

        let mut attributes = current.as_ref().clone();
        attributes.schemas = Arc::new(schemas);

        self.attributes.store(Arc::new(attributes));

        true
    }

    /// Drops the schemas no row of this table names any more, and says whether
    /// it took any - the caller owes `tables.meta` a write when it did.
    ///
    /// Without this the registry only grows: a table recreated a hundred times
    /// under a changing entity would carry a hundred dead schemas, loaded at
    /// every start, for as long as the table exists.
    ///
    /// Four rules, and each of them is here for its own reason:
    ///
    /// - **one schema or fewer costs one load.** That is what almost every table
    ///   in existence holds, and the pass has to be free for it, the same way
    ///   the row collector's cheap look is;
    /// - **the rows are walked under a read lock.** Proving a schema unused
    ///   means seeing every row, so there is no early exit by nature - but the
    ///   walk only reads, and dropping what it found is an attribute swap;
    /// - **a table nobody wrote to is not walked again.** Its rows are the rows
    ///   the last walk saw, so the answer is the answer it got; without this a
    ///   genuinely mixed table pays a full scan every thirty seconds forever;
    /// - **the last schema is never dropped, even with no rows behind it.** HTTP
    ///   is the UI's surface, and a UI showing an emptied table still needs the
    ///   shape to draw its columns.
    pub fn gc_schemas(&self) -> bool {
        let attributes = self.get_attributes();

        if attributes.schemas.len() <= 1 {
            return false;
        }

        let started = DateTimeAsMicroseconds::now();
        let last_write = self.last_write_moment.as_date_time().unix_microseconds;
        let last_check = self.last_schemas_gc_moment.as_date_time().unix_microseconds;

        // `0` is "never looked", which is what a table restored from disk starts
        // at - and that one has to be looked at once even if nothing writes to
        // it again.
        if last_check != 0 && last_write <= last_check {
            return false;
        }

        let live = self.inner.read().collect_schema_ids();

        // Stamped with the moment the walk *started*, so a write which landed
        // while it ran still reads as newer and brings the next pass back.
        self.last_schemas_gc_moment.update(started);

        let mut dead: Vec<u64> = attributes
            .schemas
            .keys()
            .copied()
            .filter(|schema_id| !live.contains(schema_id))
            .collect();

        // Sorted so that which one survives an empty table is the same on two
        // servers holding the same data, rather than whichever the hash map
        // happened to hand out first.
        dead.sort_unstable();

        if dead.len() == attributes.schemas.len() {
            // Nothing is live, so the highest id stays: a table with no rows
            // still has to be drawable.
            dead.pop();
        }

        if dead.is_empty() {
            return false;
        }

        let _guard = self.attributes_write_lock.lock();

        // A write which landed while the walk ran may have written a row under
        // one of the ids about to go. Its schema was still in the map, so
        // `register_schema` told it there was nothing to do - and dropping it
        // now would leave that row with no shape. The pass is abandoned instead;
        // the stamp above is what brings the next one back to it.
        if self.last_write_moment.as_date_time().unix_microseconds != last_write {
            return false;
        }

        let current = self.attributes.load_full();

        let mut schemas: AHashMap<u64, Arc<EntitySchema>> = current.schemas.as_ref().clone();

        for schema_id in dead {
            schemas.remove(&schema_id);
        }

        let mut attributes = current.as_ref().clone();
        attributes.schemas = Arc::new(schemas);

        self.attributes.store(Arc::new(attributes));

        true
    }

    pub fn insert_or_replace(&self, db_row: Arc<DbRow>) -> Option<Arc<DbRow>> {
        let replaced = self.inner.write().insert_or_replace(db_row);
        self.written();
        replaced
    }

    /// `false` - the row is already there and nothing was written.
    pub fn insert_if_not_exists(&self, db_row: Arc<DbRow>) -> bool {
        let inserted = self.inner.write().insert_if_not_exists(db_row);

        if inserted {
            self.written();
        }

        inserted
    }

    /// `false` - there is no such row, so there was nothing to replace.
    pub fn replace_if_exists(&self, db_row: Arc<DbRow>) -> bool {
        let replaced = self.inner.write().replace_if_exists(db_row);

        if replaced {
            self.written();
        }

        replaced
    }

    /// Overwrites the stored row, but only while it is still the version the
    /// caller read.
    ///
    /// The version compared is `expected_time_stamp` - the moment the caller
    /// read the row at - and never the TimeStamp of the row being written: that
    /// one belongs to this write and would match nothing. Both the comparison
    /// and the overwrite happen under one acquisition of the write lock, which
    /// is the only reason the check lives here instead of in the caller: two
    /// entries into the table would let the writer this check exists to catch
    /// land in between.
    pub fn replace_if_version_matches(
        &self,
        db_row: Arc<DbRow>,
        expected_time_stamp: DateTimeAsMicroseconds,
    ) -> ReplaceIfVersionMatchesResult {
        let mut inner = self.inner.write();

        let Some(stored) = inner.get_row(db_row.get_partition_key(), db_row.get_row_key()) else {
            return ReplaceIfVersionMatchesResult::RowNotFound;
        };

        if stored.get_time_stamp().unix_microseconds != expected_time_stamp.unix_microseconds {
            return ReplaceIfVersionMatchesResult::VersionMismatch;
        }

        inner.replace_if_exists(db_row);
        drop(inner);

        self.written();

        ReplaceIfVersionMatchesResult::Replaced
    }

    /// Applies a whole batch as one entry into the table.
    ///
    /// The stream a batch arrives on is transport and nothing else: the caller
    /// collects every row first and hands them over here, so the table is locked
    /// once, for as long as the batch takes to apply, and never for as long as
    /// the client takes to send it.
    ///
    /// One entry is also what lets the subscribers be told about the batch with
    /// one event - and, together with [`Self::register_and_snapshot`], what
    /// keeps a subscriber from seeing a table half way through a batch.
    pub fn bulk_write(&self, mode: BulkWriteMode, rows: Vec<Arc<DbRow>>) -> BulkWriteResult {
        let result = self.inner.write().bulk_write(mode, rows);

        // Marked only when something actually changed, like every other write
        // here. An `InsertOrReplaceIfNew` batch which lost every comparison
        // wrote nothing, and moving the moment for it would show a stalled feed
        // as a live table to the operator reading `/api/Status` to find out
        // exactly that. The partitions to persist are what says it: they hold
        // both the rows written and the partitions a clean emptied.
        if !result.partitions_to_persist.is_empty() {
            self.written();
        }

        result
    }

    /// Returns the keys of the partitions which were actually there.
    pub fn delete_partitions(&self, partition_keys: &[String]) -> Vec<String> {
        let deleted = self.inner.write().delete_partitions(partition_keys);

        if !deleted.is_empty() {
            self.written();
        }

        deleted
    }

    /// One garbage collection pass: rows whose `Expires` has come and gone, and
    /// whatever the table's own limits say does not fit any more.
    ///
    /// The cheap look comes first, under a **read** lock, because on almost
    /// every tick there is nothing to do - and a read lock lets the readers of a
    /// table which is not expiring anything carry on. Only when it finds
    /// something is the whole thing worked out again under the write lock, so
    /// what is deleted is decided and applied without a writer in between.
    pub fn gc(&self, now: DateTimeAsMicroseconds) -> GcResult {
        let attributes = self.get_attributes();

        if !self
            .inner
            .read()
            .has_anything_to_gc(now.unix_microseconds, &attributes)
        {
            return GcResult::default();
        }

        let result = self.inner.write().gc(now.unix_microseconds, &attributes);

        if !result.is_empty() {
            self.written();
        }

        result
    }

    /// Applies the partition limit right now, with a number of its own instead
    /// of the table's.
    pub fn keep_max_partitions_amount(&self, max: usize) -> GcResult {
        let result = self.inner.write().keep_max_partitions_amount(max);

        if !result.is_empty() {
            self.written();
        }

        result
    }

    /// The same for the rows of one partition.
    pub fn keep_max_rows_in_partition(&self, partition_key: &str, max: usize) -> GcResult {
        let result = self
            .inner
            .write()
            .keep_max_rows_in_partition(partition_key, max);

        if !result.is_empty() {
            self.written();
        }

        result
    }

    /// Applies a transaction: writes of several kinds, in the order the client
    /// built them, under one acquisition of the write lock.
    ///
    /// This is the whole of what a transaction buys. Collecting the actions is
    /// the client's business and the stream's; what makes them a transaction is
    /// that the table is entered once, so no reader can take a snapshot between
    /// the delete and the insert, and the subscribers are told about all of it
    /// in one contiguous run.
    pub fn apply_transaction(&self, actions: Vec<TransactionAction>) -> TransactionResult {
        let result = self.inner.write().apply_transaction(actions);

        if !result.changes.is_empty() {
            self.written();
        }

        result
    }

    pub fn get_row(&self, partition_key: &str, row_key: &str) -> Option<Arc<DbRow>> {
        self.inner.read().get_row(partition_key, row_key)
    }

    /// What the table knows about one row and the partition holding it.
    ///
    /// Reading the statistics is **not** a read of the row: nothing here moves a
    /// last-read mark. An endpoint which did would always answer "just now", and
    /// it would rescue from eviction exactly the cold rows somebody went looking
    /// for.
    ///
    /// The numbers are copied out under the lock and the guard is gone before
    /// anything is formatted - a partition lives by value inside the sorted
    /// vector and can not outlive it.
    pub fn get_row_statistics(&self, partition_key: &str, row_key: &str) -> GetRowStatisticsResult {
        self.inner.read().get_row_statistics(partition_key, row_key)
    }

    pub fn delete_row(&self, partition_key: &str, row_key: &str) -> Option<Arc<DbRow>> {
        let deleted = self.inner.write().delete_row(partition_key, row_key);

        if deleted.is_some() {
            self.written();
        }

        deleted
    }

    pub fn get_rows(&self, filter: &GetRowsFilter) -> Vec<Arc<DbRow>> {
        self.inner.read().get_rows(filter)
    }

    /// Rows of one partition whose key is at or below `row_key`, the highest
    /// one first.
    pub fn get_highest_row_and_below(
        &self,
        partition_key: &str,
        row_key: &str,
        limit: Option<usize>,
    ) -> Vec<Arc<DbRow>> {
        self.inner
            .read()
            .get_highest_row_and_below(partition_key, row_key, limit)
    }

    /// The rows named, in the order they were named.
    pub fn get_single_partition_multiple_rows(
        &self,
        partition_key: &str,
        row_keys: &[String],
    ) -> Vec<Arc<DbRow>> {
        self.inner
            .read()
            .get_single_partition_multiple_rows(partition_key, row_keys)
    }

    /// Runs `register` and takes a snapshot under one acquisition of the write
    /// lock. This is what keeps a subscriber from missing a write made while its
    /// snapshot was still being streamed.
    ///
    /// The ordering argument: a writer notifies its subscribers only *after* its
    /// own write has committed under this same lock. So if a write lands after
    /// the snapshot was taken, it also lands after `register` ran - and the
    /// notification therefore finds this subscriber. A write that landed before
    /// the snapshot is in the snapshot. The overlap (a write that is both in the
    /// snapshot and notified afterwards) is harmless: applying a row twice
    /// assigns the same state.
    ///
    /// `register` runs with the table locked, so it must not block or await -
    /// pushing into a subscriber list is all it is meant to do.
    pub fn register_and_snapshot(&self, register: impl FnOnce()) -> Vec<Arc<DbRow>> {
        let inner = self.inner.write();
        register();
        inner.get_rows(&GetRowsFilter::all())
    }

    /// What a reader tells the server it actually read.
    ///
    /// It moves the last-read marks, which is what keeps eviction from throwing
    /// out a partition everybody reads but nobody rewrites, and it pushes the
    /// expiry forward - a reader which keeps asking for a row is what keeps the
    /// row alive.
    pub fn apply_read_statistics(
        &self,
        partition_key: &str,
        row_keys: &[String],
        statistics: &UpdateReadStatistics,
        now: DateTimeAsMicroseconds,
    ) {
        self.inner
            .read()
            .apply_read_statistics(partition_key, row_keys, statistics, now);
    }

    pub fn get_partitions_amount(&self) -> usize {
        self.inner.read().get_partitions_amount()
    }

    pub fn get_rows_amount(&self) -> usize {
        self.inner.read().get_rows_amount()
    }

    pub fn get_content_size(&self) -> usize {
        self.inner.read().get_content_size()
    }

    pub fn get_partition_keys(&self) -> Vec<String> {
        self.inner.read().get_partition_keys()
    }

    /// Puts a partition loaded from disk into the table as it is.
    ///
    /// Deliberately not a write: this is the table being brought back to what it
    /// already was, and stamping it would make every table on a restarted server
    /// report that it changed at start up.
    pub fn restore_partition(&self, partition: DbPartition) {
        self.inner.write().restore_partition(partition);
    }

    /// Empties the table and returns the keys of the partitions it held, so
    /// their slots on disk can be freed.
    pub fn clean(&self) -> Vec<String> {
        let cleaned = self.inner.write().clean();
        self.written();
        cleaned
    }
}

/// The schemas the table already has, plus the ones it does not. The stored one
/// wins on a collision, and a merge which adds nothing hands the very same `Arc`
/// back - which is what every ordinary `set_attributes` call is.
fn merged(
    stored: &Arc<AHashMap<u64, Arc<EntitySchema>>>,
    incoming: &Arc<AHashMap<u64, Arc<EntitySchema>>>,
) -> Arc<AHashMap<u64, Arc<EntitySchema>>> {
    if incoming
        .keys()
        .all(|schema_id| stored.contains_key(schema_id))
    {
        return stored.clone();
    }

    let mut result = stored.as_ref().clone();

    for (schema_id, schema) in incoming.iter() {
        result.entry(*schema_id).or_insert_with(|| schema.clone());
    }

    Arc::new(result)
}

impl EntityWithStrKey for DbTable {
    fn get_key(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{AppliedChange, PartitionRowKeys, UpdateReadStatistics};
    use crate::db_entity::{ParsedEntity, consts, write_varint};

    fn build_row(partition_key: &str, row_key: &str, time_stamp: i64) -> Arc<DbRow> {
        build_row_of_schema(partition_key, row_key, time_stamp, 1)
    }

    fn build_row_of_schema(
        partition_key: &str,
        row_key: &str,
        time_stamp: i64,
        schema_id: u64,
    ) -> Arc<DbRow> {
        let mut src = Vec::new();

        for (field_no, value) in [(1u32, partition_key), (2, row_key)] {
            write_varint(
                &mut src,
                u64::from(field_no) << 3 | u64::from(consts::WIRE_TYPE_LEN),
            );
            write_varint(&mut src, value.len() as u64);
            src.extend_from_slice(value.as_bytes());
        }

        let parsed = ParsedEntity::parse(&src).unwrap();
        Arc::new(DbRow::new(
            parsed,
            schema_id,
            DateTimeAsMicroseconds::new(time_stamp),
        ))
    }

    /// Everything the table holds, as `partition/row` pairs - the shortest way to
    /// say what a batch left behind.
    fn content(db_table: &DbTable) -> Vec<String> {
        db_table
            .get_rows(&GetRowsFilter::all())
            .iter()
            .map(|db_row| format!("{}/{}", db_row.get_partition_key(), db_row.get_row_key()))
            .collect()
    }

    fn written(result: &BulkWriteResult) -> Vec<String> {
        result
            .written
            .iter()
            .map(|db_row| format!("{}/{}", db_row.get_partition_key(), db_row.get_row_key()))
            .collect()
    }

    fn table_with(rows: Vec<Arc<DbRow>>) -> DbTable {
        let db_table = DbTable::new("t".to_string(), DbTableAttributes::create_default());
        db_table.bulk_write(BulkWriteMode::InsertOrReplace, rows);
        db_table
    }

    #[test]
    fn insert_or_replace_leaves_the_partitions_the_batch_did_not_name_alone() {
        let db_table = table_with(vec![build_row("a", "1", 1), build_row("b", "1", 1)]);

        let result = db_table.bulk_write(
            BulkWriteMode::InsertOrReplace,
            vec![build_row("a", "2", 2), build_row("c", "1", 2)],
        );

        assert_eq!(content(&db_table), vec!["a/1", "a/2", "b/1", "c/1"]);
        assert_eq!(written(&result), vec!["a/2", "c/1"]);
        assert_eq!(result.partitions_to_persist, vec!["a", "c"]);
    }

    /// The point of the mode: the named partitions end up holding the batch and
    /// nothing else, while the ones it never mentions are not even looked at.
    #[test]
    fn clean_partitions_and_insert_replaces_the_content_of_the_named_partitions() {
        let db_table = table_with(vec![build_row("a", "old", 1), build_row("b", "keep", 1)]);

        let result = db_table.bulk_write(
            BulkWriteMode::CleanPartitionsAndInsert,
            vec![build_row("a", "new", 2)],
        );

        assert_eq!(content(&db_table), vec!["a/new", "b/keep"]);
        assert_eq!(result.partitions_to_persist, vec!["a"]);
    }

    #[test]
    fn clean_table_and_insert_leaves_only_the_batch_and_owns_up_to_what_it_dropped() {
        let db_table = table_with(vec![build_row("a", "old", 1), build_row("b", "old", 1)]);

        let result = db_table.bulk_write(
            BulkWriteMode::CleanTableAndInsert,
            vec![build_row("c", "new", 2)],
        );

        assert_eq!(content(&db_table), vec!["c/new"]);
        // The partitions which are gone have a slot on disk to free, so they are
        // marked exactly like the one which was written.
        assert_eq!(result.partitions_to_persist, vec!["a", "b", "c"]);
    }

    #[test]
    fn a_clean_table_and_insert_with_no_rows_empties_the_table() {
        let db_table = table_with(vec![build_row("a", "old", 1)]);

        let result = db_table.bulk_write(BulkWriteMode::CleanTableAndInsert, Vec::new());

        assert!(content(&db_table).is_empty());
        assert_eq!(result.partitions_to_persist, vec!["a"]);
        assert!(result.written.is_empty());
    }

    /// A row which lost the comparison must not reach the subscribers either -
    /// applying it would move their cache backwards in time.
    #[test]
    fn insert_or_replace_if_new_keeps_the_newer_stored_row() {
        let db_table = table_with(vec![build_row("a", "1", 100)]);

        let result = db_table.bulk_write(
            BulkWriteMode::InsertOrReplaceIfNew,
            vec![
                build_row("a", "1", 50),  // older - refused
                build_row("a", "2", 50),  // new key - taken
                build_row("b", "1", 200), // new partition - taken
            ],
        );

        assert_eq!(written(&result), vec!["a/2", "b/1"]);
        assert_eq!(result.partitions_to_persist, vec!["a", "b"]);
        assert_eq!(
            db_table
                .get_row("a", "1")
                .unwrap()
                .get_time_stamp()
                .unix_microseconds,
            100
        );
    }

    #[test]
    fn insert_or_replace_if_new_takes_a_strictly_newer_row() {
        let db_table = table_with(vec![build_row("a", "1", 100)]);

        // The same moment is not newer: a client re-sending what the server has
        // must not wake every subscriber.
        let result = db_table.bulk_write(
            BulkWriteMode::InsertOrReplaceIfNew,
            vec![build_row("a", "1", 100)],
        );
        assert!(result.written.is_empty());
        assert!(result.partitions_to_persist.is_empty());

        let result = db_table.bulk_write(
            BulkWriteMode::InsertOrReplaceIfNew,
            vec![build_row("a", "1", 101)],
        );
        assert_eq!(written(&result), vec!["a/1"]);
    }

    #[test]
    fn a_batch_which_writes_the_same_key_twice_ends_on_the_last_of_them() {
        let db_table = table_with(Vec::new());

        let result = db_table.bulk_write(
            BulkWriteMode::InsertOrReplace,
            vec![build_row("a", "1", 1), build_row("a", "1", 2)],
        );

        assert_eq!(content(&db_table), vec!["a/1"]);
        assert_eq!(
            db_table
                .get_row("a", "1")
                .unwrap()
                .get_time_stamp()
                .unix_microseconds,
            2
        );
        // Both are announced, in the order they were written: the subscriber
        // applies them in the same order and ends on the same row.
        assert_eq!(result.written.len(), 2);
        assert_eq!(result.partitions_to_persist, vec!["a"]);
    }

    /// `lastWriteAt` exists to answer "when was this table really changed", so a
    /// batch which took nothing must leave it where it was - otherwise an
    /// operator hunting a stalled feed sees a live table.
    #[test]
    fn a_batch_which_took_no_row_leaves_the_last_write_moment_alone() {
        let db_table = DbTable::new("t".to_string(), DbTableAttributes::create_default());

        // Nothing to clean and nothing to insert: the table is exactly as it was.
        db_table.bulk_write(BulkWriteMode::CleanTableAndInsert, Vec::new());
        assert!(db_table.get_last_write_moment().is_none());

        db_table.bulk_write(
            BulkWriteMode::InsertOrReplace,
            vec![build_row("a", "1", 100)],
        );
        let after_the_write = db_table.get_last_write_moment().unwrap();

        // Every row of the batch lost the version comparison.
        let result = db_table.bulk_write(
            BulkWriteMode::InsertOrReplaceIfNew,
            vec![build_row("a", "1", 50)],
        );

        assert!(result.written.is_empty());
        assert_eq!(
            db_table.get_last_write_moment().unwrap().unix_microseconds,
            after_the_write.unix_microseconds
        );
    }

    #[test]
    fn delete_partitions_reports_only_the_ones_which_were_there() {
        let db_table = table_with(vec![build_row("a", "1", 1), build_row("b", "1", 1)]);

        let removed = db_table.delete_partitions(&["a".to_string(), "never-existed".to_string()]);

        assert_eq!(removed, vec!["a"]);
        assert_eq!(content(&db_table), vec!["b/1"]);
    }

    /// What the subscribers would be told, in the order they would be told it.
    fn changes(result: &TransactionResult) -> Vec<String> {
        result
            .changes
            .iter()
            .map(|change| match change {
                AppliedChange::TableCleaned => "cleaned".to_string(),
                AppliedChange::PartitionsDeleted(partition_keys) => {
                    format!("partitions-gone:{}", partition_keys.join("+"))
                }
                AppliedChange::RowsDeleted(partitions) => format!(
                    "rows-gone:{}",
                    partitions
                        .iter()
                        .map(|itm| format!("{}/{}", itm.partition_key, itm.row_keys.join("+")))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                AppliedChange::RowsWritten(rows) => format!(
                    "written:{}",
                    rows.iter()
                        .map(|db_row| format!(
                            "{}/{}",
                            db_row.get_partition_key(),
                            db_row.get_row_key()
                        ))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            })
            .collect()
    }

    /// The thing a transaction exists for: a partition is emptied and refilled
    /// without the table ever being visible in between.
    #[test]
    fn a_transaction_cleans_and_writes_in_one_visit() {
        let db_table = table_with(vec![build_row("a", "old", 1), build_row("b", "keep", 1)]);

        let result = db_table.apply_transaction(vec![
            TransactionAction::DeletePartitions(vec!["a".to_string()]),
            TransactionAction::InsertOrReplace(vec![build_row("a", "new", 2)]),
        ]);

        assert_eq!(content(&db_table), vec!["a/new", "b/keep"]);
        assert_eq!(result.partitions_to_persist, vec!["a"]);
        assert_eq!(changes(&result), vec!["partitions-gone:a", "written:a/new"]);
    }

    #[test]
    fn neighbours_of_the_same_kind_become_one_instruction() {
        let db_table = table_with(vec![
            build_row("a", "1", 1),
            build_row("b", "1", 1),
            build_row("c", "1", 1),
        ]);

        let result = db_table.apply_transaction(vec![
            TransactionAction::DeletePartitions(vec!["a".to_string()]),
            TransactionAction::DeletePartitions(vec!["b".to_string()]),
            TransactionAction::InsertOrReplace(vec![build_row("d", "1", 2)]),
            TransactionAction::InsertOrReplace(vec![build_row("e", "1", 2)]),
        ]);

        assert_eq!(
            changes(&result),
            vec!["partitions-gone:a+b", "written:d/1 e/1"]
        );
    }

    /// Merging may only ever join neighbours: a row written, deleted and written
    /// again has to reach a subscriber in exactly that order, or the subscriber
    /// ends up with the wrong one of the two.
    #[test]
    fn the_order_of_the_actions_survives() {
        let db_table = table_with(Vec::new());

        let result = db_table.apply_transaction(vec![
            TransactionAction::InsertOrReplace(vec![build_row("a", "1", 1)]),
            TransactionAction::DeleteRows(PartitionRowKeys {
                partition_key: "a".to_string(),
                row_keys: vec!["1".to_string()],
            }),
            TransactionAction::InsertOrReplace(vec![build_row("a", "1", 2)]),
        ]);

        assert_eq!(
            changes(&result),
            vec!["written:a/1", "rows-gone:a/1", "written:a/1"]
        );
        assert_eq!(content(&db_table), vec!["a/1"]);
        assert_eq!(
            db_table
                .get_row("a", "1")
                .unwrap()
                .get_time_stamp()
                .unix_microseconds,
            2
        );
    }

    #[test]
    fn what_changed_nothing_is_not_announced() {
        let db_table = table_with(vec![build_row("a", "1", 1)]);

        let result = db_table.apply_transaction(vec![
            TransactionAction::DeletePartitions(vec!["never-existed".to_string()]),
            TransactionAction::DeleteRows(PartitionRowKeys {
                partition_key: "a".to_string(),
                row_keys: vec!["never-existed".to_string()],
            }),
            TransactionAction::InsertOrReplace(Vec::new()),
        ]);

        assert!(changes(&result).is_empty());
        assert!(result.partitions_to_persist.is_empty());
        assert_eq!(content(&db_table), vec!["a/1"]);
    }

    #[test]
    fn cleaning_twice_over_is_announced_once_but_cleaning_after_a_write_is_not() {
        let db_table = table_with(vec![build_row("a", "1", 1)]);

        let result = db_table.apply_transaction(vec![
            TransactionAction::CleanTable,
            TransactionAction::CleanTable,
            TransactionAction::InsertOrReplace(vec![build_row("b", "1", 2)]),
            TransactionAction::CleanTable,
            TransactionAction::InsertOrReplace(vec![build_row("c", "1", 2)]),
        ]);

        assert_eq!(
            changes(&result),
            vec!["cleaned", "written:b/1", "cleaned", "written:c/1"]
        );
        assert_eq!(content(&db_table), vec!["c/1"]);
        // Everything the transaction touched has a slot to rewrite or to free.
        assert_eq!(result.partitions_to_persist, vec!["a", "b", "c"]);
    }

    /// Deleting the last row of a partition takes the partition with it, and the
    /// partition still has to be handed to the persist loop so its slot is freed.
    #[test]
    fn deleting_the_last_row_of_a_partition_still_marks_it() {
        let db_table = table_with(vec![build_row("a", "1", 1)]);

        let result =
            db_table.apply_transaction(vec![TransactionAction::DeleteRows(PartitionRowKeys {
                partition_key: "a".to_string(),
                row_keys: vec!["1".to_string()],
            })]);

        assert_eq!(db_table.get_partitions_amount(), 0);
        assert_eq!(result.partitions_to_persist, vec!["a"]);
        assert_eq!(changes(&result), vec!["rows-gone:a/1"]);
    }

    /// A limit is how many rows the caller has room for, so a caller with room
    /// for none gets none. A caller working out `remaining = cap - already_have`
    /// reaches zero on its last page, and one row back from that either
    /// overshoots the cap or never finishes.
    #[test]
    fn a_limit_of_zero_returns_no_rows_at_all() {
        let db_table = table_with(vec![
            build_row("a", "1", 1),
            build_row("a", "2", 1),
            build_row("b", "1", 1),
        ]);

        let taken = |skip, limit| {
            db_table
                .get_rows(&GetRowsFilter {
                    partition_key: None,
                    row_key: None,
                    skip,
                    limit,
                })
                .len()
        };

        assert_eq!(taken(None, Some(0)), 0);
        assert_eq!(taken(Some(1), Some(0)), 0);
        assert_eq!(taken(None, Some(2)), 2);
        // Absent is still "as many as there are".
        assert_eq!(taken(None, None), 3);
        assert_eq!(taken(None, Some(10)), 3);
    }

    // ---- garbage collection ----------------------------------------------

    fn with_attributes(attributes: DbTableAttributes, rows: Vec<Arc<DbRow>>) -> DbTable {
        let db_table = DbTable::new("t".to_string(), attributes);
        db_table.bulk_write(BulkWriteMode::InsertOrReplace, rows);
        db_table
    }

    fn moment(micros: i64) -> DateTimeAsMicroseconds {
        DateTimeAsMicroseconds::new(micros)
    }

    /// Sets when the row stops being wanted. `0` is what the entity carries when
    /// it never expires, and nothing collects those.
    fn expiring(partition_key: &str, row_key: &str, expires: i64) -> Arc<DbRow> {
        let db_row = build_row(partition_key, row_key, 1);
        db_row.update_expires(Some(moment(expires)));
        db_row
    }

    #[test]
    fn a_row_whose_expires_has_passed_is_collected() {
        let db_table = with_attributes(
            DbTableAttributes::create_default(),
            vec![
                expiring("a", "gone", 100),
                expiring("a", "later", 300),
                build_row("a", "never", 1),
            ],
        );

        // Nothing is due yet.
        assert!(db_table.gc(moment(99)).is_empty());

        let result = db_table.gc(moment(100));

        assert_eq!(content(&db_table), vec!["a/later", "a/never"]);
        assert_eq!(result.partitions_to_persist, vec!["a"]);
        assert!(result.partitions_removed.is_empty());
        assert_eq!(result.rows_removed.len(), 1);
        assert_eq!(result.rows_removed[0].partition_key, "a");
        assert_eq!(result.rows_removed[0].row_keys, vec!["gone".to_string()]);
    }

    /// Emptying a partition takes the partition with it, and then it is the
    /// partition the subscriber hears about - saying both would say the same
    /// thing twice.
    #[test]
    fn a_partition_emptied_by_the_expiry_goes_with_its_rows() {
        let db_table = with_attributes(
            DbTableAttributes::create_default(),
            vec![expiring("a", "only", 100), build_row("b", "stays", 1)],
        );

        let result = db_table.gc(moment(100));

        assert_eq!(content(&db_table), vec!["b/stays"]);
        assert_eq!(result.partitions_removed, vec!["a"]);
        assert!(result.rows_removed.is_empty());
    }

    #[test]
    fn the_partitions_nobody_read_for_the_longest_are_evicted() {
        let attributes = DbTableAttributes {
            max_partitions_amount: Some(2),
            ..DbTableAttributes::create_default()
        };

        let db_table = with_attributes(
            attributes,
            vec![
                build_row("oldest", "1", 1),
                build_row("middle", "1", 1),
                build_row("newest", "1", 1),
            ],
        );

        for (partition_key, read_at) in [("oldest", 10), ("middle", 20), ("newest", 30)] {
            db_table.apply_read_statistics(
                partition_key,
                &[],
                &UpdateReadStatistics {
                    update_partition_last_read: true,
                    ..Default::default()
                },
                moment(read_at),
            );
        }

        let result = db_table.gc(moment(1_000));

        assert_eq!(content(&db_table), vec!["middle/1", "newest/1"]);
        assert_eq!(result.partitions_removed, vec!["oldest"]);
    }

    #[test]
    fn the_rows_nobody_read_for_the_longest_are_evicted() {
        let attributes = DbTableAttributes {
            max_rows_per_partition_amount: Some(2),
            ..DbTableAttributes::create_default()
        };

        let db_table = with_attributes(
            attributes,
            vec![
                build_row("a", "oldest", 1),
                build_row("a", "middle", 1),
                build_row("a", "newest", 1),
                build_row("b", "alone", 1),
            ],
        );

        for (row_key, read_at) in [("oldest", 10), ("middle", 20), ("newest", 30)] {
            db_table.apply_read_statistics(
                "a",
                &[row_key.to_string()],
                &UpdateReadStatistics {
                    update_rows_last_read: true,
                    ..Default::default()
                },
                moment(read_at),
            );
        }

        let result = db_table.gc(moment(1_000));

        assert_eq!(content(&db_table), vec!["a/middle", "a/newest", "b/alone"]);
        assert_eq!(result.rows_removed[0].row_keys, vec!["oldest".to_string()]);
    }

    /// A row on its way out for one reason must not make the limit take out
    /// another one on top of it: what is expiring already counts as leaving.
    #[test]
    fn an_expiring_row_counts_towards_the_limit() {
        let attributes = DbTableAttributes {
            max_rows_per_partition_amount: Some(2),
            ..DbTableAttributes::create_default()
        };

        let db_table = with_attributes(
            attributes,
            vec![
                expiring("a", "expired", 100),
                build_row("a", "keep-1", 1),
                build_row("a", "keep-2", 1),
            ],
        );

        let result = db_table.gc(moment(100));

        assert_eq!(content(&db_table), vec!["a/keep-1", "a/keep-2"]);
        assert_eq!(result.rows_removed[0].row_keys, vec!["expired".to_string()]);
    }

    /// A row already on its way out is not a candidate for eviction as well: it
    /// is counted as leaving once, and the limit takes the coldest of the rows
    /// which are actually staying.
    #[test]
    fn eviction_looks_only_at_the_rows_the_expiry_is_not_already_taking() {
        let attributes = DbTableAttributes {
            max_rows_per_partition_amount: Some(2),
            ..DbTableAttributes::create_default()
        };

        let db_table = with_attributes(
            attributes,
            vec![
                expiring("a", "expired", 100),
                build_row("a", "cold", 1),
                build_row("a", "warm", 1),
                build_row("a", "hot", 1),
            ],
        );

        // The expiring row is the coldest of the four, so an eviction which
        // still saw it would pick it a second time and leave "cold" behind.
        for (row_key, read_at) in [("expired", 1), ("cold", 10), ("warm", 20), ("hot", 30)] {
            db_table.apply_read_statistics(
                "a",
                &[row_key.to_string()],
                &UpdateReadStatistics {
                    update_rows_last_read: true,
                    ..Default::default()
                },
                moment(read_at),
            );
        }

        let result = db_table.gc(moment(100));

        assert_eq!(content(&db_table), vec!["a/hot", "a/warm"]);
        assert_eq!(
            result.rows_removed[0].row_keys,
            vec!["expired".to_string(), "cold".to_string()]
        );
    }

    /// A table with no limits and nothing expiring is the normal case, and it
    /// has to come back as "nothing to do" rather than as an empty pass.
    #[test]
    fn a_table_with_nothing_to_collect_is_left_alone() {
        let db_table = with_attributes(
            DbTableAttributes::create_default(),
            vec![build_row("a", "1", 1), build_row("b", "1", 1)],
        );

        let result = db_table.gc(moment(i64::MAX));

        assert!(result.is_empty());
        assert!(result.partitions_to_persist.is_empty());
        assert_eq!(db_table.get_rows_amount(), 2);
    }

    /// Sliding expiration: reading is what pushes the moment forward, so a row
    /// which was about to go stays as long as somebody keeps asking for it.
    #[test]
    fn reading_a_row_can_push_its_expiry_forward() {
        let db_table = with_attributes(
            DbTableAttributes::create_default(),
            vec![expiring("a", "1", 100)],
        );

        db_table.apply_read_statistics(
            "a",
            &["1".to_string()],
            &UpdateReadStatistics {
                set_rows_expires: Some(Some(moment(500))),
                ..Default::default()
            },
            moment(50),
        );

        // The moment it would have gone by comes and goes.
        assert!(db_table.gc(moment(100)).is_empty());
        assert_eq!(content(&db_table), vec!["a/1"]);

        // ...and the new one still applies.
        assert!(!db_table.gc(moment(500)).is_empty());
        assert!(content(&db_table).is_empty());
    }

    /// "Leave it alone" and "set it to never" are opposite instructions, and the
    /// outer `Option` is what tells them apart.
    #[test]
    fn an_expiry_can_be_taken_off_a_row_entirely() {
        let db_table = with_attributes(
            DbTableAttributes::create_default(),
            vec![expiring("a", "1", 100)],
        );

        // Nothing said about the expiry: it stands.
        db_table.apply_read_statistics(
            "a",
            &["1".to_string()],
            &UpdateReadStatistics {
                update_rows_last_read: true,
                ..Default::default()
            },
            moment(50),
        );
        assert!(db_table.get_row("a", "1").unwrap().get_expires().is_some());

        db_table.apply_read_statistics(
            "a",
            &["1".to_string()],
            &UpdateReadStatistics {
                set_rows_expires: Some(None),
                ..Default::default()
            },
            moment(50),
        );

        assert!(db_table.get_row("a", "1").unwrap().get_expires().is_none());
        assert!(db_table.gc(moment(i64::MAX)).is_empty());
    }

    /// A partition can expire as a whole, and then it goes with its rows - the
    /// subscriber is told the partition is gone rather than told about each row.
    #[test]
    fn a_partition_whose_moment_came_goes_whole() {
        let db_table = with_attributes(
            DbTableAttributes::create_default(),
            vec![
                build_row("a", "1", 1),
                build_row("a", "2", 1),
                build_row("b", "1", 1),
            ],
        );

        db_table.apply_read_statistics(
            "a",
            &[],
            &UpdateReadStatistics {
                set_partition_expires: Some(Some(moment(100))),
                ..Default::default()
            },
            moment(1),
        );

        assert!(db_table.gc(moment(99)).is_empty());

        let result = db_table.gc(moment(100));

        assert_eq!(content(&db_table), vec!["b/1"]);
        assert_eq!(result.partitions_removed, vec!["a"]);
        assert!(result.rows_removed.is_empty());
    }

    #[test]
    fn a_limit_can_be_applied_by_hand_with_a_number_of_its_own() {
        let db_table = table_with(vec![
            build_row("a", "1", 1),
            build_row("b", "1", 1),
            build_row("c", "1", 1),
        ]);

        for (partition_key, read_at) in [("a", 10), ("b", 20), ("c", 30)] {
            db_table.apply_read_statistics(
                partition_key,
                &[],
                &UpdateReadStatistics {
                    update_partition_last_read: true,
                    ..Default::default()
                },
                moment(read_at),
            );
        }

        let result = db_table.keep_max_partitions_amount(1);

        assert_eq!(content(&db_table), vec!["c/1"]);
        assert_eq!(result.partitions_removed, vec!["a", "b"]);

        // Keeping none of them is a coherent thing to ask for.
        let db_table = table_with(vec![build_row("a", "1", 1)]);
        assert_eq!(
            db_table
                .keep_max_partitions_amount(0)
                .partitions_removed
                .len(),
            1
        );
    }

    // ---- schemas -----------------------------------------------------------

    fn schema(id: u64) -> EntitySchema {
        EntitySchema::new(id, vec![id as u8])
    }

    fn schema_ids(db_table: &DbTable) -> Vec<u64> {
        let mut result: Vec<u64> = db_table.get_attributes().schemas.keys().copied().collect();
        result.sort_unstable();
        result
    }

    #[test]
    fn a_schema_is_registered_once_and_the_first_writer_wins() {
        let db_table = DbTable::new("t".to_string(), DbTableAttributes::create_default());

        assert!(db_table.register_schema(schema(7)));
        assert!(!db_table.register_schema(EntitySchema::new(7, vec![9, 9])));

        assert_eq!(schema_ids(&db_table), vec![7]);
        assert_eq!(db_table.get_schema(7).unwrap().schema, vec![7]);
        assert!(db_table.get_schema(8).is_none());
    }

    /// A request naming a limit carries no schemas, and replacing the attributes
    /// with it must not leave every stored row without a shape.
    #[test]
    fn setting_the_attributes_keeps_the_schemas_the_rows_were_written_under() {
        let db_table = DbTable::new("t".to_string(), DbTableAttributes::create_default());
        db_table.register_schema(schema(7));

        db_table.set_attributes(DbTableAttributes {
            max_partitions_amount: Some(5),
            ..DbTableAttributes::create_default()
        });

        assert_eq!(db_table.get_attributes().max_partitions_amount, Some(5));
        assert_eq!(schema_ids(&db_table), vec![7]);
    }

    /// ...and the same call is how a restore hands back what its archive
    /// carried, so what it names on top of them goes in.
    #[test]
    fn setting_the_attributes_adds_the_schemas_it_carries() {
        let db_table = DbTable::new("t".to_string(), DbTableAttributes::create_default());
        db_table.register_schema(schema(7));

        let mut incoming = AHashMap::new();
        incoming.insert(7, Arc::new(EntitySchema::new(7, vec![0xFF])));
        incoming.insert(8, Arc::new(schema(8)));

        db_table.set_attributes(DbTableAttributes {
            schemas: Arc::new(incoming),
            ..DbTableAttributes::create_default()
        });

        assert_eq!(schema_ids(&db_table), vec![7, 8]);
        // The stored one still wins, exactly as on the write path.
        assert_eq!(db_table.get_schema(7).unwrap().schema, vec![7]);
    }

    #[test]
    fn a_schema_no_row_names_any_more_is_dropped() {
        let db_table = DbTable::new("t".to_string(), DbTableAttributes::create_default());

        db_table.register_schema(schema(1));
        db_table.register_schema(schema(2));
        db_table.insert_or_replace(build_row_of_schema("a", "1", 1, 1));

        assert!(db_table.gc_schemas());
        assert_eq!(schema_ids(&db_table), vec![1]);
    }

    /// One schema is what almost every table holds, and the pass must not look
    /// at a single row to find that out.
    #[test]
    fn a_table_of_one_schema_has_nothing_to_collect() {
        let db_table = DbTable::new("t".to_string(), DbTableAttributes::create_default());

        db_table.register_schema(schema(1));
        db_table.insert_or_replace(build_row_of_schema("a", "1", 1, 1));

        assert!(!db_table.gc_schemas());
        assert_eq!(schema_ids(&db_table), vec![1]);
    }

    /// An emptied table is still drawn by the UI, and the columns come from the
    /// shape - so the last one stays however dead it is.
    #[test]
    fn the_last_schema_stays_even_when_no_row_is_left() {
        let db_table = DbTable::new("t".to_string(), DbTableAttributes::create_default());

        db_table.register_schema(schema(1));
        db_table.register_schema(schema(2));
        db_table.insert_or_replace(build_row_of_schema("a", "1", 1, 1));
        db_table.clean();

        assert!(db_table.gc_schemas());
        assert_eq!(schema_ids(&db_table), vec![2]);
        // ...and there is nothing left to take a second time.
        assert!(!db_table.gc_schemas());
    }

    /// The rows are the rows the last walk saw, so the answer is the answer it
    /// got. Without this a genuinely mixed table pays a full scan every pass for
    /// as long as it lives.
    #[test]
    fn a_table_nobody_wrote_to_is_not_walked_again() {
        let db_table = DbTable::new("t".to_string(), DbTableAttributes::create_default());

        db_table.register_schema(schema(1));
        db_table.register_schema(schema(2));
        db_table.insert_or_replace(build_row_of_schema("a", "1", 1, 1));

        assert!(db_table.gc_schemas());

        // Registering a schema is not writing to the table, and the dead one it
        // leaves behind is invisible until something actually is written.
        db_table.register_schema(schema(3));
        assert!(!db_table.gc_schemas());
        assert_eq!(schema_ids(&db_table), vec![1, 3]);

        db_table.insert_or_replace(build_row_of_schema("a", "2", 2, 1));

        assert!(db_table.gc_schemas());
        assert_eq!(schema_ids(&db_table), vec![1]);
    }

    /// The other half of the rule above, and the one the collector exists for.
    ///
    /// A table loaded from disk arrives with the schemas of its whole previous
    /// life and with nothing written to it since the process started. "Never
    /// looked" and "looked, and nothing was written since" are different states,
    /// and only a separate value for the first tells them apart - collapse them
    /// and this table never qualifies for a walk, because nobody is obliged to
    /// ever write to it again. The dead schemas would then be loaded at every
    /// start forever, which is the leak the collector was written to stop.
    #[test]
    fn a_table_restored_from_disk_is_walked_once_even_if_nobody_writes_to_it() {
        let mut schemas = AHashMap::new();
        schemas.insert(1, Arc::new(schema(1)));
        schemas.insert(2, Arc::new(schema(2)));

        let db_table = DbTable::new(
            "t".to_string(),
            DbTableAttributes {
                schemas: Arc::new(schemas),
                ..DbTableAttributes::create_default()
            },
        );

        // No row names either of them and nothing has been written, yet the walk
        // still happens - and the highest id is what an empty table keeps.
        assert!(db_table.gc_schemas());
        assert_eq!(schema_ids(&db_table), vec![2]);

        // ...and only once: the second pass has nothing to look at.
        assert!(!db_table.gc_schemas());
    }

    #[test]
    fn clean_hands_over_every_partition_it_dropped() {
        let db_table = table_with(vec![build_row("a", "1", 1), build_row("b", "1", 1)]);

        assert_eq!(db_table.clean(), vec!["a", "b"]);
        assert_eq!(db_table.get_partitions_amount(), 0);
        // Nothing left means nothing to free the second time round.
        assert!(db_table.clean().is_empty());
    }
}
