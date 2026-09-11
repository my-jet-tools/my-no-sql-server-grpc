use my_no_sql_grpc_core::db::DbTableAttributes;
use tokio::sync::Mutex;

use super::files_repo_inner::FilesRepoInner;
use super::{LoadedPartition, LoadedTableAttrs, TableMetadataFileContract};

/// Slotted-page persistence backend: partitions are stored as self-describing
/// fixed-size slots grouped into per-size-class page-files (`512`, `1024`, ...).
/// Freed slots are self-describing too (`body_len == 0`), so free-lists live only
/// in memory and recovery is a pure scan of the page-files.
pub struct FilesRepo {
    // tokio::Mutex (not parking_lot): every method holds the guard across file
    // I/O `.await`s, which a parking_lot guard can not do. The persist loop runs
    // one task at a time, so there is no real contention.
    inner: Mutex<FilesRepoInner>,
}

impl FilesRepo {
    /// `skip_errors` mirrors `SkipBrokenPartitions`: it decides whether a corrupt
    /// `tables.meta` or a torn slot is skipped or fatal.
    pub async fn open(root: String, skip_errors: bool) -> Self {
        println!("Opening files persistence at: {root}");

        Self {
            inner: Mutex::new(FilesRepoInner::open(root, skip_errors).await),
        }
    }

    pub async fn save_partition(&self, table_name: &str, partition_key: &str, payload: &[u8]) {
        self.inner
            .lock()
            .await
            .save_partition(table_name, partition_key, payload)
            .await;
    }

    pub async fn delete_partition(&self, table_name: &str, partition_key: &str) {
        self.inner
            .lock()
            .await
            .delete_partition(table_name, partition_key)
            .await;
    }

    pub async fn save_table_metadata(&self, table_name: &str, attr: &DbTableAttributes) {
        let contract: TableMetadataFileContract = attr.into();
        self.inner
            .lock()
            .await
            .save_table_metadata(table_name, contract)
            .await;
    }

    pub async fn delete_table_metadata(&self, table_name: &str) {
        self.inner
            .lock()
            .await
            .delete_table_metadata(table_name)
            .await;
    }

    pub async fn get_tables(&self) -> Vec<LoadedTableAttrs> {
        self.inner.lock().await.get_tables()
    }

    pub async fn load_all_partitions(&self, skip_errors: bool) -> Vec<LoadedPartition> {
        self.inner
            .lock()
            .await
            .load_all_partitions(skip_errors)
            .await
    }

    pub async fn delete_everything(&self) -> Result<(), String> {
        self.inner.lock().await.delete_everything().await
    }

    pub async fn vacuum(&self) {
        self.inner.lock().await.vacuum().await;
    }
}
