use ahash::AHashMap;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::{PersistQueueMetrics, PersistTask};

#[derive(Default)]
struct TableMarkers {
    metadata: Option<DateTimeAsMicroseconds>,
    /// partition_key -> the moment it may not be written later than.
    partitions: AHashMap<String, DateTimeAsMicroseconds>,
}

impl TableMarkers {
    fn is_empty(&self) -> bool {
        self.metadata.is_none() && self.partitions.is_empty()
    }
}

/// What is waiting to be written, and from which moment on it may be written.
///
/// A due moment only ever moves earlier: two writes to the same partition, one
/// asking for `Sec30` and one for `Immediately`, must not let the relaxed one
/// hold the urgent one back.
#[derive(Default)]
pub(super) struct PersistMarkersInner {
    tables: AHashMap<String, TableMarkers>,
}

impl PersistMarkersInner {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn persist_partition(
        &mut self,
        table_name: &str,
        partition_key: &str,
        persist_moment: DateTimeAsMicroseconds,
    ) {
        let table = self.get_or_create(table_name);

        match table.partitions.get_mut(partition_key) {
            Some(existing) => keep_earliest(existing, persist_moment),
            None => {
                table
                    .partitions
                    .insert(partition_key.to_string(), persist_moment);
            }
        }
    }

    pub(super) fn persist_table_metadata(
        &mut self,
        table_name: &str,
        persist_moment: DateTimeAsMicroseconds,
    ) {
        let table = self.get_or_create(table_name);

        match table.metadata.as_mut() {
            Some(existing) => keep_earliest(existing, persist_moment),
            None => table.metadata = Some(persist_moment),
        }
    }

    fn get_or_create(&mut self, table_name: &str) -> &mut TableMarkers {
        self.tables.entry(table_name.to_string()).or_default()
    }

    pub(super) fn has_something_to_persist(&self) -> bool {
        self.tables.values().any(|itm| !itm.is_empty())
    }

    /// How much is owed to the disk. Counted from the map sizes, which are
    /// `len()` calls - the mutex this takes is the one every write takes to mark
    /// a partition, so a scrape may not walk anything under it.
    pub(super) fn get_queue_metrics(&self) -> PersistQueueMetrics {
        let mut result = PersistQueueMetrics {
            partitions: 0,
            tables_metadata: 0,
        };

        for markers in self.tables.values() {
            result.partitions += markers.partitions.len();
            result.tables_metadata += usize::from(markers.metadata.is_some());
        }

        result
    }

    /// Takes everything out at once, in the order [`Self::get_task`] would have
    /// handed it out: each table's metadata before that table's partitions.
    ///
    /// A snapshot rather than a drain - what is marked after this returns
    /// belongs to whoever asks next. That is what bounds a flush running against
    /// a live writer: draining until the queue is empty would chase it forever.
    pub(super) fn take_all(&mut self) -> Vec<PersistTask> {
        let mut result = Vec::new();

        for (table_name, markers) in self.tables.drain() {
            if markers.metadata.is_some() {
                result.push(PersistTask::TableMetadata {
                    table_name: table_name.clone(),
                });
            }

            for partition_key in markers.partitions.into_keys() {
                result.push(PersistTask::Partition {
                    table_name: table_name.clone(),
                    partition_key,
                });
            }
        }

        result
    }

    /// Takes one due task out of the set. `now: None` means "everything is due",
    /// which is what the shutdown drain uses.
    pub(super) fn get_task(&mut self, now: Option<DateTimeAsMicroseconds>) -> Option<PersistTask> {
        // Metadata before content: a partition of a table the metadata never
        // mentioned would be loaded into a table with default attributes - and,
        // since the schemas of a table ride in its metadata, a partition whose
        // rows nothing could be shown through.
        let mut table_to_clean = None;
        let mut result = None;

        for (table_name, markers) in self.tables.iter_mut() {
            if is_due(markers.metadata, now) {
                markers.metadata = None;
                result = Some(PersistTask::TableMetadata {
                    table_name: table_name.clone(),
                });
            } else {
                let due_partition = markers
                    .partitions
                    .iter()
                    .find(|(_, moment)| is_due(Some(**moment), now))
                    .map(|(partition_key, _)| partition_key.clone());

                let Some(partition_key) = due_partition else {
                    continue;
                };

                markers.partitions.remove(&partition_key);
                result = Some(PersistTask::Partition {
                    table_name: table_name.clone(),
                    partition_key,
                });
            }

            if markers.is_empty() {
                table_to_clean = Some(table_name.clone());
            }

            break;
        }

        if let Some(table_name) = table_to_clean {
            self.tables.remove(&table_name);
        }

        result
    }
}

fn keep_earliest(current: &mut DateTimeAsMicroseconds, candidate: DateTimeAsMicroseconds) {
    if candidate.unix_microseconds < current.unix_microseconds {
        *current = candidate;
    }
}

fn is_due(moment: Option<DateTimeAsMicroseconds>, now: Option<DateTimeAsMicroseconds>) -> bool {
    let Some(moment) = moment else {
        return false;
    };

    let Some(now) = now else {
        return true;
    };

    moment.unix_microseconds <= now.unix_microseconds
}
