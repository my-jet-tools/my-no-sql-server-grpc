use std::sync::Arc;

use arc_swap::ArcSwap;
use parking_lot::Mutex;
use rust_extensions::sorted_vec::SortedVecOfArcWithStrKey;

use super::DbTable;

struct DbInstanceInner {
    sorted: SortedVecOfArcWithStrKey<DbTable>,
    /// The same tables as a ready-made vector, so "give me every table" is an
    /// `Arc` clone instead of an allocation on a path the metrics and the
    /// persist loop walk constantly.
    as_vec: Arc<Vec<Arc<DbTable>>>,
}

impl DbInstanceInner {
    fn empty() -> Self {
        Self {
            sorted: SortedVecOfArcWithStrKey::new(),
            as_vec: Arc::new(Vec::new()),
        }
    }

    fn from_sorted(sorted: SortedVecOfArcWithStrKey<DbTable>) -> Self {
        let as_vec = Arc::new(sorted.iter().cloned().collect());
        Self { sorted, as_vec }
    }
}

/// The tables of one namespace.
///
/// Tables are looked up on every single request and created a handful of times
/// in the life of the process, so the list is copy-on-write behind an `ArcSwap`:
/// a reader takes no lock at all, and the small mutex only keeps two concurrent
/// writers from losing each other's update.
pub struct DbInstance {
    inner: ArcSwap<DbInstanceInner>,
    write_lock: Mutex<()>,
}

impl DbInstance {
    pub fn new() -> Self {
        Self {
            inner: ArcSwap::from_pointee(DbInstanceInner::empty()),
            write_lock: Mutex::new(()),
        }
    }

    pub fn get_table(&self, table_name: &str) -> Option<Arc<DbTable>> {
        self.inner.load().sorted.get(table_name).cloned()
    }

    pub fn get_tables(&self) -> Arc<Vec<Arc<DbTable>>> {
        self.inner.load().as_vec.clone()
    }

    pub fn has_table(&self, table_name: &str) -> bool {
        self.inner.load().sorted.contains(table_name)
    }

    pub fn insert(&self, table: Arc<DbTable>) {
        let _guard = self.write_lock.lock();

        let mut sorted = self.inner.load().sorted.clone();
        sorted.insert_or_replace(table);

        self.inner
            .store(Arc::new(DbInstanceInner::from_sorted(sorted)));
    }

    /// `created` is false when the table was already there and the factory was
    /// never called.
    pub fn get_or_create<TFactory: FnOnce() -> Arc<DbTable>>(
        &self,
        table_name: &str,
        factory: TFactory,
    ) -> GetOrCreateTableResult {
        if let Some(table) = self.get_table(table_name) {
            return GetOrCreateTableResult {
                table,
                created: false,
            };
        }

        let _guard = self.write_lock.lock();

        // Somebody could have created it between the check above and the lock.
        if let Some(table) = self.get_table(table_name) {
            return GetOrCreateTableResult {
                table,
                created: false,
            };
        }

        let table = factory();

        let mut sorted = self.inner.load().sorted.clone();
        sorted.insert_or_replace(table.clone());
        self.inner
            .store(Arc::new(DbInstanceInner::from_sorted(sorted)));

        GetOrCreateTableResult {
            table,
            created: true,
        }
    }

    pub fn remove(&self, table_name: &str) -> Option<Arc<DbTable>> {
        let _guard = self.write_lock.lock();

        let mut sorted = self.inner.load().sorted.clone();
        let removed = sorted.remove(table_name)?;

        self.inner
            .store(Arc::new(DbInstanceInner::from_sorted(sorted)));

        Some(removed)
    }
}

impl Default for DbInstance {
    fn default() -> Self {
        Self::new()
    }
}

pub struct GetOrCreateTableResult {
    pub table: Arc<DbTable>,
    pub created: bool,
}
