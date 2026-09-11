/// What can go wrong while a session is being held.
///
/// Every one of these means the same thing to the reader - drop the session and
/// start over - which is why the loop never tells them apart. They are separate
/// only so a log line says what happened.
#[derive(Debug)]
pub enum MyNoSqlReaderError {
    InvalidUrl(String),
    Grpc(tonic::Status),
    /// A row arrived which does not parse as an entity at all. The server only
    /// ever sends back what it was given, so this is a torn message rather than
    /// a schema mismatch.
    CanNotParseRow(String),
}

impl From<tonic::Status> for MyNoSqlReaderError {
    fn from(src: tonic::Status) -> Self {
        Self::Grpc(src)
    }
}

impl std::fmt::Display for MyNoSqlReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MyNoSqlReaderError::InvalidUrl(url) => {
                write!(f, "'{url}' is not a url a gRPC endpoint can be built from")
            }
            MyNoSqlReaderError::Grpc(status) => write!(
                f,
                "MyNoSqlServer answered {}: {}",
                status.code(),
                status.message()
            ),
            MyNoSqlReaderError::CanNotParseRow(err) => {
                write!(f, "A row which arrived is not a readable entity: {err}")
            }
        }
    }
}

impl std::error::Error for MyNoSqlReaderError {}
