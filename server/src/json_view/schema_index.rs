use ahash::AHashMap;
use my_no_sql_grpc_abstractions::schemas::{ItemType, Scalar, Schema};

/// What a stored row's bytes mean, resolved once out of the schema the client
/// sent with its entity.
///
/// The server never needs this to store or to serve a row - the four reserved
/// fields are found by walking the wire format. It needs it to *show* one:
/// without field names and types, protobuf bytes render as numbered fields of
/// guessed type.
pub struct SchemaIndex {
    root_message: String,
    messages: AHashMap<String, MessageSchema>,
}

pub struct MessageSchema {
    /// Field number -> what that field is.
    pub fields: AHashMap<u32, FieldSchema>,
    /// Field name -> its number. The other direction of the same table, built
    /// here rather than per call because a write arrives as names and a stored
    /// row as numbers, and both are answered out of the one cached index.
    by_name: AHashMap<String, u32>,
}

impl MessageSchema {
    pub fn get_field_by_name(&self, name: &str) -> Option<(u32, &FieldSchema)> {
        let no = *self.by_name.get(name)?;
        Some((no, self.fields.get(&no)?))
    }

    /// Every name this message declares, in the order the schema declares them -
    /// which is by field number, so it reads like the entity does. Handed to a
    /// caller who named a field this message does not have.
    pub fn get_field_names(&self) -> Vec<&str> {
        let mut numbers: Vec<u32> = self.fields.keys().copied().collect();
        numbers.sort_unstable();

        numbers
            .into_iter()
            .filter_map(|no| self.fields.get(&no).map(|field| field.name.as_str()))
            .collect()
    }
}

pub struct FieldSchema {
    pub name: String,
    pub kind: FieldKind,
    pub repeated: bool,
}

/// The nine types a value can have, and a reference to a message.
///
/// Nine and not protobuf's fifteen, because the schema format has nine: the
/// macro is the only thing which produces a schema, it maps Rust onto exactly
/// these, and `sint*`, `fixed*`, `sfixed*`, enums and maps can not be written
/// down at all. They used to be here because a `FileDescriptorSet` could carry
/// them, and a branch nothing can reach is a branch nothing can test.
#[derive(Clone, PartialEq, Eq)]
pub enum FieldKind {
    Double,
    Float,
    Int64,
    Uint64,
    Int32,
    Uint32,
    Bool,
    String,
    Bytes,
    /// The message this field carries, by name - which is unique inside one
    /// schema by construction.
    Message(String),
}

impl FieldKind {
    /// A scalar may arrive packed - several values inside one length-delimited
    /// field - which is what a generated client emits for a repeated numeric.
    ///
    /// `Bytes` being unpackable is what keeps a `Vec<u8>` a base64 string
    /// instead of an array of numbers.
    pub fn is_packable(&self) -> bool {
        !matches!(
            self,
            FieldKind::String | FieldKind::Bytes | FieldKind::Message(_)
        )
    }
}

impl SchemaIndex {
    /// Flattens a schema into what the renderer asks of it.
    ///
    /// A reference in the schema is an **index** into the message table while
    /// this is keyed by **name**, so every reference is resolved once here rather
    /// than once per row. Names need no qualifying either: a schema holds one
    /// message per name by construction, so there is no package to put in front
    /// of one.
    pub fn build(schema: &[u8]) -> Result<Self, String> {
        let schema = Schema::from_slice(schema)?;

        let mut messages = AHashMap::with_capacity(schema.messages.len());

        for message in schema.messages.iter() {
            let mut fields = AHashMap::with_capacity(message.fields.len());
            let mut by_name = AHashMap::with_capacity(message.fields.len());

            for field in message.fields.iter() {
                by_name.insert(field.name.clone(), field.no);

                fields.insert(
                    field.no,
                    FieldSchema {
                        name: field.name.clone(),
                        kind: to_kind(field.tp.get_item(), &schema),
                        repeated: field.tp.is_array(),
                    },
                );
            }

            messages.insert(message.name.clone(), MessageSchema { fields, by_name });
        }

        Ok(Self {
            root_message: schema.get_root().name.clone(),
            messages,
        })
    }

    pub fn get_root_message_name(&self) -> &str {
        &self.root_message
    }

    pub fn get_message(&self, name: &str) -> Option<&MessageSchema> {
        self.messages.get(name)
    }
}

/// What one value of the schema is, in the terms the renderer speaks.
///
/// Both matches are spelled out to the end with no arm for "anything else", so a
/// type added to the format is a compile error here rather than a column which
/// quietly comes out as something it is not. It is total in both directions now
/// that `FieldKind` describes this format and nothing wider.
fn to_kind(item: ItemType, schema: &Schema) -> FieldKind {
    match item {
        ItemType::Scalar(scalar) => match scalar {
            Scalar::Bool => FieldKind::Bool,
            Scalar::I32 => FieldKind::Int32,
            Scalar::I64 => FieldKind::Int64,
            Scalar::U32 => FieldKind::Uint32,
            Scalar::U64 => FieldKind::Uint64,
            Scalar::F32 => FieldKind::Float,
            Scalar::F64 => FieldKind::Double,
            Scalar::String => FieldKind::String,
            Scalar::Bytes => FieldKind::Bytes,
        },
        ItemType::Object(index) => FieldKind::Message(schema.get_message(index).name.clone()),
    }
}
