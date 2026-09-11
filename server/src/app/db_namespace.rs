use std::sync::atomic::{AtomicU64, Ordering};

use my_no_sql_grpc_core::db::DbInstance;
use rust_extensions::date_time::{AtomicDateTimeAsMicroseconds, DateTimeAsMicroseconds};

use crate::persist::PersistRepo;
use crate::persist::markers::PersistMarkers;
use crate::settings_reader::SettingsModel;

/// A namespace owns its tables and its folder on disk. Two namespaces may hold a
/// table of the same name and they share nothing.
///
/// The schemas are not here: they belong to the table whose rows were written
/// under them, and they are stored where that table's other metadata is.
pub struct DbNamespace {
    pub name: String,
    pub tables: DbInstance,
    pub persist_repo: PersistRepo,
    pub persist_markers: PersistMarkers,
    /// When this namespace last wrote something to disk, `0` meaning "not since
    /// the process started". Together with the queue depth it is the whole
    /// answer to "is the disk keeping up": a backlog which is not moving has a
    /// last-persisted moment which is not moving either.
    last_persisted: AtomicDateTimeAsMicroseconds,
    persisted_total: AtomicU64,
}

impl DbNamespace {
    pub async fn open(name: String, settings: &SettingsModel) -> Self {
        let folder =
            crate::persist::layout::get_namespace_folder(&settings.get_persistence_dest(), &name);

        let persist_repo = PersistRepo::open(
            folder,
            settings.skip_broken_partitions,
            settings.compress_data,
        )
        .await;

        Self {
            name,
            tables: DbInstance::new(),
            persist_repo,
            persist_markers: PersistMarkers::new(),
            last_persisted: AtomicDateTimeAsMicroseconds::new(0),
            persisted_total: AtomicU64::new(0),
        }
    }

    pub fn persisted(&self, now: DateTimeAsMicroseconds, tasks: usize) {
        self.last_persisted.update(now);
        self.persisted_total
            .fetch_add(tasks as u64, Ordering::Relaxed);
    }

    /// How many persist tasks this namespace has written since the process
    /// started. Paired with the queue depth it is the whole answer to "is the
    /// disk keeping up": a backlog which is not moving has a total which is not
    /// moving either.
    pub fn get_persisted_total(&self) -> u64 {
        self.persisted_total.load(Ordering::Relaxed)
    }

    pub fn get_last_persisted(&self) -> Option<DateTimeAsMicroseconds> {
        let result = self.last_persisted.as_date_time();

        if result.unix_microseconds == 0 {
            return None;
        }

        Some(result)
    }
}
