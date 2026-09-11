/// Everything that can go wrong on the way to the server and back.
#[derive(Debug)]
pub enum MyNoSqlWriterError {
    InvalidUrl(String),
    /// What the server answered. The code carries the meaning: `NotFound` for a
    /// missing table or row, `AlreadyExists` for an insert over a stored row,
    /// `Unavailable` while the server is still loading from disk.
    Grpc(tonic::Status),
    /// A row came back which this build can not read as its own entity - the
    /// stored row was written under a schema this binary does not have.
    CanNotDecodeEntity(my_no_sql_grpc_core::db_entity::DbEntityParseFail),
}

impl MyNoSqlWriterError {
    /// `true` when the server said the row or the table is not there. It is the
    /// one status a caller routinely wants to tell apart from a failure.
    pub fn is_not_found(&self) -> bool {
        match self {
            MyNoSqlWriterError::Grpc(status) => status.code() == tonic::Code::NotFound,
            _ => false,
        }
    }

    pub fn is_already_exists(&self) -> bool {
        match self {
            MyNoSqlWriterError::Grpc(status) => status.code() == tonic::Code::AlreadyExists,
            _ => false,
        }
    }

    /// `true` when a `replace` lost its optimistic-concurrency check: the row
    /// was rewritten between the read it was built on and the write. It is the
    /// one failure a caller is meant to answer by reading again and repeating,
    /// so the retry loop needs to be able to tell it from everything else.
    pub fn is_conflict(&self) -> bool {
        match self {
            MyNoSqlWriterError::Grpc(status) => status.code() == tonic::Code::Aborted,
            _ => false,
        }
    }
}

impl From<tonic::Status> for MyNoSqlWriterError {
    fn from(src: tonic::Status) -> Self {
        Self::Grpc(src)
    }
}

impl From<my_no_sql_grpc_core::db_entity::DbEntityParseFail> for MyNoSqlWriterError {
    fn from(src: my_no_sql_grpc_core::db_entity::DbEntityParseFail) -> Self {
        Self::CanNotDecodeEntity(src)
    }
}

impl std::fmt::Display for MyNoSqlWriterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MyNoSqlWriterError::InvalidUrl(url) => {
                write!(f, "'{url}' is not a url a gRPC endpoint can be built from")
            }
            MyNoSqlWriterError::Grpc(status) => write!(
                f,
                "MyNoSqlServer answered {}: {}",
                status.code(),
                status.message()
            ),
            MyNoSqlWriterError::CanNotDecodeEntity(err) => {
                write!(f, "A stored row does not decode into this entity: {err}")
            }
        }
    }
}

impl std::error::Error for MyNoSqlWriterError {}
