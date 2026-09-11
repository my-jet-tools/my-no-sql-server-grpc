use my_no_sql_grpc_core::db_entity::DbEntityParseFail;

#[derive(Debug)]
pub enum DbOperationError {
    /// The server is still loading its tables from disk.
    NotInitialized,
    NamespaceNotFound(String),
    /// The name arrived from a caller and would have become a folder inside the
    /// persistence root.
    InvalidNamespaceName(String),
    /// A client which names no namespace has to land somewhere, so the default
    /// one is not a namespace like the others.
    DefaultNamespaceCanNotBeDeleted,
    /// The namespace is gone from memory but its folder is still on disk.
    /// Nothing retries it - the namespace nobody can reach any more is nobody's
    /// queue - so the caller is the only one who can be told.
    NamespaceFolderNotDeleted(String),
    TableNotFound(String),
    /// Told apart from a missing row on purpose: whoever is asking where their
    /// row went is told whether the partition holding it is gone as well.
    PartitionNotFound(String),
    TableAlreadyExists(String),
    RowNotFound,
    RowAlreadyExists,
    /// A `Replace` whose row has been rewritten since the caller read it. Told
    /// apart from every other refusal because it is the one the caller is
    /// supposed to answer by reading the row again and repeating the write -
    /// the read-modify-write loop is built on exactly this reply.
    OptimisticConcurrencyUpdateFails,
    EntityParseFail(DbEntityParseFail),
    /// Something about a backup did not work - it is not configured, the file is
    /// not there, or it is not a backup of this server.
    BackupFailed(String),
    /// The other server could not be reached, or did not answer with a table.
    MigrationFailed(String),
}

impl From<DbEntityParseFail> for DbOperationError {
    fn from(src: DbEntityParseFail) -> Self {
        Self::EntityParseFail(src)
    }
}

impl std::fmt::Display for DbOperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbOperationError::NotInitialized => {
                write!(f, "The server is not initialized yet")
            }
            DbOperationError::NamespaceNotFound(namespace) => {
                write!(f, "Namespace '{namespace}' is not found")
            }
            DbOperationError::InvalidNamespaceName(namespace) => {
                write!(
                    f,
                    "'{namespace}' is not a namespace name: up to 64 letters, digits, '-', '_' and '.', and not dots alone"
                )
            }
            DbOperationError::DefaultNamespaceCanNotBeDeleted => {
                write!(
                    f,
                    "The default namespace can not be deleted - it is where every write which names no namespace lands"
                )
            }
            DbOperationError::NamespaceFolderNotDeleted(err) => write!(f, "{err}"),
            DbOperationError::TableNotFound(table_name) => {
                write!(f, "Table '{table_name}' is not found")
            }
            DbOperationError::PartitionNotFound(partition_key) => {
                write!(f, "Partition '{partition_key}' is not found")
            }
            DbOperationError::TableAlreadyExists(table_name) => {
                write!(f, "Table '{table_name}' already exists")
            }
            DbOperationError::RowNotFound => write!(f, "Row is not found"),
            DbOperationError::RowAlreadyExists => write!(f, "Row already exists"),
            DbOperationError::OptimisticConcurrencyUpdateFails => write!(
                f,
                "The row has been changed since it was read - read it again and repeat the write"
            ),
            DbOperationError::EntityParseFail(err) => write!(f, "{err}"),
            DbOperationError::BackupFailed(err) => write!(f, "{err}"),
            DbOperationError::MigrationFailed(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for DbOperationError {}
