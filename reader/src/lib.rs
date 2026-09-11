//! The client side of the `Reader` contract.
//!
//! It keeps an in-process image of the tables it subscribed to and answers from
//! that image, so reading costs nothing and never leaves the process. The image
//! is a `DbTable` from the core - the very structure the server keeps - because
//! that is exactly what a reader's cache is.
//!
//! ```ignore
//! let reader = MyNoSqlGrpcReader::new("http://127.0.0.1:5124", "my-app", "1.0.0")?;
//! let traders = reader.subscribe::<TraderEntity>();
//! reader.start();
//!
//! traders.wait_until_initialized().await;
//! let trader = traders.get_row("acc-1", "eur-usd")?;
//! ```

mod error;
pub use error::*;
mod reader;
pub use reader::*;
mod session;
pub(crate) use session::read_loop;
mod table_cache;
pub use table_cache::*;

/// Declared once in the core, so an application does not describe its entity
/// twice to write it and to read it.
pub use my_no_sql_grpc_core::MyNoSqlEntity;

pub mod my_no_sql_reader_grpc {
    tonic::include_proto!("my_no_sql_reader");
}
