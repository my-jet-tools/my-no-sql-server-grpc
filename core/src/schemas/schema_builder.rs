//! Declaring a [`Schema`] from the outside, where indices do not exist yet.
//!
//! This is what the entity macro expands into, so it runs inside whatever crate
//! declared the entity - the same reason the serializer lives in the core rather
//! than in the server: both sides have to mean the same bytes by the same
//! entity, and the way to guarantee that is one implementation.

use super::{Field, ItemType, Message, Scalar, Schema, Tp};

/// What a field carries, named the way whoever declares it is able to name it.
///
/// A message does not know the index it will get - indices exist only once every
/// message is known and put in order - so a declaration points at one by name and
/// [`SchemaBuilder::build`] turns that into an [`ItemType::Object`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DeclaredItemType {
    Scalar(Scalar),
    Object(String),
}

/// [`Tp`] before the names became indices. Nested the same way and for the same
/// reason: an array of arrays and an array of nothing can not be written down.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DeclaredTp {
    Item(DeclaredItemType),
    Array(DeclaredItemType),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeclaredField {
    pub no: u32,
    pub name: String,
    pub tp: DeclaredTp,
}

impl DeclaredField {
    pub fn scalar(name: &str, no: u32, scalar: Scalar, is_array: bool) -> Self {
        Self::new(name, no, DeclaredItemType::Scalar(scalar), is_array)
    }

    pub fn object(name: &str, no: u32, message_name: &str, is_array: bool) -> Self {
        Self::new(
            name,
            no,
            DeclaredItemType::Object(message_name.to_string()),
            is_array,
        )
    }

    fn new(name: &str, no: u32, item: DeclaredItemType, is_array: bool) -> Self {
        Self {
            no,
            name: name.to_string(),
            tp: if is_array {
                DeclaredTp::Array(item)
            } else {
                DeclaredTp::Item(item)
            },
        }
    }
}

struct DeclaredMessage {
    name: String,
    fields: Vec<DeclaredField>,
}

/// Builds the schema of one entity.
///
/// A message a field carries is declared **beside** the entity rather than
/// inside it: one message reached through two fields is then one declaration and
/// one index, and the message a field points at does not depend on which field
/// it was reached through.
///
/// Everything here fails by panicking rather than by `Result`. The schema is
/// built once per type inside a `OnceLock`, so there is no caller to hand an
/// error to, and the first write says so loudly.
pub struct SchemaBuilder {
    root: DeclaredMessage,
    messages: Vec<DeclaredMessage>,
}

impl SchemaBuilder {
    pub fn new(root_message_name: &str) -> Self {
        Self {
            root: DeclaredMessage {
                name: root_message_name.to_string(),
                fields: Vec::new(),
            },
            messages: Vec::new(),
        }
    }

    /// A field of the entity itself.
    pub fn add_field(mut self, field: DeclaredField) -> Self {
        self.root.fields.push(field);
        self
    }

    /// Declares a message some field carries.
    ///
    /// Callers ask [`Self::has_message`] first and only declare what is not there
    /// yet, so declaring the same one twice is free: the same message reached
    /// through two fields is the same message.
    ///
    /// That is also what would make a message carrying itself terminate here -
    /// the name is registered before its own fields are walked. Nothing produces
    /// such a declaration any more, because the macro folds a `const SCHEMA_ID`
    /// which can not name itself, but this stays the shape it is: the format
    /// itself is perfectly able to *describe* one, since a reference is an index
    /// rather than the message, and the renderer meets such schemas as bytes.
    ///
    /// Two *different* messages under one name is the opposite case and is fatal.
    /// A declaration is deduplicated by name and by nothing else, so the second
    /// one would be dropped and both fields would be shown through the first - a
    /// `f64` rendered as a `String`, with nobody told. Rust lets two crates, or
    /// two modules, spell one struct name, and this is the only place where that
    /// stops being harmless.
    pub fn add_message(mut self, name: &str, fields: Vec<DeclaredField>) -> Self {
        assert!(
            name != self.root.name,
            "message '{name}' is declared under the name of the entity, and a name is all a declaration is told apart by"
        );

        let declared = canonical(DeclaredMessage {
            name: name.to_string(),
            fields,
        });

        if let Some(already) = self.messages.iter().find(|itm| itm.name == name) {
            assert!(
                already.fields == declared.fields,
                "two different messages are declared as '{name}' - a schema tells them apart by name and by nothing else"
            );

            return self;
        }

        self.messages.push(declared);

        self
    }

    pub fn has_message(&self, name: &str) -> bool {
        self.messages.iter().any(|itm| itm.name == name)
    }

    /// Puts the messages in order, hands out the indices, and resolves every
    /// reference against them.
    ///
    /// This is the only place a message table is sorted, and it has to be: an
    /// index is a position in that table, so a sort anywhere downstream would
    /// repoint every reference at a different message. Everything after this
    /// point takes the canonical order as given - which is what makes the same
    /// entity, declared in whatever order its fields and messages happen to come
    /// in, hash to the same id.
    pub fn build(self) -> Schema {
        let root_name = self.root.name.clone();

        let mut declared = Vec::with_capacity(self.messages.len() + 1);
        declared.push(canonical(self.root));
        declared.extend(self.messages);

        // By bytes, not by anybody's locale: the id is the hash of what comes out
        // of here, and two machines which sorted differently would disagree on it.
        declared.sort_by(|left, right| left.name.cmp(&right.name));

        assert!(
            declared.len() <= usize::from(u16::MAX) + 1,
            "an entity of {} messages has more of them than an index can name",
            declared.len()
        );

        let messages = declared
            .iter()
            .map(|message| Message {
                name: message.name.clone(),
                fields: message
                    .fields
                    .iter()
                    .map(|field| Field {
                        no: field.no,
                        name: field.name.clone(),
                        tp: resolve(field, &message.name, &declared),
                    })
                    .collect(),
            })
            .collect();

        Schema {
            root: index_of(&root_name, &declared)
                .expect("the root is one of the messages it was put among"),
            messages,
        }
    }
}

/// Sorts the fields by number - the other half of what makes the serialized form
/// canonical - and refuses two fields under one number, which is a field the
/// reader could never reach because a number is how it finds one.
///
/// Done as the message is declared rather than at the end, so that the shape two
/// declarations are compared by is the canonical one: the same message declared
/// with its fields written down in a different order is the same message.
fn canonical(mut message: DeclaredMessage) -> DeclaredMessage {
    message.fields.sort_by_key(|field| field.no);

    for field in message.fields.iter() {
        assert!(
            field.no != 0,
            "field '{}' of '{}' is numbered 0, which is not a number protobuf has",
            field.name,
            message.name
        );
    }

    for pair in message.fields.windows(2) {
        assert!(
            pair[0].no != pair[1].no,
            "fields '{}' and '{}' of '{}' are both numbered {}",
            pair[0].name,
            pair[1].name,
            message.name,
            pair[0].no
        );
    }

    message
}

fn resolve(field: &DeclaredField, declared_in: &str, messages: &[DeclaredMessage]) -> Tp {
    let (item, is_array) = match &field.tp {
        DeclaredTp::Item(item) => (item, false),
        DeclaredTp::Array(item) => (item, true),
    };

    let item = match item {
        DeclaredItemType::Scalar(scalar) => ItemType::Scalar(*scalar),
        DeclaredItemType::Object(name) => {
            let Some(index) = index_of(name, messages) else {
                panic!(
                    "field '{}' of '{declared_in}' carries message '{name}', which nothing declared",
                    field.name
                );
            };

            ItemType::Object(index)
        }
    };

    if is_array {
        Tp::Array(item)
    } else {
        Tp::Item(item)
    }
}

/// The messages are sorted by name by the time anything asks, so this is the
/// search the order was paid for.
fn index_of(name: &str, messages: &[DeclaredMessage]) -> Option<u16> {
    let found = messages
        .binary_search_by(|message| message.name.as_str().cmp(name))
        .ok()?;

    Some(found as u16)
}

#[cfg(test)]
mod tests {
    use crate::schemas::get_schema_id;

    use super::*;

    /// The whole point of the format. Two processes declaring one entity have no
    /// reason to walk its fields or its messages in the same order, and the id
    /// every stored row carries is the hash of these bytes.
    #[test]
    fn the_same_entity_declared_in_two_orders_is_the_same_bytes() {
        let limits = || {
            vec![
                DeclaredField::scalar("MaxLots", 1, Scalar::F64, false),
                DeclaredField::object("Allowed", 2, "Instrument", true),
            ]
        };

        let instrument = || vec![DeclaredField::scalar("Id", 1, Scalar::String, false)];

        let one = SchemaBuilder::new("TraderEntity")
            .add_field(DeclaredField::scalar(
                "PartitionKey",
                1,
                Scalar::String,
                false,
            ))
            .add_field(DeclaredField::object("Limits", 5, "Limits", false))
            .add_field(DeclaredField::scalar("Amount", 6, Scalar::F64, false))
            .add_message("Limits", limits())
            .add_message("Instrument", instrument())
            .build();

        let other = SchemaBuilder::new("TraderEntity")
            .add_field(DeclaredField::scalar("Amount", 6, Scalar::F64, false))
            .add_field(DeclaredField::object("Limits", 5, "Limits", false))
            .add_field(DeclaredField::scalar(
                "PartitionKey",
                1,
                Scalar::String,
                false,
            ))
            .add_message("Instrument", instrument())
            .add_message(
                "Limits",
                limits().into_iter().rev().collect::<Vec<DeclaredField>>(),
            )
            .build();

        assert_eq!(one, other);
        assert_eq!(one.serialize(), other.serialize());
        assert_eq!(
            get_schema_id(&one.serialize()),
            get_schema_id(&other.serialize())
        );

        // And the reference survived the sorting: it names the message, not the
        // position it happened to be declared at.
        let ItemType::Object(index) = one.get_root().get_field(5).unwrap().tp.get_item() else {
            panic!("the field carries a message");
        };

        assert_eq!(one.get_message(index).name, "Limits");
    }

    #[test]
    fn the_built_schema_is_canonical_and_reads_back() {
        let schema = SchemaBuilder::new("TraderEntity")
            .add_field(DeclaredField::scalar("Tags", 6, Scalar::String, true))
            .add_field(DeclaredField::scalar("Amount", 5, Scalar::F64, false))
            .add_message(
                "Limits",
                vec![DeclaredField::scalar("Max", 1, Scalar::I32, false)],
            )
            .build();

        assert_eq!(
            schema
                .messages
                .iter()
                .map(|itm| itm.name.as_str())
                .collect::<Vec<&str>>(),
            vec!["Limits", "TraderEntity"]
        );

        assert_eq!(schema.get_root().name, "TraderEntity");
        assert_eq!(
            schema.get_root().fields[0].tp,
            Tp::Item(ItemType::Scalar(Scalar::F64))
        );
        assert_eq!(
            schema.get_root().fields[1].tp,
            Tp::Array(ItemType::Scalar(Scalar::String))
        );

        assert_eq!(Schema::from_slice(&schema.serialize()).unwrap(), schema);
    }

    /// Two messages carrying each other terminate because the name is registered
    /// before the fields are walked, and the reference is an index rather than
    /// the message itself. No macro emits this any more - a `const` can not name
    /// itself - but the format has to go on describing it, because a schema
    /// reaches this server as bytes rather than as a declaration.
    #[test]
    fn messages_which_carry_each_other_are_declared_once_each() {
        let mut builder = SchemaBuilder::new("TraderEntity")
            .add_field(DeclaredField::object("Left", 5, "Left", false));

        if !builder.has_message("Left") {
            builder = builder.add_message(
                "Left",
                vec![DeclaredField::object("Right", 1, "Right", false)],
            );

            if !builder.has_message("Right") {
                builder = builder.add_message(
                    "Right",
                    vec![DeclaredField::object("Left", 1, "Left", true)],
                );
            }
        }

        // Declaring it a second time is what the generated code does when a
        // second field reaches the same message, and it costs nothing.
        let schema = builder
            .add_message(
                "Left",
                vec![DeclaredField::object("Right", 1, "Right", false)],
            )
            .build();

        assert_eq!(schema.messages.len(), 3);

        let left = schema.get_message(0);
        assert_eq!(left.name, "Left");
        assert_eq!(left.fields[0].tp, Tp::Item(ItemType::Object(1)));
        assert_eq!(
            schema.get_message(1).fields[0].tp,
            Tp::Array(ItemType::Object(0))
        );
    }

    /// Rust lets two crates spell one struct name. Keeping the first shape and
    /// dropping the second showed one message's field through the other's type,
    /// and said nothing.
    #[test]
    #[should_panic(expected = "two different messages are declared as 'Limits'")]
    fn two_shapes_under_one_message_name_are_fatal() {
        SchemaBuilder::new("TraderEntity")
            .add_message(
                "Limits",
                vec![DeclaredField::scalar("Max", 1, Scalar::F64, false)],
            )
            .add_message(
                "Limits",
                vec![DeclaredField::scalar("Max", 1, Scalar::String, false)],
            );
    }

    /// The entity is a message of the same table, so a message wearing its name
    /// is the same collision with the same silent outcome.
    #[test]
    #[should_panic(expected = "under the name of the entity")]
    fn a_message_named_after_the_entity_is_fatal() {
        SchemaBuilder::new("TraderEntity").add_message("TraderEntity", vec![]);
    }

    #[test]
    #[should_panic(expected = "which nothing declared")]
    fn a_field_carrying_a_message_nobody_declared_is_fatal() {
        SchemaBuilder::new("TraderEntity")
            .add_field(DeclaredField::object("Limits", 5, "Limits", false))
            .build();
    }

    #[test]
    #[should_panic(expected = "are both numbered 5")]
    fn two_fields_under_one_number_are_fatal() {
        SchemaBuilder::new("TraderEntity")
            .add_field(DeclaredField::scalar("Amount", 5, Scalar::F64, false))
            .add_field(DeclaredField::scalar("Other", 5, Scalar::F64, false))
            .build();
    }

    #[test]
    #[should_panic(expected = "is not a number protobuf has")]
    fn a_field_numbered_zero_is_fatal() {
        SchemaBuilder::new("TraderEntity")
            .add_field(DeclaredField::scalar("Amount", 0, Scalar::F64, false))
            .build();
    }
}
