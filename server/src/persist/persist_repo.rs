use std::sync::Arc;

use my_no_sql_grpc_core::db::{DbRow, DbTableAttributes};

use super::{FilesRepo, LoadedPartition, LoadedTableAttrs, partition_blob};

/// Everything one namespace keeps on disk: its page-files and its `tables.meta`.
pub struct PersistRepo {
    files_repo: FilesRepo,
    compress_data: bool,
}

impl PersistRepo {
    pub async fn open(folder: String, skip_errors: bool, compress_data: bool) -> Self {
        Self {
            files_repo: FilesRepo::open(folder, skip_errors).await,
            compress_data,
        }
    }

    pub async fn save_partition(
        &self,
        table_name: &str,
        partition_key: &str,
        db_rows: &[Arc<DbRow>],
    ) {
        let payload = partition_blob::serialize(db_rows, self.compress_data);
        self.files_repo
            .save_partition(table_name, partition_key, &payload)
            .await;
    }

    pub async fn delete_partition(&self, table_name: &str, partition_key: &str) {
        self.files_repo
            .delete_partition(table_name, partition_key)
            .await;
    }

    pub async fn save_table_metadata(&self, table_name: &str, attr: &DbTableAttributes) {
        self.files_repo.save_table_metadata(table_name, attr).await;
    }

    pub async fn delete_table_metadata(&self, table_name: &str) {
        self.files_repo.delete_table_metadata(table_name).await;
    }

    pub async fn get_tables(&self) -> Vec<LoadedTableAttrs> {
        self.files_repo.get_tables().await
    }

    pub async fn load_all_partitions(&self, skip_errors: bool) -> Vec<LoadedPartition> {
        self.files_repo.load_all_partitions(skip_errors).await
    }

    /// Gets the backend ready to be written into by a namespace which is **not**
    /// going through the start up load - one created while the server is already
    /// running. The folder may well hold page-files already (a namespace deleted
    /// and made again, a folder which outlived the process that wrote it), and
    /// only the scan rebuilds the key index and the free-lists and seeds the
    /// version counter past every slot already there. Writing without it would
    /// hand out versions BELOW the ones on disk, and the higher-version-wins
    /// dedup of the next start would then prefer the old slots - reverting
    /// everything written since.
    ///
    /// The start up path must not call this: its own load is the very same scan.
    pub async fn prime_for_writes(&self, skip_errors: bool) {
        let _ = self.files_repo.load_all_partitions(skip_errors).await;
    }

    /// Everything this namespace kept, gone: its page-files and its
    /// `tables.meta`. Only a namespace which has been removed from the server
    /// calls this - there is no coming back from it.
    pub async fn delete_everything(&self) -> Result<(), String> {
        self.files_repo.delete_everything().await
    }

    pub async fn vacuum(&self) {
        self.files_repo.vacuum().await;
    }
}
