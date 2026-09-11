use my_no_sql_grpc_abstractions::schemas::EntitySchema;

use crate::data_sync_period::DataSyncPeriod;

/// A write as the database sees it - the transport is already gone by this
/// point.
pub struct WriteRowRequest {
    /// Travels with every entity. The server keeps it only the first time it
    /// meets this id.
    pub schema: EntitySchema,
    /// The entity as the client serialized it.
    pub row: Vec<u8>,
    /// `true` - keep the TimeStamp the entity carries instead of stamping the
    /// server's clock.
    pub use_client_time_stamp: bool,
    pub sync_period: DataSyncPeriod,
}
