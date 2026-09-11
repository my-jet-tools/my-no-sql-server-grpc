use parking_lot::Mutex;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::persist_markers_inner::PersistMarkersInner;
use super::{PersistQueueMetrics, PersistTask};

/// The write-behind queue of one namespace: what changed and from which moment
/// the persist loop may take it to disk.
///
/// A `parking_lot` mutex is right here - every critical section is a handful of
/// map operations and nothing under it awaits. The disk work happens after the
/// task has been taken out.
pub struct PersistMarkers {
    inner: Mutex<PersistMarkersInner>,
}

impl PersistMarkers {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(PersistMarkersInner::new()),
        }
    }

    pub fn persist_partition(
        &self,
        table_name: &str,
        partition_key: &str,
        persist_moment: DateTimeAsMicroseconds,
    ) {
        self.inner
            .lock()
            .persist_partition(table_name, partition_key, persist_moment);
    }

    pub fn persist_table_metadata(&self, table_name: &str, persist_moment: DateTimeAsMicroseconds) {
        self.inner
            .lock()
            .persist_table_metadata(table_name, persist_moment);
    }

    /// `now: None` means "give me everything regardless of its due moment" - the
    /// shutdown drain.
    pub fn get_task(&self, now: Option<DateTimeAsMicroseconds>) -> Option<PersistTask> {
        self.inner.lock().get_task(now)
    }

    pub fn has_something_to_persist(&self) -> bool {
        self.inner.lock().has_something_to_persist()
    }

    pub fn get_queue_metrics(&self) -> PersistQueueMetrics {
        self.inner.lock().get_queue_metrics()
    }

    /// Everything queued right now, due or not - see
    /// [`PersistMarkersInner::take_all`].
    pub fn take_all(&self) -> Vec<PersistTask> {
        self.inner.lock().take_all()
    }
}

impl Default for PersistMarkers {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moment(micros: i64) -> DateTimeAsMicroseconds {
        DateTimeAsMicroseconds::new(micros)
    }

    #[test]
    fn nothing_is_given_out_before_it_is_due() {
        let markers = PersistMarkers::new();
        markers.persist_partition("table", "pk", moment(100));

        assert!(markers.get_task(Some(moment(99))).is_none());
        assert!(markers.has_something_to_persist());

        assert_eq!(
            markers.get_task(Some(moment(100))),
            Some(PersistTask::Partition {
                table_name: "table".to_string(),
                partition_key: "pk".to_string(),
            })
        );

        assert!(!markers.has_something_to_persist());
    }

    #[test]
    fn a_more_urgent_write_moves_the_due_moment_earlier() {
        let markers = PersistMarkers::new();

        markers.persist_partition("table", "pk", moment(1000));
        markers.persist_partition("table", "pk", moment(10));
        // The relaxed one must not push it back again.
        markers.persist_partition("table", "pk", moment(5000));

        assert!(markers.get_task(Some(moment(10))).is_some());
    }

    #[test]
    fn table_metadata_goes_before_its_partitions() {
        let markers = PersistMarkers::new();

        markers.persist_partition("table", "pk", moment(10));
        markers.persist_table_metadata("table", moment(10));

        assert_eq!(
            markers.get_task(Some(moment(10))),
            Some(PersistTask::TableMetadata {
                table_name: "table".to_string(),
            })
        );
        assert_eq!(
            markers.get_task(Some(moment(10))),
            Some(PersistTask::Partition {
                table_name: "table".to_string(),
                partition_key: "pk".to_string(),
            })
        );
        assert!(markers.get_task(Some(moment(10))).is_none());
    }

    #[test]
    fn a_flush_takes_everything_at_once_in_the_order_a_pass_would_have() {
        let markers = PersistMarkers::new();

        markers.persist_partition("table", "pk", moment(i64::MAX));
        markers.persist_table_metadata("table", moment(i64::MAX));

        let taken = markers.take_all();

        // Nothing is due, and all of it comes out anyway - that is the whole
        // point of a flush.
        assert_eq!(taken.len(), 2);
        assert_eq!(
            taken[0],
            PersistTask::TableMetadata {
                table_name: "table".to_string(),
            }
        );
        assert_eq!(
            taken[1],
            PersistTask::Partition {
                table_name: "table".to_string(),
                partition_key: "pk".to_string(),
            }
        );

        assert!(!markers.has_something_to_persist());
        assert!(markers.take_all().is_empty());
    }

    /// A flush is a snapshot: what is marked while it runs is the next caller's,
    /// which is what keeps it from chasing a live writer forever.
    #[test]
    fn what_is_marked_after_a_flush_took_the_queue_stays_queued() {
        let markers = PersistMarkers::new();
        markers.persist_partition("table", "first", moment(1));

        assert_eq!(markers.take_all().len(), 1);

        markers.persist_partition("table", "second", moment(1));

        assert!(markers.has_something_to_persist());
        assert_eq!(
            markers.get_task(Some(moment(1))),
            Some(PersistTask::Partition {
                table_name: "table".to_string(),
                partition_key: "second".to_string(),
            })
        );
    }

    #[test]
    fn the_queue_says_how_much_it_owes() {
        let markers = PersistMarkers::new();

        let empty = markers.get_queue_metrics();
        assert_eq!(empty.partitions, 0);
        assert_eq!(empty.tables_metadata, 0);

        markers.persist_partition("one", "pk-1", moment(1));
        markers.persist_partition("one", "pk-2", moment(1));
        // The same partition twice is one thing to write, not two.
        markers.persist_partition("one", "pk-2", moment(1));
        markers.persist_table_metadata("two", moment(1));

        let metrics = markers.get_queue_metrics();

        assert_eq!(metrics.partitions, 2);
        assert_eq!(metrics.tables_metadata, 1);
    }

    #[test]
    fn the_drain_takes_everything_regardless_of_the_due_moment() {
        let markers = PersistMarkers::new();
        markers.persist_partition("table", "pk", moment(i64::MAX));

        assert!(markers.get_task(None).is_some());
        assert!(!markers.has_something_to_persist());
    }

    #[test]
    fn every_marked_partition_is_given_out_once() {
        let markers = PersistMarkers::new();

        for no in 0..10 {
            markers.persist_partition("table", &format!("pk-{no}"), moment(1));
        }

        let mut taken = Vec::new();
        while let Some(task) = markers.get_task(Some(moment(1))) {
            taken.push(task);
        }

        assert_eq!(taken.len(), 10);
        assert!(!markers.has_something_to_persist());
    }
}
