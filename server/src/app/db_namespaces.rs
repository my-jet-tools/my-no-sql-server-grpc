use std::sync::Arc;

use ahash::AHashMap;
use arc_swap::ArcSwap;

use crate::db_operations::DbOperationError;
use crate::settings_reader::SettingsModel;

use super::DbNamespace;

/// Every namespace of the server. Resolved on every single request and created a
/// handful of times in the life of the process, so it is copy-on-write behind an
/// `ArcSwap` - a lookup takes no lock.
pub struct DbNamespaces {
    inner: ArcSwap<AHashMap<String, Arc<DbNamespace>>>,
    // tokio::Mutex, not parking_lot: creating a namespace opens its persistence
    // folder, so the guard is held across file I/O awaits. Only the create path
    // touches it - lookups never do.
    write_lock: tokio::sync::Mutex<()>,
}

impl DbNamespaces {
    pub fn new() -> Self {
        Self {
            inner: ArcSwap::from_pointee(AHashMap::new()),
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// An empty name means the default namespace - that is what a client which
    /// does not care about namespaces sends.
    pub fn resolve_name(name: &str) -> &str {
        if name.is_empty() {
            return crate::consts::DEFAULT_NAMESPACE;
        }

        name
    }

    pub fn get(&self, name: &str) -> Option<Arc<DbNamespace>> {
        let name = Self::resolve_name(name);

        // Refused here as well as on the create path, so resolution stays
        // symmetric: a name this server would never have created is a name it
        // never answers to either, however the entry got into the map.
        if !crate::persist::layout::is_valid_namespace_name(name) {
            return None;
        }

        self.inner.load().get(name).cloned()
    }

    /// The namespace a request names, brought into existence if it is the first
    /// time anybody named it.
    ///
    /// **This is where the name stops being a string and becomes a path**, so it
    /// is validated here rather than at each call site: the folder is created
    /// from it and, on a delete, removed recursively by it. The benign shapes
    /// matter as much as the hostile ones - a name with a space in it makes a
    /// folder the next start skips, so every row written into it becomes
    /// unreachable without anybody being told.
    pub async fn get_or_create(
        &self,
        name: &str,
        settings: &SettingsModel,
    ) -> Result<Arc<DbNamespace>, DbOperationError> {
        let name = Self::resolve_name(name);

        if !crate::persist::layout::is_valid_namespace_name(name) {
            return Err(DbOperationError::InvalidNamespaceName(name.to_string()));
        }

        if let Some(namespace) = self.get(name) {
            return Ok(namespace);
        }

        let _guard = self.write_lock.lock().await;

        // Somebody could have created it between the check above and the lock.
        if let Some(namespace) = self.get(name) {
            return Ok(namespace);
        }

        let namespace = Arc::new(DbNamespace::open(name.to_string(), settings).await);

        // The folder is not necessarily empty - the server is already running,
        // so this one has no start up load behind it to scan the page-files.
        namespace
            .persist_repo
            .prime_for_writes(settings.skip_broken_partitions)
            .await;

        Ok(self.insert(name, namespace))
    }

    /// The start up path: the name comes from a folder which is already there,
    /// and the caller loads the namespace right after. That load is the scan
    /// which seeds the index, the free-lists and the version counter, so this
    /// one deliberately does not prime - it would read every page-file twice.
    pub async fn open_on_start_up(&self, name: &str, settings: &SettingsModel) -> Arc<DbNamespace> {
        let _guard = self.write_lock.lock().await;

        let namespace = Arc::new(DbNamespace::open(name.to_string(), settings).await);

        self.insert(name, namespace)
    }

    fn insert(&self, name: &str, namespace: Arc<DbNamespace>) -> Arc<DbNamespace> {
        let mut map = self.inner.load().as_ref().clone();
        map.insert(name.to_string(), namespace.clone());
        self.inner.store(Arc::new(map));

        namespace
    }

    pub fn get_all(&self) -> Vec<Arc<DbNamespace>> {
        self.inner.load().values().cloned().collect()
    }

    /// Forgets a namespace. Its folder on disk is the caller's business - this
    /// only stops the server from resolving the name.
    pub async fn remove(&self, name: &str) -> Option<Arc<DbNamespace>> {
        let name = Self::resolve_name(name);

        let _guard = self.write_lock.lock().await;

        let mut map = self.inner.load().as_ref().clone();
        let removed = map.remove(name)?;
        self.inner.store(Arc::new(map));

        Some(removed)
    }
}

impl Default for DbNamespaces {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static FOLDER_NO: AtomicU64 = AtomicU64::new(0);

    /// A folder of its own per test, nested one level below the temp directory,
    /// so a name which does escape the persistence root escapes into somewhere
    /// this test can look at and then clean up.
    fn new_test_base() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "my-no-sql-grpc-namespaces-{}-{}",
            std::process::id(),
            FOLDER_NO.fetch_add(1, Ordering::SeqCst)
        ));

        let _ = std::fs::remove_dir_all(&base);
        base
    }

    fn settings(persistence_dest: &std::path::Path) -> SettingsModel {
        SettingsModel {
            persistence_dest: persistence_dest.to_string_lossy().to_string(),
            location: "test".to_string(),
            compress_data: false,
            skip_broken_partitions: false,
            backups_dest: None,
            backup_interval_secs: None,
            max_backups: None,
            api_key: None,
        }
    }

    #[tokio::test]
    async fn a_namespace_named_by_a_request_never_becomes_a_folder_outside_the_root() {
        let base = new_test_base();
        let root = base.join("persistence");
        let settings = settings(&root);

        let namespaces = DbNamespaces::new();

        for name in ["../escaped", "..", ".", "with space", &"a".repeat(65)] {
            let result = namespaces.get_or_create(name, &settings).await;

            assert!(
                matches!(result, Err(DbOperationError::InvalidNamespaceName(_))),
                "'{name}' was accepted as a namespace name"
            );

            // And a name which can not be created can not be resolved either.
            assert!(namespaces.get(name).is_none(), "'{name}' resolved");
        }

        assert!(
            !base.join("escaped").exists(),
            "a namespace name walked out of the persistence root"
        );

        // The name which is one still works, folder and all.
        namespaces
            .get_or_create("archive", &settings)
            .await
            .unwrap();
        assert!(root.join("archive").is_dir());

        let _ = std::fs::remove_dir_all(&base);
    }
}
