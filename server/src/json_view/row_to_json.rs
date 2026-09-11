use ahash::AHashMap;
use my_json::json_writer::{JsonArrayWriter, JsonObjectWriter};
use my_no_sql_grpc_abstractions::db_entity::{ProtobufReader, consts, moment_is_in_range};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::base64;
use super::{FieldKind, SchemaIndex};

/// Renders a stored row as JSON into `writer`.
///
/// The writer is passed in rather than created here: when the row is an element
/// of an array, the array's own state lives in the writer my-json hands to the
/// closure, and building a fresh one instead drops the enclosing brackets.
///
/// With a schema the fields come out under their own names and types. Without
/// one - the client never registered that entity version, or its schema did not
/// parse - the row is still shown, with field numbers for names and the types
/// guessed from the wire format. Refusing to show a row we can not name is worse
/// than showing it plainly.
pub fn write_row_as_json(
    writer: JsonObjectWriter,
    row_bytes: &[u8],
    schema: Option<&SchemaIndex>,
) -> JsonObjectWriter {
    let message_name = schema.map(|schema| schema.get_root_message_name().to_string());

    write_message(writer, row_bytes, schema, message_name.as_deref(), 0)
}

/// How deep a nested value is followed before it is shown as a blob instead.
///
/// The renderer walks the **data**, one stack frame per level, and two bytes of
/// wire make a level - so a forty-kilobyte row is twenty thousand of them.
///
/// A message which carries itself no longer *compiles* on the client side: the
/// id is a constant and a constant can not name itself. That does not retire
/// this cap, and it was never what the cap was for. A schema reaches the server
/// as bytes, and bytes are not a declaration - anybody able to write a row can
/// write down the shape no macro would have accepted. Overflowing the stack is
/// an abort, not a panic a handler could catch, and the row would be on disk to
/// do it again after the restart. 100 is what protobuf implementations use.
const MAX_NESTING_DEPTH: usize = 100;

/// One occurrence of one field on the wire.
struct FieldValue<'s> {
    wire_type: u8,
    value: &'s [u8],
    value_as_u64: u64,
}

/// Everything a message carries, grouped by field number and keeping the order
/// the fields first appeared in so the JSON comes out stable.
struct CollectedMessage<'s> {
    order: Vec<u32>,
    fields: AHashMap<u32, Vec<FieldValue<'s>>>,
}

fn collect(bytes: &[u8]) -> CollectedMessage<'_> {
    let mut order = Vec::new();
    let mut fields: AHashMap<u32, Vec<FieldValue>> = AHashMap::new();

    let mut reader = ProtobufReader::new(bytes);

    // A message we can not walk to the end is shown as far as it was readable -
    // this is a viewer, and half a row beats an error page.
    while let Ok(Some(field)) = reader.get_next() {
        let entry = fields.entry(field.field_no).or_insert_with(|| {
            order.push(field.field_no);
            Vec::new()
        });

        entry.push(FieldValue {
            wire_type: field.wire_type,
            value: field.value.get_slice(bytes),
            value_as_u64: field.value_as_u64,
        });
    }

    CollectedMessage { order, fields }
}

fn write_message(
    mut writer: JsonObjectWriter,
    bytes: &[u8],
    schema: Option<&SchemaIndex>,
    message_name: Option<&str>,
    depth: usize,
) -> JsonObjectWriter {
    let collected = collect(bytes);

    let message_schema = match (schema, message_name) {
        (Some(schema), Some(message_name)) => schema.get_message(message_name),
        _ => None,
    };

    for field_no in collected.order.iter().copied() {
        let values = collected.fields.get(&field_no).unwrap();

        let field_schema = message_schema.and_then(|itm| itm.fields.get(&field_no));

        // TimeStamp and Expires are `int64` in the contract, but they are the two
        // fields whose meaning the server owns - showing raw microseconds would
        // make every row unreadable in a UI.
        if depth == 0
            && (field_no == consts::FIELD_TIME_STAMP || field_no == consts::FIELD_EXPIRES)
            && let Some(last) = values.last()
        {
            let name = match field_schema {
                Some(field_schema) => field_schema.name.as_str(),
                None if field_no == consts::FIELD_TIME_STAMP => "TimeStamp",
                None => "Expires",
            };

            writer = writer.write(name, to_iso(last.value_as_u64 as i64));
            continue;
        }

        let Some(field_schema) = field_schema else {
            writer = write_unknown_field(writer, field_no, values);
            continue;
        };

        writer = write_known_field(writer, field_schema, values, schema, depth);
    }

    writer
}

fn write_known_field(
    writer: JsonObjectWriter,
    field_schema: &super::FieldSchema,
    values: &[FieldValue],
    schema: Option<&SchemaIndex>,
    depth: usize,
) -> JsonObjectWriter {
    let name = field_schema.name.as_str();

    // Past the cap the message is not followed but shown - as base64. The row
    // stays readable down to the level where it stopped, which is the same
    // bargain the rest of this viewer makes.
    if let FieldKind::Message(message_name) = &field_schema.kind
        && depth < MAX_NESTING_DEPTH
    {
        if field_schema.repeated {
            return writer.write_json_array(name, |mut array| {
                for value in values {
                    array = array.write_json_object(|nested| {
                        write_message(nested, value.value, schema, Some(message_name), depth + 1)
                    });
                }
                array
            });
        }

        let Some(last) = values.last() else {
            return writer;
        };

        return writer.write_json_object(name, |nested| {
            write_message(nested, last.value, schema, Some(message_name), depth + 1)
        });
    }

    if field_schema.repeated {
        return writer.write_json_array(name, |mut array| {
            for scalar in expand_scalars(&field_schema.kind, values) {
                array = write_scalar_into_array(array, scalar);
            }
            array
        });
    }

    let Some(last) = values.last() else {
        return writer;
    };

    write_scalar_into_object(writer, name, to_scalar(&field_schema.kind, last))
}

fn write_unknown_field(
    writer: JsonObjectWriter,
    field_no: u32,
    values: &[FieldValue],
) -> JsonObjectWriter {
    let name = field_no.to_string();

    if values.len() > 1 {
        return writer.write_json_array(name.as_str(), |mut array| {
            for value in values {
                array = write_scalar_into_array(array, guess_scalar(value));
            }
            array
        });
    }

    let Some(last) = values.last() else {
        return writer;
    };

    write_scalar_into_object(writer, name.as_str(), guess_scalar(last))
}

/// A decoded value, in the handful of shapes JSON actually has.
enum Scalar {
    Int(i64),
    Uint(u64),
    Double(f64),
    Bool(bool),
    Text(String),
}

fn write_scalar_into_object(
    writer: JsonObjectWriter,
    name: &str,
    scalar: Scalar,
) -> JsonObjectWriter {
    match scalar {
        Scalar::Int(value) => writer.write(name, value),
        Scalar::Uint(value) => writer.write(name, value),
        Scalar::Double(value) => writer.write(name, value),
        Scalar::Bool(value) => writer.write(name, value),
        Scalar::Text(value) => writer.write(name, value.as_str()),
    }
}

fn write_scalar_into_array(writer: JsonArrayWriter, scalar: Scalar) -> JsonArrayWriter {
    match scalar {
        Scalar::Int(value) => writer.write(value),
        Scalar::Uint(value) => writer.write(value),
        Scalar::Double(value) => writer.write(value),
        Scalar::Bool(value) => writer.write(value),
        Scalar::Text(value) => writer.write(value.as_str()),
    }
}

/// Repeated scalars may arrive either as one field per value or packed into a
/// single length-delimited field - a generated client emits the packed form by
/// default.
fn expand_scalars(kind: &FieldKind, values: &[FieldValue]) -> Vec<Scalar> {
    let mut result = Vec::new();

    for value in values {
        if !kind.is_packable() || value.wire_type != consts::WIRE_TYPE_LEN {
            result.push(to_scalar(kind, value));
            continue;
        }

        let mut reader = PackedReader::new(value.value);

        while let Some(element) = reader.read_next(kind) {
            result.push(to_scalar(kind, &element));
        }
    }

    result
}

fn to_scalar(kind: &FieldKind, value: &FieldValue) -> Scalar {
    match kind {
        FieldKind::Double => Scalar::Double(f64::from_bits(value.value_as_u64)),
        FieldKind::Float => Scalar::Double(f32::from_bits(value.value_as_u64 as u32) as f64),
        FieldKind::Int64 => Scalar::Int(value.value_as_u64 as i64),
        FieldKind::Int32 => Scalar::Int(value.value_as_u64 as i32 as i64),
        FieldKind::Uint64 => Scalar::Uint(value.value_as_u64),
        FieldKind::Uint32 => Scalar::Uint(value.value_as_u64 as u32 as u64),
        FieldKind::Bool => Scalar::Bool(value.value_as_u64 != 0),
        FieldKind::String => Scalar::Text(String::from_utf8_lossy(value.value).to_string()),
        FieldKind::Bytes => Scalar::Text(base64::encode(value.value)),
        // Every reference is checked when the schema is read, so the only way to
        // arrive here is past the nesting cap - this is that fallback.
        FieldKind::Message(_) => Scalar::Text(base64::encode(value.value)),
    }
}

/// What a field looks like when nothing says what it is.
fn guess_scalar(value: &FieldValue) -> Scalar {
    match value.wire_type {
        consts::WIRE_TYPE_LEN => match std::str::from_utf8(value.value) {
            Ok(text) => Scalar::Text(text.to_string()),
            Err(_) => Scalar::Text(base64::encode(value.value)),
        },
        _ => Scalar::Int(value.value_as_u64 as i64),
    }
}

/// A moment outside the calendar is shown as the number it is.
///
/// Such a value can no longer be written - `ParsedEntity::parse` refuses it -
/// but a row that took one before that check existed is on disk, and rendering
/// it panics inside the date library. A panic here is not one row failing: it is
/// every read of the partition holding it, after every restart, with no way to
/// look at the row and see why.
fn to_iso(unix_microseconds: i64) -> String {
    if !moment_is_in_range(unix_microseconds) {
        return unix_microseconds.to_string();
    }

    DateTimeAsMicroseconds::new(unix_microseconds).to_rfc3339()
}

/// Cursor over a packed repeated payload.
struct PackedReader<'s> {
    src: &'s [u8],
    pos: usize,
}

impl<'s> PackedReader<'s> {
    fn new(src: &'s [u8]) -> Self {
        Self { src, pos: 0 }
    }

    /// A packed payload has no tags - it is values of one type back to back, so
    /// the width comes from the field's type rather than from the wire.
    fn read_next(&mut self, kind: &FieldKind) -> Option<FieldValue<'s>> {
        match kind {
            FieldKind::Double => self.read_fixed(8),
            FieldKind::Float => self.read_fixed(4),
            _ => self.read_varint(),
        }
    }

    fn read_fixed(&mut self, len: usize) -> Option<FieldValue<'s>> {
        let end = self.pos.checked_add(len)?;

        if end > self.src.len() {
            return None;
        }

        let bytes = &self.src[self.pos..end];
        self.pos = end;

        let mut value_as_u64: u64 = 0;
        for (shift, byte) in bytes.iter().enumerate() {
            value_as_u64 |= u64::from(*byte) << (shift * 8);
        }

        Some(FieldValue {
            wire_type: consts::WIRE_TYPE_I64,
            value: bytes,
            value_as_u64,
        })
    }

    fn read_varint(&mut self) -> Option<FieldValue<'s>> {
        let start = self.pos;
        let mut result: u64 = 0;

        for shift in 0..10u32 {
            let byte = *self.src.get(self.pos)?;
            self.pos += 1;
            result |= u64::from(byte & 0x7F) << (shift * 7);

            if byte & 0x80 == 0 {
                return Some(FieldValue {
                    wire_type: consts::WIRE_TYPE_VARINT,
                    value: &self.src[start..self.pos],
                    value_as_u64: result,
                });
            }
        }

        None
    }
}

/// The nesting cap, and the moment that can not be turned into a date. Both are
/// about a row which is already stored: the rest of the renderer is exercised in
/// `json_view::tests` against a schema built the way a client's arrives.
#[cfg(test)]
mod tests {
    use my_no_sql_grpc_abstractions::db_entity::{MAX_MOMENT_MICROSECONDS, write_varint};
    use my_no_sql_grpc_abstractions::schemas::{DeclaredField, SchemaBuilder};

    use super::*;

    /// `message Deep { Deep Inner = 5; }` - a message which carries itself.
    ///
    /// No macro would compile this any more: the id is a constant and a constant
    /// can not name itself. It is still perfectly writable *as bytes*, which is
    /// the whole reason the cap below is not a client-side concern - the builder
    /// is a convenience of ours, and the server is handed a blob.
    fn self_referential_schema() -> SchemaIndex {
        let schema = SchemaBuilder::new("Deep")
            .add_field(DeclaredField::object("Inner", 5, "Deep", false))
            .build()
            .serialize();

        SchemaIndex::build(&schema).unwrap()
    }

    fn varint_len(mut value: u64) -> usize {
        let mut result = 1;

        while value >= 0x80 {
            value >>= 7;
            result += 1;
        }

        result
    }

    /// Written outside in from the sizes: nesting one level at a time would copy
    /// the whole payload once per level, and the point of this row is that there
    /// are twenty thousand of them.
    fn nested_row(levels: usize) -> Vec<u8> {
        let mut sizes = Vec::with_capacity(levels + 1);
        sizes.push(0usize);

        for level in 1..=levels {
            let inner = sizes[level - 1];
            sizes.push(1 + varint_len(inner as u64) + inner);
        }

        let mut result = Vec::with_capacity(sizes[levels]);

        for level in (1..=levels).rev() {
            write_varint(&mut result, 5 << 3 | u64::from(consts::WIRE_TYPE_LEN));
            write_varint(&mut result, sizes[level - 1] as u64);
        }

        result
    }

    /// Before the cap this walked one stack frame per level of the *data*, so a
    /// row of a few dozen kilobytes overflowed the worker stack on the first
    /// read - and a stack overflow aborts the process instead of failing the
    /// request. The row is persisted, so it did it again after every restart.
    #[test]
    fn a_row_nested_deeper_than_the_cap_is_rendered_instead_of_walked() {
        let row = nested_row(20_000);
        assert!(row.len() > 40_000, "the row is {} bytes", row.len());

        let json = write_row_as_json(
            JsonObjectWriter::new(),
            &row,
            Some(&self_referential_schema()),
        )
        .build();

        // Followed exactly to the cap...
        assert_eq!(json.matches(r#""Inner":{"#).count(), MAX_NESTING_DEPTH);
        // ...and what is left is shown the way an unresolvable message is: a
        // base64 string, not an error and not a truncated row.
        assert_eq!(json.matches(r#""Inner":""#).count(), 1);
    }

    #[test]
    fn a_row_within_the_cap_is_followed_all_the_way_down() {
        let json = write_row_as_json(
            JsonObjectWriter::new(),
            &nested_row(3),
            Some(&self_referential_schema()),
        )
        .build();

        assert_eq!(json, r#"{"Inner":{"Inner":{"Inner":{}}}}"#);
    }

    /// A row written before `ParsedEntity::parse` learned to refuse such a value
    /// still has to be viewable: the date library panics on it, and a panic here
    /// takes down every read of the partition rather than this one row.
    #[test]
    fn a_moment_outside_the_calendar_is_shown_as_a_number() {
        for value in [i64::MAX, i64::MIN, MAX_MOMENT_MICROSECONDS + 1] {
            let mut row = Vec::new();
            write_varint(
                &mut row,
                u64::from(consts::FIELD_EXPIRES) << 3 | u64::from(consts::WIRE_TYPE_VARINT),
            );
            write_varint(&mut row, value as u64);

            let json = write_row_as_json(JsonObjectWriter::new(), &row, None).build();

            assert_eq!(json, format!(r#"{{"Expires":"{value}"}}"#));
        }
    }
}
