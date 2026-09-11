//! The macro which declares an entity of this server.
//!
//! It **rewrites** the struct rather than adding an impl beside it: the four
//! fields the contract reserves are put in front of whatever the application
//! declared, so `PartitionKey = 1`, `RowKey = 2`, `TimeStamp = 3` and
//! `Expires = 4` are not something an application can get wrong, forget, or
//! number differently. That is the whole reason it is an attribute macro and
//! not a derive - a derive can only look at a struct, it can not put anything
//! into it.
//!
//! ```ignore
//! #[my_no_sql_entity(table_name: "traders")]
//! pub struct TraderEntity {
//!     #[proto_no(5)]
//!     pub amount: f64,
//!     #[proto_no(6)]
//!     pub instrument: String,
//!     /// No `proto_no`, so it is never written and never read - an ordinary
//!     /// field of an ordinary struct.
//!     pub computed_locally: u64,
//! }
//! ```
//!
//! Field numbers are written down rather than counted off the declaration
//! order: the number is what a stored row is read back by, so it has to survive
//! somebody adding a field in the middle.
//!
//! # Nothing here may carry itself
//!
//! Both macros emit `const SCHEMA_ID: u64`, folded out of the type's own fields
//! and the `SCHEMA_ID` of every message they carry. A `const` can not name
//! itself, so a struct which carries itself and two which carry each other are
//! both refused - deliberately, and by the project owner's decision: a row of a
//! NoSQL table is a flat record, and a tree is modelled with a second table or
//! stored as a `Vec<u8>`.
//!
//! A macro sees **one struct at a time**, so only the direct case is visible
//! here, and that one is refused with a message naming the type. The mutual
//! case, where `A` carries `B` which carries `A`, is not visible from any single
//! expansion, and what catches it is the constant itself: rustc reports `cycle
//! detected when evaluating` on `SCHEMA_ID`. That message is hard to connect to
//! the declaration that caused it, which is why it is written down here rather
//! than left to be discovered.

mod codegen;
mod entity;
mod field_type;
mod fields;
mod message;

use proc_macro::TokenStream;

/// Declares an entity. Takes `table_name: "..."` and nothing else.
#[proc_macro_attribute]
pub fn my_no_sql_entity(attr: TokenStream, item: TokenStream) -> TokenStream {
    match entity::generate(attr.into(), item.into()) {
        Ok(result) => result.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Declares a message an entity's field can carry.
///
/// It takes no arguments: a message has no table and no schema of its own - it
/// is declared inside the schema of whichever entity carries it.
///
/// ```ignore
/// #[my_no_sql_message]
/// pub struct Limits {
///     #[proto_no(1)]
///     pub max_lots: f64,
///     #[proto_no(2)]
///     pub instruments: Vec<String>,
/// }
///
/// #[my_no_sql_entity(table_name: "traders")]
/// pub struct TraderEntity {
///     #[proto_no(5)]
///     pub limits: Limits,
///     #[proto_no(6)]
///     pub history: Vec<Limits>,
/// }
/// ```
///
/// Numbering starts at 1, not at 5: the four the entity contract reserves are
/// the entity's own, and a message carries no keys.
#[proc_macro_attribute]
pub fn my_no_sql_message(_attr: TokenStream, item: TokenStream) -> TokenStream {
    match message::generate(item.into()) {
        Ok(result) => result.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
