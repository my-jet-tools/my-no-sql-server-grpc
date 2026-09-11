//! The shape of a stored entity, as this server means to describe it.
//!
//! It exists for one purpose - showing a stored row as JSON - and takes no part
//! in storing one: a row is parsed by walking the wire format, and that is why
//! the server never needs a schema to accept a write.
//!
//! Everything here is **canonical by construction**: a message's fields are
//! sorted by number and a schema's messages by name, so the same declared entity
//! produces the same bytes in every process and every build. That is not a
//! nicety: the server tells two schemas apart by **comparing these bytes**, so a
//! second spelling of one schema would be a second answer to "is this the same
//! schema", and two processes which disagreed on the order would disagree about
//! every entity they both declare.

use crate::db_entity::{ProtobufField, ProtobufReader, write_len_field, write_varint_field};

/// One of the nine types a stored value can have.
///
/// Deliberately fewer than protobuf's fifteen. The macro is the only thing that
/// produces a schema and `macros/src/field_type.rs` maps Rust onto exactly these
/// nine, so `sint*`, `fixed*`, `sfixed*`, enums and maps are not missing from
/// this list - they can not be written down at all. The closed world stops being
/// a convention of our own client and becomes a property of the format.
///
/// The wire type in front of a value already says varint from fixed width, so
/// this does not repeat it. What it answers is "integer or float" and "signed or
/// not" - which is what tells an `F64` from a `U64` when both arrive as wire
/// type I64.
///
/// The numbers are part of the serialized form and are never reused: a stored
/// row names its schema by the hash of those bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scalar {
    Bool = 1,
    I32 = 2,
    I64 = 3,
    U32 = 4,
    U64 = 5,
    F32 = 6,
    F64 = 7,
    String = 8,
    Bytes = 9,
}

impl Scalar {
    fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            1 => Self::Bool,
            2 => Self::I32,
            3 => Self::I64,
            4 => Self::U32,
            5 => Self::U64,
            6 => Self::F32,
            7 => Self::F64,
            8 => Self::String,
            9 => Self::Bytes,
            _ => return None,
        })
    }
}

/// What one value is.
///
/// There is no `Array` variant here and that is the point: protobuf has no
/// `repeated repeated`, so an array of arrays is not a case to refuse at run
/// time - it can not be written down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ItemType {
    Scalar(Scalar),
    /// A message, by its **index** in [`Schema::messages`] rather than inlined
    /// into the field. That is what makes a message which carries itself, or two
    /// which carry each other, a finite number of bytes.
    Object(u16),
}

/// What a field carries: one value or a run of them.
///
/// `Array` carries its item type rather than standing on its own, so an array of
/// nothing can not be written down either. Both of these are the reason the two
/// enums are nested this way instead of being one flat enum with a flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tp {
    Item(ItemType),
    Array(ItemType),
}

impl Tp {
    /// What one value of the field is, whether or not there are many of them.
    pub fn get_item(&self) -> ItemType {
        match self {
            Tp::Item(item) | Tp::Array(item) => *item,
        }
    }

    pub fn is_array(&self) -> bool {
        matches!(self, Tp::Array(_))
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Field {
    /// The number the value sits under on the wire. Never `0`: protobuf has no
    /// field zero.
    ///
    /// As wide as the wire allows rather than as narrow as anybody is likely to
    /// need. It is written as a varint, so the width costs nothing for any
    /// number a person would pick, and the ceiling that does exist - `2^29-1` -
    /// is protobuf's own and is already refused where rows are parsed, in
    /// `db_entity::ProtobufReader`. A narrower type here would be a second,
    /// quieter limit: a field the reader hands out but the schema can not name.
    pub no: u32,
    pub name: String,
    pub tp: Tp,
}

/// One message: the entity itself, or something a field of it carries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    pub name: String,
    /// Sorted by [`Field::no`], with no number twice.
    pub fields: Vec<Field>,
}

impl Message {
    /// The field a number on the wire belongs to, or `None` for a number this
    /// schema does not describe - a row written by a build which had one more
    /// field than this schema does.
    ///
    /// A binary search rather than a scan, which the sorted order pays for: the
    /// renderer asks this once per field of every row it shows.
    pub fn get_field(&self, no: u32) -> Option<&Field> {
        let found = self
            .fields
            .binary_search_by_key(&no, |field| field.no)
            .ok()?;
        self.fields.get(found)
    }
}

/// Everything needed to show a row of one entity.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Schema {
    /// Where the entity itself sits in `messages`. It is one of them: a field
    /// pointing at the entity resolves the same way as any other reference.
    pub root: u16,
    /// Sorted by [`Message::name`], with no name twice.
    pub messages: Vec<Message>,
}

// The frame, written with the same primitives everything else on this wire uses:
//
//   Schema  { root = 1 (varint); repeated Message = 2 }
//   Message { name = 1; repeated Field = 2 }
//   Field   { no = 1 (varint); name = 2;
//             scalar = 3 (varint) exclusive-or object = 4 (varint);
//             array = 5 (varint) }
//
// Every singular field is written every time, default value or not, and read
// back exactly once. Proto3's habit of leaving a default off the wire would give
// one schema two spellings, and two spellings are two ids.
const SCHEMA_ROOT: u32 = 1;
const SCHEMA_MESSAGE: u32 = 2;
const MESSAGE_NAME: u32 = 1;
const MESSAGE_FIELD: u32 = 2;
const FIELD_NO: u32 = 1;
const FIELD_NAME: u32 = 2;
const FIELD_SCALAR: u32 = 3;
const FIELD_OBJECT: u32 = 4;
const FIELD_ARRAY: u32 = 5;

impl Schema {
    /// The canonical bytes. Same schema, same bytes - in this process and in any
    /// other.
    ///
    /// The order is **asserted** rather than imposed here, and it has to be:
    /// [`ItemType::Object`] is a position in `messages`, so a sort at this point
    /// would repoint every reference in the schema at a different message. The
    /// one place messages are put in order is
    /// [`super::SchemaBuilder::build`], where the indices are handed out
    /// afterwards.
    pub fn serialize(&self) -> Vec<u8> {
        self.assert_canonical();

        let mut result = Vec::new();

        write_varint_field(&mut result, SCHEMA_ROOT, u64::from(self.root));

        for message in self.messages.iter() {
            write_len_field(&mut result, SCHEMA_MESSAGE, &serialize_message(message));
        }

        result
    }

    /// Reads back what [`Self::serialize`] wrote.
    ///
    /// It refuses everything which is not the canonical spelling - fields out of
    /// order, a name or a number twice, a field of the frame this format does not
    /// have - because the bytes and the schema have to mean each other both ways:
    /// the id is the hash of the bytes, so a second way of spelling one schema
    /// would be a second id for it, and the sorted order the rest of the code
    /// leans on would be a promise nobody kept.
    ///
    /// The failure is text and nothing else: nobody branches on why a blob is not
    /// a schema, they say so and refuse the write or the file.
    pub fn from_slice(src: &[u8]) -> Result<Self, String> {
        let mut reader = ProtobufReader::new(src);

        let mut root = None;
        let mut messages: Vec<Message> = Vec::new();

        while let Some(field) = next(&mut reader)? {
            match field.field_no {
                SCHEMA_ROOT => {
                    set_once(&mut root, read_u16(&field)?, "the schema's root")?;
                }

                SCHEMA_MESSAGE => {
                    let message = read_message(as_message(&field, src)?)?;

                    if let Some(previous) = messages.last()
                        && message.name <= previous.name
                    {
                        return Err(format!(
                            "messages are not sorted by name: '{}' comes after '{}'",
                            message.name, previous.name
                        ));
                    }

                    messages.push(message);
                }

                other => return Err(format!("a schema has no field {other}")),
            }
        }

        let Some(root) = root else {
            return Err("the schema does not say which of its messages is the root".to_string());
        };

        let result = Self { root, messages };
        result.validate_references()?;

        Ok(result)
    }

    /// The message a reference points at. Every reference is checked when the
    /// schema is built or read, so this indexes rather than answers `Option`.
    pub fn get_message(&self, index: u16) -> &Message {
        &self.messages[index as usize]
    }

    pub fn get_root(&self) -> &Message {
        self.get_message(self.root)
    }

    /// Every index has to name a message that is there, or the renderer would
    /// walk off the end of the table on a row it has already accepted.
    fn validate_references(&self) -> Result<(), String> {
        let amount = self.messages.len();

        if usize::from(self.root) >= amount {
            return Err(format!(
                "the root is message {}, and the schema has {amount} of them",
                self.root
            ));
        }

        for message in self.messages.iter() {
            for field in message.fields.iter() {
                let ItemType::Object(index) = field.tp.get_item() else {
                    continue;
                };

                if usize::from(index) >= amount {
                    return Err(format!(
                        "field '{}' of '{}' carries message {index}, and the schema has {amount} of them",
                        field.name, message.name
                    ));
                }
            }
        }

        Ok(())
    }

    fn assert_canonical(&self) {
        for pair in self.messages.windows(2) {
            assert!(
                pair[0].name < pair[1].name,
                "the schema is not canonical: message '{}' comes after '{}'",
                pair[1].name,
                pair[0].name
            );
        }

        for message in self.messages.iter() {
            for pair in message.fields.windows(2) {
                assert!(
                    pair[0].no < pair[1].no,
                    "message '{}' is not canonical: field {} comes after {}",
                    message.name,
                    pair[1].no,
                    pair[0].no
                );
            }
        }
    }
}

fn serialize_message(message: &Message) -> Vec<u8> {
    let mut result = Vec::new();

    write_len_field(&mut result, MESSAGE_NAME, message.name.as_bytes());

    for field in message.fields.iter() {
        write_len_field(&mut result, MESSAGE_FIELD, &serialize_field(field));
    }

    result
}

fn serialize_field(field: &Field) -> Vec<u8> {
    let mut result = Vec::new();

    write_varint_field(&mut result, FIELD_NO, u64::from(field.no));
    write_len_field(&mut result, FIELD_NAME, field.name.as_bytes());

    match field.tp.get_item() {
        ItemType::Scalar(scalar) => {
            write_varint_field(&mut result, FIELD_SCALAR, scalar as u64);
        }
        ItemType::Object(index) => {
            write_varint_field(&mut result, FIELD_OBJECT, u64::from(index));
        }
    }

    write_varint_field(&mut result, FIELD_ARRAY, u64::from(field.tp.is_array()));

    result
}

fn read_message(src: &[u8]) -> Result<Message, String> {
    let mut reader = ProtobufReader::new(src);

    let mut name = None;
    let mut fields: Vec<Field> = Vec::new();

    while let Some(field) = next(&mut reader)? {
        match field.field_no {
            MESSAGE_NAME => {
                set_once(&mut name, as_string(&field, src)?, "a message's name")?;
            }

            MESSAGE_FIELD => {
                let read = read_field(as_message(&field, src)?)?;

                if let Some(previous) = fields.last()
                    && read.no <= previous.no
                {
                    return Err(format!(
                        "fields are not sorted by number: {} comes after {}",
                        read.no, previous.no
                    ));
                }

                fields.push(read);
            }

            other => return Err(format!("a message has no field {other}")),
        }
    }

    let Some(name) = name else {
        return Err("a message of the schema has no name".to_string());
    };

    Ok(Message { name, fields })
}

fn read_field(src: &[u8]) -> Result<Field, String> {
    let mut reader = ProtobufReader::new(src);

    let mut no = None;
    let mut name = None;
    let mut item = None;
    let mut is_array = None;

    while let Some(field) = next(&mut reader)? {
        match field.field_no {
            FIELD_NO => set_once(&mut no, read_u32(&field)?, "a field's number")?,

            FIELD_NAME => set_once(&mut name, as_string(&field, src)?, "a field's name")?,

            FIELD_SCALAR => {
                let code = as_varint(&field)?;

                let Some(scalar) = Scalar::from_code(code) else {
                    return Err(format!("{code} is not one of the nine scalar types"));
                };

                // Exclusive with the one below, and said by the same words: a
                // field carrying both, or neither, describes nothing.
                set_once(&mut item, ItemType::Scalar(scalar), "what a field carries")?;
            }

            FIELD_OBJECT => {
                let index = read_u16(&field)?;
                set_once(&mut item, ItemType::Object(index), "what a field carries")?;
            }

            FIELD_ARRAY => {
                let value = as_varint(&field)?;

                if value > 1 {
                    return Err(format!("{value} is not an answer to 'is it an array'"));
                }

                set_once(&mut is_array, value == 1, "whether a field is an array")?;
            }

            other => return Err(format!("a field has no field {other}")),
        }
    }

    let (Some(no), Some(name), Some(item), Some(is_array)) = (no, name, item, is_array) else {
        return Err("a field of the schema is missing its number, name, type or arity".to_string());
    };

    if no == 0 {
        return Err("field number 0 is not a number protobuf has".to_string());
    }

    Ok(Field {
        no,
        name,
        tp: if is_array {
            Tp::Array(item)
        } else {
            Tp::Item(item)
        },
    })
}

/// Every singular field of the frame arrives exactly once. Twice is the same
/// schema spelled two ways, and the id would follow the spelling.
fn set_once<T>(dest: &mut Option<T>, value: T, what: &str) -> Result<(), String> {
    if dest.is_some() {
        return Err(format!("{what} is stated twice"));
    }

    *dest = Some(value);
    Ok(())
}

/// The walker fails at the same thing this reader does - the bytes are not what
/// they claim to be - so its reasons arrive here as text and stay text.
fn next(reader: &mut ProtobufReader) -> Result<Option<ProtobufField>, String> {
    reader.get_next().map_err(|err| err.to_string())
}

fn as_varint(field: &ProtobufField) -> Result<u64, String> {
    field.read_varint().map_err(|err| err.to_string())
}

/// An index into the message table, which is bounded by how many types somebody
/// wrote down rather than by anything on the wire.
fn read_u16(field: &ProtobufField) -> Result<u16, String> {
    let value = as_varint(field)?;

    u16::try_from(value).map_err(|_| format!("{value} does not fit the two bytes it is read into"))
}

/// A field number, which is bounded by protobuf and not by us.
fn read_u32(field: &ProtobufField) -> Result<u32, String> {
    let value = as_varint(field)?;

    u32::try_from(value).map_err(|_| format!("{value} does not fit the four bytes it is read into"))
}

fn as_string(field: &ProtobufField, src: &[u8]) -> Result<String, String> {
    field.read_string(src).map_err(|err| err.to_string())
}

fn as_message<'s>(field: &ProtobufField, src: &'s [u8]) -> Result<&'s [u8], String> {
    field.read_message_slice(src).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        Schema {
            root: 1,
            messages: vec![
                Message {
                    name: "Limits".to_string(),
                    fields: vec![
                        Field {
                            no: 1,
                            name: "MaxLots".to_string(),
                            tp: Tp::Item(ItemType::Scalar(Scalar::F64)),
                        },
                        Field {
                            no: 2,
                            name: "Allowed".to_string(),
                            tp: Tp::Array(ItemType::Scalar(Scalar::String)),
                        },
                    ],
                },
                Message {
                    name: "TraderEntity".to_string(),
                    fields: vec![
                        Field {
                            no: 1,
                            name: "PartitionKey".to_string(),
                            tp: Tp::Item(ItemType::Scalar(Scalar::String)),
                        },
                        Field {
                            no: 5,
                            name: "History".to_string(),
                            tp: Tp::Array(ItemType::Object(0)),
                        },
                    ],
                },
            ],
        }
    }

    #[test]
    fn a_schema_round_trips_through_its_bytes() {
        let bytes = schema().serialize();
        let restored = Schema::from_slice(&bytes).unwrap();

        assert_eq!(restored, schema());

        // And back out to the same bytes: what a stored row names by hashing them
        // has to survive being read and written again, or a server which
        // re-registered a schema from its own file would rename it.
        assert_eq!(restored.serialize(), bytes);
    }

    #[test]
    fn the_root_is_a_message_of_the_table_like_any_other() {
        let schema = schema();

        assert_eq!(schema.get_root().name, "TraderEntity");
        assert_eq!(schema.get_message(0).name, "Limits");

        // The reference in the root's repeated field is what points back at it.
        let field = schema.get_root().get_field(5).unwrap();
        assert_eq!(field.tp, Tp::Array(ItemType::Object(0)));
        assert!(schema.get_root().get_field(2).is_none());
    }

    /// A message which carries itself is a finite schema, because the reference
    /// is an index and not the message.
    #[test]
    fn a_message_which_carries_itself_is_finite() {
        let schema = Schema {
            root: 0,
            messages: vec![Message {
                name: "Node".to_string(),
                fields: vec![Field {
                    no: 1,
                    name: "Next".to_string(),
                    tp: Tp::Item(ItemType::Object(0)),
                }],
            }],
        };

        assert_eq!(Schema::from_slice(&schema.serialize()).unwrap(), schema);
    }

    /// A field number is protobuf's to bound, and protobuf bounds it at
    /// `2^29-1`, which `db_entity::ProtobufReader` already refuses past.
    /// Anything narrower here would be a limit of our own on a number the row
    /// parser happily hands out, and the row would then be stored under a
    /// number its own schema could not name.
    #[test]
    fn a_field_number_wider_than_two_bytes_is_still_a_field_number() {
        const HIGHEST: u32 = (1 << 29) - 1;

        for no in [u32::from(u16::MAX) + 1, HIGHEST] {
            let schema = Schema {
                root: 0,
                messages: vec![Message {
                    name: "M".to_string(),
                    fields: vec![Field {
                        no,
                        name: "F".to_string(),
                        tp: Tp::Item(ItemType::Scalar(Scalar::Bool)),
                    }],
                }],
            };

            let restored = Schema::from_slice(&schema.serialize()).unwrap();

            assert_eq!(restored, schema);
            assert_eq!(restored.get_root().get_field(no).unwrap().name, "F");
        }
    }

    #[test]
    fn every_scalar_survives_the_trip() {
        for scalar in [
            Scalar::Bool,
            Scalar::I32,
            Scalar::I64,
            Scalar::U32,
            Scalar::U64,
            Scalar::F32,
            Scalar::F64,
            Scalar::String,
            Scalar::Bytes,
        ] {
            for tp in [
                Tp::Item(ItemType::Scalar(scalar)),
                Tp::Array(ItemType::Scalar(scalar)),
            ] {
                let schema = Schema {
                    root: 0,
                    messages: vec![Message {
                        name: "M".to_string(),
                        fields: vec![Field {
                            no: 7,
                            name: "F".to_string(),
                            tp,
                        }],
                    }],
                };

                assert_eq!(Schema::from_slice(&schema.serialize()).unwrap(), schema);
            }
        }
    }

    /// The bytes arrive from whoever wrote a row, so every one of these is a
    /// blob some other client may send. Each of them would otherwise be a schema
    /// which serializes back into different bytes - that is, into a different id
    /// for the same shape.
    #[test]
    fn a_spelling_which_is_not_the_canonical_one_is_refused() {
        let refused = |bytes: Vec<u8>, because: &str| {
            let err = Schema::from_slice(&bytes)
                .expect_err(&format!("this should not have been read: {because}"));

            println!("{because}: {err}");
        };

        let mut messages_out_of_order = Vec::new();
        write_varint_field(&mut messages_out_of_order, SCHEMA_ROOT, 0);
        write_len_field(
            &mut messages_out_of_order,
            SCHEMA_MESSAGE,
            &serialize_message(&schema().messages[1]),
        );
        write_len_field(
            &mut messages_out_of_order,
            SCHEMA_MESSAGE,
            &serialize_message(&schema().messages[0]),
        );
        refused(messages_out_of_order, "messages are not sorted by name");

        let mut fields_out_of_order = Vec::new();
        write_len_field(&mut fields_out_of_order, MESSAGE_NAME, b"M");
        for no in [2u32, 1] {
            write_len_field(
                &mut fields_out_of_order,
                MESSAGE_FIELD,
                &serialize_field(&Field {
                    no,
                    name: "F".to_string(),
                    tp: Tp::Item(ItemType::Scalar(Scalar::Bool)),
                }),
            );
        }
        let mut out_of_order = Vec::new();
        write_varint_field(&mut out_of_order, SCHEMA_ROOT, 0);
        write_len_field(&mut out_of_order, SCHEMA_MESSAGE, &fields_out_of_order);
        refused(out_of_order, "fields are not sorted by number");

        let mut no_root = Vec::new();
        write_len_field(
            &mut no_root,
            SCHEMA_MESSAGE,
            &serialize_message(&schema().messages[0]),
        );
        refused(no_root, "nothing says which message is the root");

        let mut unknown_frame_field = schema().serialize();
        write_varint_field(&mut unknown_frame_field, 7, 1);
        refused(unknown_frame_field, "a schema has no field 7");

        let mut root_twice = schema().serialize();
        write_varint_field(&mut root_twice, SCHEMA_ROOT, 0);
        refused(root_twice, "the root is stated twice");
    }

    #[test]
    fn a_field_which_describes_nothing_is_refused() {
        let field = |body: Vec<u8>| {
            let mut message = Vec::new();
            write_len_field(&mut message, MESSAGE_NAME, b"M");
            write_len_field(&mut message, MESSAGE_FIELD, &body);

            let mut result = Vec::new();
            write_varint_field(&mut result, SCHEMA_ROOT, 0);
            write_len_field(&mut result, SCHEMA_MESSAGE, &message);
            result
        };

        let mut both = Vec::new();
        write_varint_field(&mut both, FIELD_NO, 1);
        write_len_field(&mut both, FIELD_NAME, b"F");
        write_varint_field(&mut both, FIELD_SCALAR, Scalar::Bool as u64);
        write_varint_field(&mut both, FIELD_OBJECT, 0);
        write_varint_field(&mut both, FIELD_ARRAY, 0);
        assert!(Schema::from_slice(&field(both)).is_err());

        let mut neither = Vec::new();
        write_varint_field(&mut neither, FIELD_NO, 1);
        write_len_field(&mut neither, FIELD_NAME, b"F");
        write_varint_field(&mut neither, FIELD_ARRAY, 0);
        assert!(Schema::from_slice(&field(neither)).is_err());

        let mut unknown_scalar = Vec::new();
        write_varint_field(&mut unknown_scalar, FIELD_NO, 1);
        write_len_field(&mut unknown_scalar, FIELD_NAME, b"F");
        write_varint_field(&mut unknown_scalar, FIELD_SCALAR, 10);
        write_varint_field(&mut unknown_scalar, FIELD_ARRAY, 0);
        assert!(Schema::from_slice(&field(unknown_scalar)).is_err());

        let mut zero = Vec::new();
        write_varint_field(&mut zero, FIELD_NO, 0);
        write_len_field(&mut zero, FIELD_NAME, b"F");
        write_varint_field(&mut zero, FIELD_SCALAR, Scalar::Bool as u64);
        write_varint_field(&mut zero, FIELD_ARRAY, 0);
        assert!(Schema::from_slice(&field(zero)).is_err());

        let mut arity = Vec::new();
        write_varint_field(&mut arity, FIELD_NO, 1);
        write_len_field(&mut arity, FIELD_NAME, b"F");
        write_varint_field(&mut arity, FIELD_SCALAR, Scalar::Bool as u64);
        write_varint_field(&mut arity, FIELD_ARRAY, 2);
        assert!(Schema::from_slice(&field(arity)).is_err());
    }

    /// A reference the renderer would follow off the end of the table. The rows
    /// are already stored by the time anybody looks at them, so this is refused
    /// where the bytes arrive.
    #[test]
    fn a_reference_to_a_message_which_is_not_there_is_refused() {
        let dangling = Schema {
            root: 0,
            messages: vec![Message {
                name: "M".to_string(),
                fields: vec![Field {
                    no: 1,
                    name: "F".to_string(),
                    tp: Tp::Item(ItemType::Object(3)),
                }],
            }],
        };

        assert!(Schema::from_slice(&dangling.serialize()).is_err());

        let mut no_such_root = Vec::new();
        write_varint_field(&mut no_such_root, SCHEMA_ROOT, 2);
        write_len_field(
            &mut no_such_root,
            SCHEMA_MESSAGE,
            &serialize_message(&dangling.messages[0]),
        );

        assert!(Schema::from_slice(&no_such_root).is_err());
    }

    /// Sorting here would repoint every `Object(..)` in the schema, so the order
    /// is a promise the builder keeps and this only checks.
    #[test]
    #[should_panic(expected = "the schema is not canonical")]
    fn serializing_messages_which_are_out_of_order_is_fatal() {
        let mut schema = schema();
        schema.messages.reverse();
        schema.serialize();
    }

    #[test]
    #[should_panic(expected = "is not canonical")]
    fn serializing_fields_which_are_out_of_order_is_fatal() {
        let mut schema = schema();
        schema.messages[0].fields.reverse();
        schema.serialize();
    }

    /// What the server files a resolved schema under. Two schemas which differ
    /// anywhere have to be two entries, or one of them would be shown through
    /// the other's field names.
    #[test]
    fn the_cache_key_follows_the_bytes() {
        let first = super::super::get_schema_id(&schema().serialize());

        // The same schema is the same key, in this process and in any other.
        assert_eq!(first, super::super::get_schema_id(&schema().serialize()));

        // A changed schema is a different schema - including one which only
        // renamed a field, because the name is what a row is shown under.
        let mut renamed = schema();
        renamed.messages[0].fields[0].name = "MaxLot".to_string();
        assert_ne!(first, super::super::get_schema_id(&renamed.serialize()));
    }
}
