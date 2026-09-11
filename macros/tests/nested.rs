//! A message an entity carries: what the macro generates for it, and what the
//! entity's schema says about it.

use my_no_sql_grpc_core::db_entity::ProtobufReader;
use my_no_sql_grpc_core::schemas::{ItemType, Message, Schema, Tp};
use my_no_sql_grpc_core::{MyNoSqlEntity, MyNoSqlMessage};
use my_no_sql_grpc_macros::{my_no_sql_entity, my_no_sql_message};

/// A message numbers its fields from 1: the four the entity contract reserves
/// are the entity's own, and a message carries no keys.
#[my_no_sql_message]
#[derive(Clone, PartialEq, Debug)]
pub struct Instrument {
    #[proto_no(1)]
    pub id: String,
    #[proto_no(2)]
    pub lots: f64,
}

#[my_no_sql_message]
#[derive(Clone, PartialEq, Debug)]
pub struct Limits {
    #[proto_no(1)]
    pub max_lots: f64,
    /// A message inside a message.
    #[proto_no(2)]
    pub default_instrument: Instrument,
    #[proto_no(3)]
    pub allowed: Vec<Instrument>,
}

#[my_no_sql_entity(table_name: "traders")]
#[derive(Clone, PartialEq, Debug)]
pub struct TraderEntity {
    #[proto_no(5)]
    pub limits: Limits,
    #[proto_no(6)]
    pub history: Vec<Instrument>,
    #[proto_no(7)]
    pub fallback: Option<Instrument>,
    #[proto_no(8)]
    pub note: String,
}

fn instrument(id: &str, lots: f64) -> Instrument {
    Instrument {
        id: id.to_string(),
        lots,
    }
}

fn filled() -> TraderEntity {
    TraderEntity {
        partition_key: "acc-1".to_string(),
        row_key: "rk".to_string(),
        time_stamp: 1_700_000_000_000_000,
        expires: 0,
        limits: Limits {
            max_lots: 12.5,
            default_instrument: instrument("EURUSD", 1.0),
            allowed: vec![instrument("EURUSD", 1.0), instrument("BTCUSD", 0.25)],
        },
        history: vec![instrument("GBPUSD", 3.0)],
        fallback: Some(instrument("USDJPY", 2.0)),
        note: "hello".to_string(),
    }
}

#[test]
fn a_nested_message_round_trips_at_every_depth() {
    let source = filled();

    let read_back = TraderEntity::from_slice(&source.to_vec()).unwrap();

    assert_eq!(read_back, source);
    assert_eq!(read_back.limits.allowed.len(), 2);
    assert_eq!(read_back.limits.default_instrument.id, "EURUSD");
    assert_eq!(read_back.fallback.unwrap().lots, 2.0);
}

/// A message has explicit presence in protobuf, so there is nothing to leave
/// out: a struct field is a value somebody put there. `Option<..>` is how "not
/// set" is spelled, and an absent one is what does not reach the wire.
#[test]
fn an_absent_optional_message_is_not_written_and_a_default_one_is() {
    let mut source = filled();
    source.fallback = None;
    source.limits = Limits::default();
    source.history = Vec::new();

    let bytes = source.to_vec();

    assert!(
        field_numbers(&bytes).contains(&5),
        "the message field is written even when it is the default"
    );
    assert!(!field_numbers(&bytes).contains(&6));
    assert!(!field_numbers(&bytes).contains(&7));

    assert_eq!(TraderEntity::from_slice(&bytes).unwrap(), source);
}

fn schema() -> Schema {
    Schema::from_slice(&<TraderEntity as MyNoSqlEntity>::get_schema().schema).unwrap()
}

fn carried(schema: &Schema, message: &Message, no: u32) -> String {
    match message.get_field(no).unwrap().tp.get_item() {
        ItemType::Object(index) => schema.get_message(index).name.clone(),
        ItemType::Scalar(_) => panic!("field {no} was supposed to carry a message"),
    }
}

/// The whole point of declaring them: the schema has to name the nested
/// messages, or a stored row can not be shown under its own field names.
#[test]
fn the_schema_declares_every_message_the_entity_carries() {
    let schema = schema();

    assert_eq!(schema.get_root().name, "TraderEntity");

    // The entity, the message it carries, and the message that one carries -
    // and `Instrument`, reached through three different fields, is one entry.
    // Sorted by name, because an index only means the same thing in two
    // processes if they put the table in the same order.
    assert_eq!(
        schema
            .messages
            .iter()
            .map(|itm| itm.name.as_str())
            .collect::<Vec<&str>>(),
        vec!["Instrument", "Limits", "TraderEntity"]
    );

    let root = schema.get_root();

    assert_eq!(carried(&schema, root, 5), "Limits");
    assert_eq!(carried(&schema, root, 6), "Instrument");
    assert_eq!(carried(&schema, root, 7), "Instrument");

    assert!(root.get_field(6).unwrap().tp.is_array());
    // `Option<T>` is one value: whether it was set is the client's business and
    // does not change what the field carries.
    assert!(!root.get_field(7).unwrap().tp.is_array());

    // ...and a scalar beside them names no message at all.
    assert_eq!(root.get_field(8).unwrap().name, "Note");
    assert!(matches!(
        root.get_field(8).unwrap().tp,
        Tp::Item(ItemType::Scalar(_))
    ));

    // `Instrument` is reached through `Limits` as well, and the reference from
    // there resolves to the same message - which is what proves a reference is
    // followed rather than guessed.
    let ItemType::Object(limits) = root.get_field(5).unwrap().tp.get_item() else {
        panic!("field 5 was supposed to carry a message");
    };

    let limits = schema.get_message(limits);
    assert_eq!(carried(&schema, limits, 2), "Instrument");
    assert_eq!(carried(&schema, limits, 3), "Instrument");
}

/// A message which changes shape has to reach the entity that carries it, or the
/// entity would keep an id whose rows no longer render the way the id promises.
/// Seen from out here that is three distinct constants, none of them derived
/// from the built bytes.
#[test]
fn every_type_gets_its_own_compile_time_id() {
    const ID: u64 = <TraderEntity as MyNoSqlEntity>::SCHEMA_ID;

    assert_eq!(<TraderEntity as MyNoSqlEntity>::get_schema().id, ID);

    assert_ne!(ID, <Limits as MyNoSqlMessage>::SCHEMA_ID);
    assert_ne!(
        <Limits as MyNoSqlMessage>::SCHEMA_ID,
        <Instrument as MyNoSqlMessage>::SCHEMA_ID
    );
}

/// A message declared but never carried by the entity has no business in its
/// schema.
#[my_no_sql_message]
pub struct Unused {
    #[proto_no(1)]
    pub whatever: String,
}

#[test]
fn a_message_nobody_carries_is_not_declared() {
    assert!(!schema().messages.iter().any(|itm| itm.name == "Unused"));

    // It is still a message in its own right - it simply travels with whatever
    // entity carries it, and nothing does.
    assert_eq!(<Unused as MyNoSqlMessage>::MESSAGE_NAME, "Unused");
}

fn field_numbers(src: &[u8]) -> Vec<u32> {
    let mut reader = ProtobufReader::new(src);
    let mut result = Vec::new();

    while let Some(field) = reader.get_next().unwrap() {
        result.push(field.field_no);
    }

    result
}
