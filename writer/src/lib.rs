//! The client side of the `Writer` contract.
//!
//! An entity is an ordinary protobuf message whose first fields are the ones the
//! contract reserves - `PartitionKey = 1`, `RowKey = 2`, `TimeStamp = 3`,
//! `Expires = 4` - with everything of its own from 5. The server never needs the
//! schema to find those four; the schema travels along only so a stored row can
//! be *shown* under its own field names.
//!
//! ```ignore
//! let connection = MyNoSqlGrpcConnection::new("http://127.0.0.1:5124")?;
//! let writer: MyNoSqlGrpcWriter<TraderEntity> = MyNoSqlGrpcWriter::new(connection);
//!
//! writer.create_table_if_not_exists(TableAttributesGrpcModel {
//!     persist: true,
//!     ..Default::default()
//! }).await?;
//!
//! writer.insert_or_replace(&trader).await?;
//!
//! let mut transaction = writer.begin_transaction().await?;
//! transaction.delete_partitions(vec!["acc-1".to_string()]);
//! transaction.insert_or_replace(&fresh_rows);
//! transaction.commit().await?;
//! ```

mod connection;
pub use connection::*;
mod cut_rows;
pub use cut_rows::{MAX_MESSAGE_SIZE, MAX_ROWS_PER_MESSAGE};
pub(crate) use cut_rows::{cut_rows, cut_rows_at_least_once};
mod entity;
pub use entity::*;
/// Declared once in the core, so an application does not describe its entity
/// twice to write it and to read it.
pub use my_no_sql_grpc_core::MyNoSqlEntity;
mod error;
pub use error::*;
mod transaction;
pub use transaction::*;
mod writer;
pub use writer::*;

pub mod my_no_sql_writer_grpc {
    tonic::include_proto!("my_no_sql_writer");
}

/// The parts of the contract a caller actually names.
pub use my_no_sql_writer_grpc::{
    BulkWriteModeGrpcModel, SyncPeriodGrpcModel, TableAttributesGrpcModel, TableGrpcModel,
};
