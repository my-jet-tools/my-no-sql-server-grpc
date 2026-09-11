//! JSON in, a stored row out - the other direction of [`super::row_to_json`].
//!
//! It exists for one caller: the MCP surface, where a person describes a row in
//! words and the agent hands over JSON. Everywhere else an entity arrives
//! already serialized, carrying the schema it was serialized under, and the
//! server never has to know its shape to store it.
//!
//! Here it does, and the schema it uses is one the **table already holds** -
//! registered by whichever client writes this entity for real. Nothing new is
//! registered from this path: a schema invented at the keyboard would be a
//! second shape under an id the client folded out of its own type, which is
//! exactly what `build_db_row::register_schema` refuses.

use my_json::json_reader::{JsonFirstLineIterator, JsonValueRef};
use my_no_sql_grpc_core::db_entity::{
    consts, moment_is_in_range, write_i32_field, write_i64_field, write_len_field,
    write_varint_field,
};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::base64;
use super::{FieldKind, SchemaIndex};

/// The two fields the server owns. They are not in the schema - they are not in
/// a stored row either, the server writes them on the way out - so they are
/// matched here by the names the contract gives them, which is the same
/// spelling the renderer falls back to.
const TIME_STAMP: &str = "TimeStamp";
const EXPIRES: &str = "Expires";

/// The same cap the renderer has, for the same reason: this walks the **data**,
/// one stack frame per level, and a stack overflow is an abort rather than an
/// error anybody could be told about.
const MAX_NESTING_DEPTH: usize = 100;

/// Turns a JSON object into the bytes of an entity of `schema`.
///
/// Every rule here is chosen so that a caller is told when it is wrong:
///
/// - **a name the schema does not have is refused**, with the names it does
///   have. A row accepted with a misspelled field would be a row stored without
///   that value and an `ok` saying otherwise;
/// - **`TimeStamp` is accepted and ignored.** It is what the renderer shows, so
///   a row read out and handed straight back carries one; and the server stamps
///   its own clock on every write regardless, so honouring it would be a lie
///   either way;
/// - **`Expires` is accepted**, as the RFC3339 the renderer shows or as `null`
///   for "never". It is the one server-owned field a caller has a reason to set;
/// - a field which is `null` is left out, which in proto3 is what "not set"
///   means.
pub fn write_row_from_json(json: &[u8], schema: &SchemaIndex) -> Result<Vec<u8>, String> {
    let root = schema.get_root_message_name().to_string();

    let mut result = Vec::new();
    write_message(&mut result, json, schema, &root, 0)?;

    Ok(result)
}

fn write_message(
    dest: &mut Vec<u8>,
    json: &[u8],
    schema: &SchemaIndex,
    message_name: &str,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_NESTING_DEPTH {
        return Err(format!(
            "the value is nested deeper than {MAX_NESTING_DEPTH} messages"
        ));
    }

    let Some(message) = schema.get_message(message_name) else {
        // Every reference inside a schema is resolved when the index is built,
        // so this is only reachable for the root of a schema which named a
        // message it does not carry.
        return Err(format!("the schema has no message '{message_name}'"));
    };

    let reader = JsonFirstLineIterator::new(json);

    while let Some(next) = reader.get_next() {
        let (name, value) =
            next.map_err(|err| format!("{message_name} is not a JSON object: {err:?}"))?;

        let name = name.as_str().map_err(|err| format!("{err:?}"))?;
        let name = name.as_str();

        if depth == 0 && (name == TIME_STAMP || name == EXPIRES) {
            write_server_owned_field(dest, name, &value)?;
            continue;
        }

        let Some((no, field)) = message.get_field_by_name(name) else {
            return Err(format!(
                "'{message_name}' has no field '{name}'. It has: {}",
                message.get_field_names().join(", ")
            ));
        };

        if value.is_null() {
            continue;
        }

        if field.repeated {
            let items = value
                .unwrap_as_array()
                .map_err(|_| format!("'{name}' of '{message_name}' is not an array"))?;

            while let Some(item) = items.get_next() {
                let item = item.map_err(|err| format!("'{name}' of '{message_name}': {err:?}"))?;
                write_value(
                    dest,
                    no,
                    &field.kind,
                    &item,
                    schema,
                    depth,
                    name,
                    message_name,
                )?;
            }

            continue;
        }

        write_value(
            dest,
            no,
            &field.kind,
            &value,
            schema,
            depth,
            name,
            message_name,
        )?;
    }

    Ok(())
}

/// `TimeStamp` and `Expires`: the two moments which are the server's, not the
/// entity's.
fn write_server_owned_field(
    dest: &mut Vec<u8>,
    name: &str,
    value: &JsonValueRef,
) -> Result<(), String> {
    // The write path stamps the server's clock on the row it stores, so a
    // TimeStamp here is at best the version the caller read - and this call is
    // not the one that checks versions.
    if name == TIME_STAMP {
        return Ok(());
    }

    // Absent and null are both "never", which is what a row without the field
    // already means.
    if value.is_null() {
        return Ok(());
    }

    let moment = read_moment(value)?;

    write_varint_field(dest, consts::FIELD_EXPIRES, moment as u64);

    Ok(())
}

/// A moment as the renderer shows one - RFC3339 - or as the number of unix
/// microseconds it is underneath.
///
/// The range is checked here rather than left to the parser: a value outside
/// the calendar is stored happily and then can not be shown, and the row would
/// do it again after every restart. `ParsedEntity::parse` refuses it too, so
/// this only decides which message the caller gets.
fn read_moment(value: &JsonValueRef) -> Result<i64, String> {
    let moment = if value.is_string() {
        let Some(text) = value.as_str() else {
            return Err("'Expires' can not be read".to_string());
        };

        let Some(moment) = DateTimeAsMicroseconds::from_str(text.as_str()) else {
            return Err(format!(
                "'Expires' is '{}', which is not a date and time",
                text.as_str()
            ));
        };

        moment.unix_microseconds
    } else {
        as_integer::<i64>(value).ok_or_else(|| {
            "'Expires' is neither a date and time nor unix microseconds".to_string()
        })?
    };

    if !moment_is_in_range(moment) {
        return Err("'Expires' is outside the calendar this server can show".to_string());
    }

    Ok(moment)
}

#[allow(clippy::too_many_arguments)]
fn write_value(
    dest: &mut Vec<u8>,
    no: u32,
    kind: &FieldKind,
    value: &JsonValueRef,
    schema: &SchemaIndex,
    depth: usize,
    name: &str,
    message_name: &str,
) -> Result<(), String> {
    let wrong = |expected: &str| format!("'{name}' of '{message_name}' is not {expected}");

    match kind {
        FieldKind::Message(nested_name) => {
            if !value.is_object() {
                return Err(wrong("an object"));
            }

            let mut nested = Vec::new();
            write_message(
                &mut nested,
                value.as_slice(),
                schema,
                nested_name,
                depth + 1,
            )?;

            write_len_field(dest, no, &nested);
        }

        FieldKind::String => {
            let Some(text) = as_string(value) else {
                return Err(wrong("a string"));
            };

            write_len_field(dest, no, text.as_str().as_bytes());
        }

        FieldKind::Bytes => {
            let Some(text) = as_string(value) else {
                return Err(wrong("a base64 string"));
            };

            let bytes = base64::decode(text.as_str())
                .map_err(|err| format!("'{name}' of '{message_name}': {err}"))?;

            write_len_field(dest, no, &bytes);
        }

        FieldKind::Bool => {
            let Some(flag) = value.unwrap_as_bool() else {
                return Err(wrong("a boolean"));
            };

            write_varint_field(dest, no, u64::from(flag));
        }

        FieldKind::Double => {
            let number = as_double(value).ok_or_else(|| wrong("a number"))?;
            write_i64_field(dest, no, number.to_bits());
        }

        FieldKind::Float => {
            let number = as_double(value).ok_or_else(|| wrong("a number"))?;
            write_i32_field(dest, no, (number as f32).to_bits());
        }

        FieldKind::Int64 => {
            let number = as_integer::<i64>(value).ok_or_else(|| wrong("a whole number"))?;
            write_varint_field(dest, no, number as u64);
        }

        FieldKind::Int32 => {
            let number = as_integer::<i32>(value).ok_or_else(|| wrong("a whole number"))?;
            // Sign extended, which is how protobuf writes a negative `int32`.
            write_varint_field(dest, no, number as i64 as u64);
        }

        FieldKind::Uint64 => {
            let number = as_integer::<u64>(value).ok_or_else(|| wrong("a whole number"))?;
            write_varint_field(dest, no, number);
        }

        FieldKind::Uint32 => {
            let number = as_integer::<u32>(value).ok_or_else(|| wrong("a whole number"))?;
            write_varint_field(dest, no, u64::from(number));
        }
    }

    Ok(())
}

/// A string, and only a string. `as_str` is happy to hand back a number as its
/// text, so the type is asked first: a row key which arrived as `1` is a caller
/// bug, not a key called "1".
fn as_string<'s>(value: &'s JsonValueRef<'s>) -> Option<rust_extensions::StrOrString<'s>> {
    if !value.is_string() {
        return None;
    }

    value.as_str()
}

fn as_double(value: &JsonValueRef) -> Option<f64> {
    // `is_number` is the whole ones and `is_double` the rest; a float field
    // takes either, which is what makes `{"Amount": 1}` a valid `double`.
    if !value.is_number() && !value.is_double() {
        return None;
    }

    value.as_raw_str()?.trim().parse().ok()
}

/// A whole number of the field's own width.
///
/// Parsed as the target type, so a value which does not fit is refused instead
/// of wrapped: a limit stored as its own negative is worse than a call which
/// failed. `1.5` into an `int32` fails here for the same reason - rounding it
/// would be an answer nobody asked for.
fn as_integer<T: std::str::FromStr>(value: &JsonValueRef) -> Option<T> {
    if !value.is_number() {
        return None;
    }

    value.as_raw_str()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use my_no_sql_grpc_core::schemas::{DeclaredField, Scalar, SchemaBuilder};

    use super::*;
    use crate::json_view::write_row_as_json;

    fn index(schema: &[u8]) -> SchemaIndex {
        SchemaIndex::build(schema).unwrap()
    }

    fn field(name: &str, no: u32, scalar: Scalar, is_array: bool) -> DeclaredField {
        DeclaredField::scalar(name, no, scalar, is_array)
    }

    fn rendered(row: &[u8], schema: &SchemaIndex) -> String {
        write_row_as_json(
            my_json::json_writer::JsonObjectWriter::new(),
            row,
            Some(schema),
        )
        .build()
    }

    /// The shape the macro declares for an entity: the two keys as ordinary
    /// fields, everything else from 5 up.
    fn trader_schema() -> Vec<u8> {
        SchemaBuilder::new("TraderEntity")
            .add_field(field("PartitionKey", 1, Scalar::String, false))
            .add_field(field("RowKey", 2, Scalar::String, false))
            .add_field(field("Amount", 5, Scalar::F64, false))
            .add_field(field("Level", 6, Scalar::I32, false))
            .add_field(field("Enabled", 7, Scalar::Bool, false))
            .add_field(field("Instruments", 8, Scalar::String, true))
            .add_field(field("Blob", 9, Scalar::Bytes, false))
            .build()
            .serialize()
    }

    /// What goes in comes back out: the row is written by this module and read
    /// by the renderer, so a disagreement between the two shows up here.
    #[test]
    fn a_row_written_from_json_renders_back_to_the_same_json() {
        let schema = index(&trader_schema());

        let json = r#"{"PartitionKey":"acc-1","RowKey":"rk-1","Amount":12.5,"Level":-3,"Enabled":true,"Instruments":["EURUSD","GBPUSD"],"Blob":"Zm9vYmFy"}"#;

        let row = write_row_from_json(json.as_bytes(), &schema).unwrap();

        assert_eq!(rendered(&row, &schema), json);
    }

    /// The whole reason the schema is consulted by name: a typo has to be an
    /// answer, not a row stored without the value.
    #[test]
    fn a_field_the_schema_does_not_have_is_refused() {
        let schema = index(&trader_schema());

        let err = write_row_from_json(
            br#"{"PartitionKey":"acc-1","RowKey":"rk-1","Amonut":1}"#,
            &schema,
        )
        .unwrap_err();

        assert!(err.contains("Amonut"), "{err}");
        // ...and the caller is told what it could have said instead.
        assert!(err.contains("Amount"), "{err}");
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused() {
        let schema = index(&trader_schema());

        for json in [
            br#"{"Amount":"12.5"}"#.as_slice(),
            br#"{"Level":1.5}"#,
            br#"{"Enabled":"true"}"#,
            br#"{"PartitionKey":1}"#,
            br#"{"Instruments":"EURUSD"}"#,
            br#"{"Blob":"not base64!"}"#,
        ] {
            assert!(
                write_row_from_json(json, &schema).is_err(),
                "{}",
                std::str::from_utf8(json).unwrap()
            );
        }
    }

    /// Absent, and null, and "never" are one state - and it is the state a row
    /// with no `Expires` at all is already in.
    #[test]
    fn expires_is_taken_and_time_stamp_is_not() {
        let schema = index(&trader_schema());

        let row = write_row_from_json(
            br#"{"PartitionKey":"acc-1","RowKey":"rk-1","TimeStamp":"2024-01-01T00:00:00","Expires":"2030-01-01T00:00:00"}"#,
            &schema,
        )
        .unwrap();

        let parsed = my_no_sql_grpc_core::db_entity::ParsedEntity::parse(&row).unwrap();

        assert!(
            parsed.time_stamp.is_none(),
            "the caller's TimeStamp was kept"
        );
        assert_eq!(
            &DateTimeAsMicroseconds::new(parsed.expires).to_rfc3339()[..19],
            "2030-01-01T00:00:00"
        );

        let without = write_row_from_json(
            br#"{"PartitionKey":"acc-1","RowKey":"rk-1","Expires":null}"#,
            &schema,
        )
        .unwrap();

        // Zero is what a row with no `Expires` at all already says: never.
        assert_eq!(
            my_no_sql_grpc_core::db_entity::ParsedEntity::parse(&without)
                .unwrap()
                .expires,
            0
        );
    }

    /// A moment past the calendar is refused where it is written rather than
    /// where it is shown: the row would be on disk, unshowable, for good.
    #[test]
    fn an_expires_outside_the_calendar_is_refused() {
        let schema = index(&trader_schema());

        let err = write_row_from_json(
            br#"{"PartitionKey":"acc-1","RowKey":"rk-1","Expires":9223372036854775807}"#,
            &schema,
        )
        .unwrap_err();

        assert!(err.contains("calendar"), "{err}");
    }

    /// A message inside a message, and a repeated one: the schema resolves the
    /// nested name and the encoder writes one length-delimited field per item.
    #[test]
    fn nested_messages_are_written_through_the_schema() {
        let schema = SchemaBuilder::new("TraderEntity")
            .add_field(field("PartitionKey", 1, Scalar::String, false))
            .add_field(field("RowKey", 2, Scalar::String, false))
            .add_field(DeclaredField::object("Limits", 5, "Limits", false))
            .add_field(DeclaredField::object("History", 6, "Limits", true))
            .add_message("Limits", vec![field("MaxLots", 1, Scalar::F64, false)])
            .build()
            .serialize();

        let schema = index(&schema);

        let json = r#"{"PartitionKey":"acc-1","RowKey":"rk-1","Limits":{"MaxLots":10.5},"History":[{"MaxLots":1.25},{"MaxLots":2.75}]}"#;

        let row = write_row_from_json(json.as_bytes(), &schema).unwrap();

        assert_eq!(rendered(&row, &schema), json);
    }
}
