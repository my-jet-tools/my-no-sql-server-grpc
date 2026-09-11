use std::sync::Arc;

use ahash::AHashSet;
use rust_extensions::sorted_vec::SortedVecWithStrKey;

use super::bulk_write::distinct_partition_keys;
use super::{
    AppliedChange, BulkWriteMode, BulkWriteResult, DbPartition, DbRow, GcResult, GetRowsFilter,
    PartitionRowKeys, TransactionAction, TransactionResult,
};

/// Whether an expiry moment has come and gone. `None` is "never", which is what
/// `0` on the wire means.
fn is_expired(
    expires: Option<rust_extensions::date_time::DateTimeAsMicroseconds>,
    now: i64,
) -> bool {
    match expires {
        Some(expires) => expires.unix_microseconds <= now,
        None => false,
    }
}

/// Rows whose `Expires` has come and gone. `0` means the row never expires, and
/// [`DbRow::get_expires`] already reads it that way.
fn expired_row_keys(partition: &DbPartition, now: i64) -> Vec<String> {
    partition
        .get_all_rows()
        .iter()
        .filter(|db_row| is_expired(db_row.get_expires(), now))
        .map(|db_row| db_row.get_row_key().to_string())
        .collect()
}

/// Whether the partition holds a row whose moment has come. Separate from
/// [`expired_row_keys`] because the cheap look only ever needs the answer, and
/// materialising every expired key of a partition to find out that one exists is
/// the work that look is there to avoid.
fn has_expired_row(partition: &DbPartition, now: i64) -> bool {
    partition
        .get_all_rows()
        .iter()
        .any(|db_row| is_expired(db_row.get_expires(), now))
}

/// The rows nobody has read for the longest, so that at most `max` are left.
fn append_rows_to_evict(partition: &DbPartition, max: usize, dest: &mut Vec<String>) {
    // What is already going out counts towards the limit - evicting on top of an
    // expiry would take out more rows than the limit asked for.
    let staying = partition.rows_count() - dest.len().min(partition.rows_count());

    if staying <= max {
        return;
    }

    // The keys already going out are put into a set once instead of being
    // scanned per row: this runs under the write lock, and the two conditions
    // which bring it here at all - a partition which has expiring rows and
    // exceeds its row limit - are exactly the case where both sequences are
    // long, so a scan per row is the whole table frozen for the square of them.
    let leaving: AHashSet<&str> = dest.iter().map(|row_key| row_key.as_str()).collect();

    let mut by_last_read: Vec<(&str, i64)> = partition
        .get_all_rows()
        .iter()
        .filter(|db_row| !leaving.contains(db_row.get_row_key()))
        .map(|db_row| {
            (
                db_row.get_row_key(),
                db_row.get_last_read_access().unix_microseconds,
            )
        })
        .collect();

    by_last_read.sort_unstable_by_key(|(_, last_read)| *last_read);
    by_last_read.truncate(staying - max);

    dest.extend(
        by_last_read
            .into_iter()
            .map(|(row_key, _)| row_key.to_string()),
    );
}

/// All the data of one table. Every method takes `&mut self` / `&self`, so the
/// borrow checker - not a set of per-field locks - is what keeps the partitions
/// and their rows consistent with each other.
pub(super) struct DbTableInner {
    partitions: SortedVecWithStrKey<DbPartition>,
}

impl DbTableInner {
    pub(super) fn new() -> Self {
        Self {
            partitions: SortedVecWithStrKey::new(),
        }
    }

    fn get_mut_or_create(&mut self, partition_key: &str) -> &mut DbPartition {
        match self.partitions.get_mut_or_create(partition_key) {
            rust_extensions::sorted_vec::GetMutOrCreateEntry::GetMut(partition) => partition,
            rust_extensions::sorted_vec::GetMutOrCreateEntry::Create(entry) => {
                entry.insert_and_get_value_mut(DbPartition::new(partition_key.to_string()))
            }
        }
    }

    pub(super) fn insert_or_replace(&mut self, db_row: Arc<DbRow>) -> Option<Arc<DbRow>> {
        let partition = self.get_mut_or_create(db_row.get_partition_key());
        partition.insert_or_replace(db_row)
    }

    pub(super) fn insert_if_not_exists(&mut self, db_row: Arc<DbRow>) -> bool {
        let partition = self.get_mut_or_create(db_row.get_partition_key());
        partition.insert_if_not_exists(db_row)
    }

    pub(super) fn replace_if_exists(&mut self, db_row: Arc<DbRow>) -> bool {
        let Some(partition) = self.partitions.get_mut(db_row.get_partition_key()) else {
            return false;
        };

        partition.replace_if_exists(db_row)
    }

    /// Applies a whole batch. Nothing here is allowed to be split across two
    /// acquisitions of the table's lock: a batch is the only transaction this
    /// server has, so a reader taking a snapshot must find the table either
    /// before it or after it, never inside it.
    pub(super) fn bulk_write(
        &mut self,
        mode: BulkWriteMode,
        rows: Vec<Arc<DbRow>>,
    ) -> BulkWriteResult {
        // Starts as the partitions the batch empties - their slot on disk has to
        // be rewritten (or freed) even when the batch puts nothing back into
        // them - and collects the ones it writes into as it goes.
        let mut partitions_to_persist = match mode {
            BulkWriteMode::CleanTableAndInsert => self.clean(),
            BulkWriteMode::CleanPartitionsAndInsert => {
                let partition_keys = distinct_partition_keys(&rows);

                for partition_key in partition_keys.iter() {
                    self.partitions.remove(partition_key);
                }

                partition_keys
            }
            BulkWriteMode::InsertOrReplace | BulkWriteMode::InsertOrReplaceIfNew => Vec::new(),
        };

        let mut written = Vec::with_capacity(rows.len());

        for db_row in rows {
            if mode == BulkWriteMode::InsertOrReplaceIfNew && !self.is_newer_than_stored(&db_row) {
                continue;
            }

            partitions_to_persist.push(db_row.get_partition_key().to_string());
            self.insert_or_replace(db_row.clone());
            written.push(db_row);
        }

        partitions_to_persist.sort_unstable();
        partitions_to_persist.dedup();

        BulkWriteResult {
            written,
            partitions_to_persist,
        }
    }

    /// Applies a transaction: writes of several kinds, in the order the client
    /// built them, in one visit to the table.
    ///
    /// What comes back is the same list with the changes that changed nothing
    /// dropped and neighbours of the same kind merged - so a client which posted
    /// ten `DeletePartitions` in a row costs its subscribers one instruction,
    /// while a delete followed by a write followed by a delete still reaches
    /// them as three, in that order.
    pub(super) fn apply_transaction(
        &mut self,
        actions: Vec<TransactionAction>,
    ) -> TransactionResult {
        let mut changes: Vec<AppliedChange> = Vec::new();
        let mut partitions_to_persist = Vec::new();

        for action in actions {
            match action {
                TransactionAction::CleanTable => {
                    partitions_to_persist.extend(self.clean());

                    // Cleaning what the previous action already cleaned says
                    // nothing new. Cleaning after anything else does.
                    if !matches!(changes.last(), Some(AppliedChange::TableCleaned)) {
                        changes.push(AppliedChange::TableCleaned);
                    }
                }

                TransactionAction::DeletePartitions(partition_keys) => {
                    let removed = self.delete_partitions(&partition_keys);

                    if removed.is_empty() {
                        continue;
                    }

                    partitions_to_persist.extend(removed.iter().cloned());

                    match changes.last_mut() {
                        Some(AppliedChange::PartitionsDeleted(merged)) => merged.extend(removed),
                        _ => changes.push(AppliedChange::PartitionsDeleted(removed)),
                    }
                }

                TransactionAction::DeleteRows(keys) => {
                    let row_keys: Vec<String> = keys
                        .row_keys
                        .into_iter()
                        .filter(|row_key| self.delete_row(&keys.partition_key, row_key).is_some())
                        .collect();

                    if row_keys.is_empty() {
                        continue;
                    }

                    partitions_to_persist.push(keys.partition_key.clone());

                    let removed = PartitionRowKeys {
                        partition_key: keys.partition_key,
                        row_keys,
                    };

                    match changes.last_mut() {
                        Some(AppliedChange::RowsDeleted(merged)) => merged.push(removed),
                        _ => changes.push(AppliedChange::RowsDeleted(vec![removed])),
                    }
                }

                TransactionAction::InsertOrReplace(rows) => {
                    if rows.is_empty() {
                        continue;
                    }

                    for db_row in rows.iter() {
                        partitions_to_persist.push(db_row.get_partition_key().to_string());
                        self.insert_or_replace(db_row.clone());
                    }

                    match changes.last_mut() {
                        Some(AppliedChange::RowsWritten(merged)) => merged.extend(rows),
                        _ => changes.push(AppliedChange::RowsWritten(rows)),
                    }
                }
            }
        }

        partitions_to_persist.sort_unstable();
        partitions_to_persist.dedup();

        TransactionResult {
            changes,
            partitions_to_persist,
        }
    }

    /// Whether a pass would take anything out at all.
    ///
    /// This is the whole reason the pass runs under a read lock first: on almost
    /// every tick the answer is no, and a table nothing is expiring out of has no
    /// business blocking its writers thirty times a minute.
    ///
    /// It walks the rows in the worst case, stopping at the first thing it
    /// finds. A per-partition index of expiry moments would make it O(1), and is
    /// what to reach for when a profile says this shows up - not before.
    pub(super) fn has_anything_to_gc(
        &self,
        now: i64,
        attributes: &super::DbTableAttributes,
    ) -> bool {
        if let Some(max) = attributes.max_partitions_amount
            && self.partitions.len() > max
        {
            return true;
        }

        for partition in self.partitions.iter() {
            if is_expired(partition.get_expires(), now) {
                return true;
            }

            if let Some(max) = attributes.max_rows_per_partition_amount
                && partition.rows_count() > max
            {
                return true;
            }

            if has_expired_row(partition, now) {
                return true;
            }
        }

        false
    }

    /// One pass, applied where it was decided - which is what keeps it from
    /// deleting a row somebody wrote or read between the two.
    ///
    /// The order is the point. Partitions are evicted first, so the rows inside
    /// them are gone before anything looks at rows: a subscriber told the
    /// partition is gone must not then be told about the rows that were in it.
    pub(super) fn gc(&mut self, now: i64, attributes: &super::DbTableAttributes) -> GcResult {
        let mut result = GcResult::default();

        // A partition whose own moment has come goes whole, before anything
        // looks at rows - and before the limit does, so it does not count
        // towards a limit it is about to leave.
        let expired: Vec<String> = self
            .partitions
            .iter()
            .filter(|partition| is_expired(partition.get_expires(), now))
            .map(|partition| partition.get_partition_key().to_string())
            .collect();

        for partition_key in expired.iter() {
            self.partitions.remove(partition_key);
        }

        result.partitions_to_persist.extend(expired.iter().cloned());
        result.partitions_removed.extend(expired);

        if let Some(max) = attributes.max_partitions_amount {
            self.evict_partitions(max, &mut result);
        }

        let plan: Vec<PartitionRowKeys> = self
            .partitions
            .iter()
            .filter_map(|partition| {
                let mut row_keys = expired_row_keys(partition, now);

                if let Some(max) = attributes.max_rows_per_partition_amount {
                    append_rows_to_evict(partition, max, &mut row_keys);
                }

                if row_keys.is_empty() {
                    return None;
                }

                Some(PartitionRowKeys {
                    partition_key: partition.get_partition_key().to_string(),
                    row_keys,
                })
            })
            .collect();

        self.remove_rows(plan, &mut result);

        result.partitions_to_persist.sort_unstable();
        result.partitions_to_persist.dedup();

        result
    }

    /// Keeps at most `max` partitions, dropping the ones nobody has read for the
    /// longest. What a reader tells the server it read is what feeds this, which
    /// is why the last-read mark is worth carrying at all.
    pub(super) fn keep_max_partitions_amount(&mut self, max: usize) -> GcResult {
        let mut result = GcResult::default();
        self.evict_partitions(max, &mut result);
        result
    }

    /// The same for the rows of one partition.
    pub(super) fn keep_max_rows_in_partition(
        &mut self,
        partition_key: &str,
        max: usize,
    ) -> GcResult {
        let mut result = GcResult::default();

        let Some(partition) = self.partitions.get(partition_key) else {
            return result;
        };

        let mut row_keys = Vec::new();
        append_rows_to_evict(partition, max, &mut row_keys);

        if row_keys.is_empty() {
            return result;
        }

        self.remove_rows(
            vec![PartitionRowKeys {
                partition_key: partition_key.to_string(),
                row_keys,
            }],
            &mut result,
        );

        result
    }

    fn evict_partitions(&mut self, max: usize, dest: &mut GcResult) {
        if self.partitions.len() <= max {
            return;
        }

        let mut by_last_read: Vec<(&str, i64)> = self
            .partitions
            .iter()
            .map(|partition| {
                (
                    partition.get_partition_key(),
                    partition.get_last_read_access().unix_microseconds,
                )
            })
            .collect();

        by_last_read.sort_unstable_by_key(|(_, last_read)| *last_read);
        by_last_read.truncate(self.partitions.len() - max);

        let victims: Vec<String> = by_last_read
            .into_iter()
            .map(|(partition_key, _)| partition_key.to_string())
            .collect();

        for partition_key in victims.iter() {
            self.partitions.remove(partition_key);
        }

        dest.partitions_to_persist.extend(victims.iter().cloned());
        dest.partitions_removed.extend(victims);
    }

    /// Takes the planned rows out. A partition emptied by it goes with them, and
    /// then it is the partition the subscriber is told about rather than the
    /// rows - the same thing said once instead of twice.
    fn remove_rows(&mut self, plan: Vec<PartitionRowKeys>, dest: &mut GcResult) {
        for partition in plan {
            for row_key in partition.row_keys.iter() {
                self.delete_row(&partition.partition_key, row_key);
            }

            dest.partitions_to_persist
                .push(partition.partition_key.clone());

            if self
                .partitions
                .get(partition.partition_key.as_str())
                .is_none()
            {
                dest.partitions_removed.push(partition.partition_key);
                continue;
            }

            dest.rows_removed.push(partition);
        }
    }

    /// A row nobody has stored yet is new by definition. Equal timestamps keep
    /// the stored row: a client re-sending what the server already has must not
    /// push it to every subscriber again.
    fn is_newer_than_stored(&self, db_row: &DbRow) -> bool {
        let Some(stored) = self
            .partitions
            .get(db_row.get_partition_key())
            .and_then(|partition| partition.get_row(db_row.get_row_key()))
        else {
            return true;
        };

        db_row.get_time_stamp().unix_microseconds > stored.get_time_stamp().unix_microseconds
    }

    /// Returns the keys of the partitions which were actually there - the ones
    /// whose slot on disk has to go with them.
    pub(super) fn delete_partitions(&mut self, partition_keys: &[String]) -> Vec<String> {
        let mut result = Vec::new();

        for partition_key in partition_keys {
            if self.partitions.remove(partition_key).is_some() {
                result.push(partition_key.clone());
            }
        }

        result
    }

    pub(super) fn get_row_statistics(
        &self,
        partition_key: &str,
        row_key: &str,
    ) -> super::GetRowStatisticsResult {
        use super::{GetRowStatisticsResult, RowStatistics};

        let Some(partition) = self.partitions.get(partition_key) else {
            return GetRowStatisticsResult::PartitionNotFound;
        };

        let Some(db_row) = partition.get_row(row_key) else {
            return GetRowStatisticsResult::RowNotFound;
        };

        GetRowStatisticsResult::Found(RowStatistics {
            partition_last_read_access: partition.get_last_read_access(),
            partition_expires: partition.get_expires(),
            partition_rows_count: partition.rows_count(),
            partition_content_size: partition.get_content_size(),
            row_time_stamp: db_row.get_time_stamp(),
            row_last_read_access: db_row.get_last_read_access(),
            row_expires: db_row.get_expires(),
            row_stored_size: db_row.get_stored_size(),
        })
    }

    pub(super) fn get_row(&self, partition_key: &str, row_key: &str) -> Option<Arc<DbRow>> {
        self.partitions
            .get(partition_key)?
            .get_row(row_key)
            .cloned()
    }

    /// Removes the row and, when that empties the partition, the partition too -
    /// an empty partition still costs a slot on disk.
    pub(super) fn delete_row(&mut self, partition_key: &str, row_key: &str) -> Option<Arc<DbRow>> {
        let partition = self.partitions.get_mut(partition_key)?;

        let removed = partition.remove_row(row_key)?;

        if partition.is_empty() {
            self.partitions.remove(partition_key);
        }

        Some(removed)
    }

    pub(super) fn get_rows(&self, filter: &GetRowsFilter) -> Vec<Arc<DbRow>> {
        let mut result = Vec::new();
        let mut skipped = 0;

        for partition in self.iter_partitions(filter.partition_key) {
            for db_row in partition.get_all_rows() {
                if let Some(row_key) = filter.row_key
                    && db_row.get_row_key() != row_key
                {
                    continue;
                }

                if let Some(skip) = filter.skip
                    && skipped < skip
                {
                    skipped += 1;
                    continue;
                }

                // Asked before the row is taken, not after: a caller computing
                // `remaining = cap - already_have` reaches zero, and a zero
                // which returns one row overshoots the cap or never terminates.
                if let Some(limit) = filter.limit
                    && result.len() >= limit
                {
                    return result;
                }

                result.push(db_row.clone());
            }
        }

        result
    }

    /// Rows of one partition whose key is at or below `row_key`, the highest
    /// one first.
    ///
    /// The rows are already sorted by key, so this is a binary search and a walk
    /// backwards from it - not a scan and not a sort.
    pub(super) fn get_highest_row_and_below(
        &self,
        partition_key: &str,
        row_key: &str,
        limit: Option<usize>,
    ) -> Vec<Arc<DbRow>> {
        let Some(partition) = self.partitions.get(partition_key) else {
            return Vec::new();
        };

        let rows = partition.get_all_rows();

        // The first row strictly above the key: everything before it is at or
        // below, which is what was asked for.
        let above = rows.partition_point(|db_row| db_row.get_row_key() <= row_key);

        let mut result: Vec<Arc<DbRow>> = rows[..above].iter().rev().cloned().collect();

        if let Some(limit) = limit {
            result.truncate(limit);
        }

        result
    }

    /// The rows named, in the order they were named. A key which is not there is
    /// left out rather than reported - the caller asked for rows, not for an
    /// answer per key.
    pub(super) fn get_single_partition_multiple_rows(
        &self,
        partition_key: &str,
        row_keys: &[String],
    ) -> Vec<Arc<DbRow>> {
        let Some(partition) = self.partitions.get(partition_key) else {
            return Vec::new();
        };

        row_keys
            .iter()
            .filter_map(|row_key| partition.get_row(row_key).cloned())
            .collect()
    }

    fn iter_partitions<'s>(
        &'s self,
        partition_key: Option<&str>,
    ) -> Box<dyn Iterator<Item = &'s DbPartition> + 's> {
        match partition_key {
            Some(partition_key) => Box::new(self.partitions.get(partition_key).into_iter()),
            None => Box::new(self.partitions.iter()),
        }
    }

    /// Everything is done through `&self`: a last-read mark and an expiry are
    /// atomics on the row and on the partition, so a reader saying what it read
    /// never blocks a writer. Which is the point - it happens on every call a
    /// reader makes.
    pub(super) fn apply_read_statistics(
        &self,
        partition_key: &str,
        row_keys: &[String],
        statistics: &super::UpdateReadStatistics,
        now: rust_extensions::date_time::DateTimeAsMicroseconds,
    ) {
        let Some(partition) = self.partitions.get(partition_key) else {
            return;
        };

        if statistics.update_partition_last_read {
            partition.update_last_read_access(now);
        }

        if let Some(expires) = statistics.set_partition_expires {
            partition.update_expires(expires);
        }

        if !statistics.update_rows_last_read && statistics.set_rows_expires.is_none() {
            return;
        }

        for row_key in row_keys {
            let Some(db_row) = partition.get_row(row_key) else {
                continue;
            };

            if statistics.update_rows_last_read {
                db_row.update_last_read_access(now);
            }

            if let Some(expires) = statistics.set_rows_expires {
                db_row.update_expires(expires);
            }
        }
    }

    /// The ids of the schemas the stored rows actually name.
    ///
    /// Every row is looked at because that is what the question is: a schema is
    /// unused only when no row anywhere in the table refers to it, and there is
    /// no shortcut to that. Nothing is cloned on the way - the caller wants a
    /// set of eight-byte numbers, not the rows.
    pub(super) fn collect_schema_ids(&self) -> AHashSet<u64> {
        let mut result = AHashSet::new();

        for partition in self.partitions.iter() {
            for db_row in partition.get_all_rows() {
                result.insert(db_row.get_schema_id());
            }
        }

        result
    }

    pub(super) fn get_partitions_amount(&self) -> usize {
        self.partitions.len()
    }

    pub(super) fn get_rows_amount(&self) -> usize {
        self.partitions.iter().map(|itm| itm.rows_count()).sum()
    }

    pub(super) fn get_content_size(&self) -> usize {
        self.partitions
            .iter()
            .map(|itm| itm.get_content_size())
            .sum()
    }

    pub(super) fn get_partition_keys(&self) -> Vec<String> {
        self.partitions
            .iter()
            .map(|itm| itm.get_partition_key().to_string())
            .collect()
    }

    pub(super) fn restore_partition(&mut self, partition: DbPartition) {
        self.partitions.insert_or_replace(partition);
    }

    /// Returns the keys of every partition the table held, so their slots on
    /// disk can be freed - the table itself forgets them here and now.
    pub(super) fn clean(&mut self) -> Vec<String> {
        let result = self.get_partition_keys();
        self.partitions.clear(None);
        result
    }
}
