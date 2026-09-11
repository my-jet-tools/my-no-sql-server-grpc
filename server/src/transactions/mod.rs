/// The registry lives in `registry.rs` rather than in a `transactions.rs`: a
/// module of the same name as its parent is a name nobody can write down
/// unambiguously.
mod registry;
pub use registry::*;
mod transaction;
pub use transaction::*;
