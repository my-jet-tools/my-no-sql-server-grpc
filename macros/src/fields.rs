//! What the two macros share: reading `#[proto_no(..)]` off a struct's fields
//! and the small spelling rules that go with it.

use std::collections::HashSet;

use syn::{Attribute, Error, Fields, ItemStruct, LitInt, Result};

use crate::codegen::EntityField;
use crate::field_type::{self, FieldKind};

/// Reads the fields the application declared, keeping only the ones it asked to
/// store.
///
/// `first_field_no` is where this kind of struct is allowed to start - an entity
/// starts at 5 because 1..4 are the contract's, a message starts at 1 because it
/// has no contract of its own. `reserved_names` are the names it may not
/// declare, for the same reason.
pub fn read_fields(
    source: &ItemStruct,
    what: &str,
    first_field_no: u32,
    reserved_names: &[(&str, &str)],
) -> Result<Vec<EntityField>> {
    // Said out loud rather than left to the expansion. Both macros emit a plain
    // `struct Name { .. }`, so type parameters, lifetimes and a where-clause are
    // dropped on the way through, and what the caller then gets is a trait error
    // about a struct they did not write. And there is nothing to fix by carrying
    // them: the schema is a static of the type and the table name a constant of
    // it, so one descriptor per instantiation is a thing the contract has no
    // place for.
    if !source.generics.params.is_empty() || source.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            source,
            format!(
                "{what} can not be generic: the macro declares a plain struct, and its schema is a static of the type, so type parameters, lifetimes and a where-clause have nowhere to go. Declare it with the concrete types in place"
            ),
        ));
    }

    let Fields::Named(named) = &source.fields else {
        return Err(Error::new_spanned(
            source,
            format!("{what} is a struct with named fields"),
        ));
    };

    let declared_name = source.ident.to_string();

    let mut result = Vec::new();
    let mut taken: HashSet<u32> = HashSet::new();

    for field in named.named.iter() {
        let ident = field.ident.clone().unwrap();

        if reserved_names.iter().any(|(reserved, _)| ident == reserved) {
            return Err(Error::new_spanned(
                &field.ident,
                format!(
                    "'{ident}' is one of the four fields the contract reserves - the macro puts it into the struct itself"
                ),
            ));
        }

        let Some(number) = read_proto_no(&field.attrs)? else {
            continue;
        };

        if number < first_field_no {
            return Err(Error::new_spanned(
                field,
                format!(
                    "proto_no({number}) is reserved: 1..4 are PartitionKey, RowKey, TimeStamp and Expires, so an entity starts at {first_field_no}"
                ),
            ));
        }

        if !taken.insert(number) {
            return Err(Error::new_spanned(
                field,
                format!("proto_no({number}) is used by another field of this {what}"),
            ));
        }

        let proto = field_type::resolve(&field.ty).map_err(|err| Error::new_spanned(field, err))?;

        if let FieldKind::Message(carried) = &proto.kind
            && field_type::type_name(carried).as_deref() == Some(declared_name.as_str())
        {
            return Err(Error::new_spanned(field, recursion_message(&declared_name)));
        }

        result.push(EntityField {
            proto_name: to_pascal_case(&ident.to_string()),
            ident,
            number,
            proto,
        });
    }

    Ok(result)
}

/// A struct which carries itself, caught here so that rustc does not have to
/// catch it as `cycle detected when evaluating`.
///
/// The id is a `const`, and a `const` can not name itself: a message carrying
/// itself would need `SCHEMA_ID` to be an input to its own definition. That is
/// the deliberate cost of computing the id at compile time, and it is only the
/// direct case that is visible from here - see the module docs of this crate for
/// the one the const cycle catches instead.
fn recursion_message(declared_name: &str) -> String {
    format!(
        "'{declared_name}' carries itself, and it can not: its SCHEMA_ID is a compile-time constant folded out of the types its fields carry, so a type among them would have to be an input to its own definition. A row of a NoSQL table is a flat record - model a tree with a second table, or store it as a Vec<u8>"
    )
}

pub fn read_proto_no(attrs: &[Attribute]) -> Result<Option<u32>> {
    for attr in attrs {
        if !attr.path().is_ident("proto_no") {
            continue;
        }

        let number: LitInt = attr.parse_args()?;
        return Ok(Some(number.base10_parse()?));
    }

    Ok(None)
}

pub fn derives_default(attrs: &[Attribute]) -> bool {
    let mut result = false;

    for attr in attrs {
        if !attr.path().is_ident("derive") {
            continue;
        }

        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("Default") {
                result = true;
            }

            Ok(())
        });
    }

    result
}

/// `instrument_id` becomes `InstrumentId`. The four reserved fields are spelled
/// that way by the contract, and one JSON object with two spellings of the same
/// convention in it reads like a bug.
pub fn to_pascal_case(src: &str) -> String {
    let mut result = String::with_capacity(src.len());
    let mut capitalize = true;

    for symbol in src.chars() {
        if symbol == '_' {
            capitalize = true;
            continue;
        }

        if capitalize {
            result.extend(symbol.to_uppercase());
            capitalize = false;
        } else {
            result.push(symbol);
        }
    }

    result
}
