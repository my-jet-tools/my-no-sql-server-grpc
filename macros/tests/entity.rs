//! What the macro generates, exercised against the core's own protobuf reader.
//!
//! It lives here rather than in the core because the core is what the generated
//! code refers to by name - it can not expand the macro over itself.

use my_no_sql_grpc_core::MyNoSqlEntity;
use my_no_sql_grpc_core::db_entity::{ParsedEntity, write_len_field, write_varint_field};
use my_no_sql_grpc_core::schemas::{ItemType, Scalar, Schema, Tp};
use my_no_sql_grpc_macros::my_no_sql_entity;

#[my_no_sql_entity(table_name: "everything")]
#[derive(Clone, PartialEq, Debug)]
pub struct EverythingEntity {
    #[proto_no(5)]
    pub text: String,
    #[proto_no(6)]
    pub flag: bool,
    #[proto_no(7)]
    pub small: i32,
    #[proto_no(8)]
    pub big: i64,
    #[proto_no(9)]
    pub unsigned: u64,
    #[proto_no(10)]
    pub single: f32,
    #[proto_no(11)]
    pub double: f64,
    #[proto_no(12)]
    pub blob: Vec<u8>,
    #[proto_no(13)]
    pub tags: Vec<String>,
    #[proto_no(14)]
    pub numbers: Vec<i64>,
    #[proto_no(15)]
    pub maybe: Option<i32>,
    /// `Vec<u8>` is `bytes` wherever it stands, so these are an optional byte
    /// string and a repeated one - not a nested repeated number, which protobuf
    /// has no way to spell anyway.
    #[proto_no(16)]
    pub maybe_blob: Option<Vec<u8>>,
    #[proto_no(17)]
    pub blobs: Vec<Vec<u8>>,
    /// Not marked, so it is a field of the struct and nothing more.
    pub never_stored: String,
}

fn filled() -> EverythingEntity {
    EverythingEntity {
        partition_key: "acc-1".to_string(),
        row_key: "rk".to_string(),
        time_stamp: 1_700_000_000_000_000,
        expires: 1_800_000_000_000_000,
        text: "hello".to_string(),
        flag: true,
        small: -7,
        big: -9_000_000_000,
        unsigned: u64::MAX,
        single: 1.5,
        double: -2.25,
        blob: vec![1, 2, 3, 0, 255],
        tags: vec!["a".to_string(), "b".to_string()],
        numbers: vec![1, -2, 3],
        maybe: Some(-1),
        maybe_blob: Some(vec![9, 0, 9]),
        // An empty element among them: a repeated field writes every occurrence,
        // default or not, so it has to come back as it went.
        blobs: vec![vec![1, 2], Vec::new(), vec![255]],
        never_stored: "not on the wire".to_string(),
    }
}

#[test]
fn every_supported_type_round_trips() {
    let source = filled();

    let mut restored = EverythingEntity::from_slice(&source.to_vec()).unwrap();

    // The one field with no proto_no came back as the default, which is the
    // whole point of leaving it unmarked.
    assert_eq!(restored.never_stored, "");
    restored.never_stored = source.never_stored.clone();

    assert_eq!(restored, source);
}

/// `Vec<u8>` inside `Option<>` and inside `Vec<>` is what the contract says is
/// usable, and a byte string nested that way used to fall into the "unknown type
/// is a message" branch and be refused.
#[test]
fn a_byte_string_is_bytes_at_any_depth() {
    let restored = EverythingEntity::from_slice(&filled().to_vec()).unwrap();

    assert_eq!(restored.maybe_blob, Some(vec![9, 0, 9]));
    assert_eq!(restored.blobs, vec![vec![1, 2], Vec::new(), vec![255]]);

    // "Not set" and "set to nothing" stay apart, the same as for any other
    // optional field.
    let mut empty = filled();
    empty.maybe_blob = None;
    assert_eq!(
        EverythingEntity::from_slice(&empty.to_vec())
            .unwrap()
            .maybe_blob,
        None
    );

    empty.maybe_blob = Some(Vec::new());
    assert_eq!(
        EverythingEntity::from_slice(&empty.to_vec())
            .unwrap()
            .maybe_blob,
        Some(Vec::new())
    );
}

/// The four reserved fields have to be where the contract says, or the server
/// would not find them without the schema.
#[test]
fn the_reserved_fields_land_on_1_to_4() {
    let parsed = ParsedEntity::parse(&filled().to_vec()).unwrap();

    assert_eq!(parsed.get_partition_key(), "acc-1");
    assert_eq!(parsed.get_row_key(), "rk");
    assert_eq!(parsed.time_stamp, Some(1_700_000_000_000_000));
    assert_eq!(parsed.expires, 1_800_000_000_000_000);
}

/// Proto3 leaves a default value off the wire, and so does this - which is what
/// keeps `TimeStamp` absent until somebody sets it, letting the server stamp its
/// own clock.
#[test]
fn defaults_are_not_written() {
    let entity = EverythingEntity {
        partition_key: "pk".to_string(),
        row_key: "rk".to_string(),
        ..Default::default()
    };

    let bytes = entity.to_vec();

    // Only PartitionKey and RowKey: tag, length and the two values.
    assert_eq!(bytes.len(), (1 + 1 + 2) * 2);

    // ...and an Option which is Some(0) is written all the same, because that is
    // the difference between zero and not set.
    let mut with_zero = entity.clone();
    with_zero.maybe = Some(0);
    assert!(with_zero.to_vec().len() > bytes.len());
    assert_eq!(
        EverythingEntity::from_slice(&with_zero.to_vec())
            .unwrap()
            .maybe,
        Some(0)
    );
}

/// A row written by a build which knew a field this one does not still has to
/// be readable - that is what lets two versions of an application run at once.
#[test]
fn a_field_this_build_does_not_know_is_skipped() {
    let mut bytes = filled().to_vec();
    write_len_field(&mut bytes, 900, b"from a newer build");
    write_varint_field(&mut bytes, 901, 42);

    let restored = EverythingEntity::from_slice(&bytes).unwrap();

    assert_eq!(restored.text, "hello");
}

/// protoc packs a repeated numeric by default, so a row written by a generated
/// client arrives that way even though this one writes them one by one.
#[test]
fn a_packed_repeated_field_is_understood() {
    let mut bytes = Vec::new();
    write_len_field(&mut bytes, 1, b"pk");
    write_len_field(&mut bytes, 2, b"rk");

    let mut packed = Vec::new();
    for value in [1u64, 2, 3] {
        my_no_sql_grpc_core::db_entity::write_varint(&mut packed, value);
    }
    write_len_field(&mut bytes, 14, &packed);

    let restored = EverythingEntity::from_slice(&bytes).unwrap();

    assert_eq!(restored.numbers, vec![1, 2, 3]);
}

/// A field number this entity knows, arriving encoded as something else, was
/// written under a different schema. Saying so beats defaulting it silently.
#[test]
fn a_known_field_with_the_wrong_encoding_is_refused() {
    let mut bytes = Vec::new();
    write_len_field(&mut bytes, 1, b"pk");
    write_len_field(&mut bytes, 2, b"rk");
    // Field 5 is a string here, and this is a varint.
    write_varint_field(&mut bytes, 5, 1);

    assert!(EverythingEntity::from_slice(&bytes).is_err());
}

#[test]
fn the_table_name_comes_from_the_entity() {
    assert_eq!(EverythingEntity::TABLE_NAME, "everything");
    assert_eq!(schema().get_root().name, "EverythingEntity");
}

fn schema() -> Schema {
    Schema::from_slice(&<EverythingEntity as MyNoSqlEntity>::get_schema().schema).unwrap()
}

/// Every Rust type the macro accepts, and what the schema says it is. This is
/// the whole mapping in one place: what the schema calls a field is what the
/// server shows it as, so a type quietly described as another one is a column
/// rendered as something it is not.
#[test]
fn every_declared_type_reaches_the_schema_as_itself() {
    let schema = schema();
    let root = schema.get_root();

    let tp = |no: u32| root.get_field(no).unwrap().tp;

    assert_eq!(tp(5), Tp::Item(ItemType::Scalar(Scalar::String)));
    assert_eq!(tp(6), Tp::Item(ItemType::Scalar(Scalar::Bool)));
    assert_eq!(tp(7), Tp::Item(ItemType::Scalar(Scalar::I32)));
    assert_eq!(tp(8), Tp::Item(ItemType::Scalar(Scalar::I64)));
    assert_eq!(tp(9), Tp::Item(ItemType::Scalar(Scalar::U64)));
    assert_eq!(tp(10), Tp::Item(ItemType::Scalar(Scalar::F32)));
    assert_eq!(tp(11), Tp::Item(ItemType::Scalar(Scalar::F64)));
    // `Vec<u8>` is one byte string, not a run of numbers - at any depth.
    assert_eq!(tp(12), Tp::Item(ItemType::Scalar(Scalar::Bytes)));
    assert_eq!(tp(13), Tp::Array(ItemType::Scalar(Scalar::String)));
    assert_eq!(tp(14), Tp::Array(ItemType::Scalar(Scalar::I64)));
    // `Option<T>` is one value: presence is the client's business and does not
    // change what the field carries.
    assert_eq!(tp(15), Tp::Item(ItemType::Scalar(Scalar::I32)));
    assert_eq!(tp(16), Tp::Item(ItemType::Scalar(Scalar::Bytes)));
    assert_eq!(tp(17), Tp::Array(ItemType::Scalar(Scalar::Bytes)));

    // The field with no `proto_no` is a field of the struct and nothing else.
    assert!(root.get_field(18).is_none());
    assert!(!root.fields.iter().any(|field| field.name == "NeverStored"));
}

/// PartitionKey and RowKey are described as ordinary fields, which is what keeps
/// the renderer from having to know that any number is reserved. TimeStamp and
/// Expires are not described at all: they are not in the stored row - the server
/// owns them and puts them back on the way out - so a schema naming them would
/// describe something that is never there.
#[test]
fn the_schema_describes_the_two_keys_and_neither_moment() {
    let schema = schema();
    let root = schema.get_root();

    assert_eq!(root.get_field(1).unwrap().name, "PartitionKey");
    assert_eq!(
        root.get_field(1).unwrap().tp,
        Tp::Item(ItemType::Scalar(Scalar::String))
    );
    assert_eq!(root.get_field(2).unwrap().name, "RowKey");

    assert!(root.get_field(3).is_none());
    assert!(root.get_field(4).is_none());
}

/// The id is a `const`, so it can be written down where only a constant is
/// allowed - and it is the one the schema travels under, without anybody hashing
/// anything at run time.
#[test]
fn the_schema_id_is_a_compile_time_constant() {
    const ID: u64 = <EverythingEntity as MyNoSqlEntity>::SCHEMA_ID;

    assert_ne!(ID, 0);
    assert_eq!(<EverythingEntity as MyNoSqlEntity>::get_schema().id, ID);
}

/// An entity of the same shape under a different name is a different entity: the
/// name is what a row is shown as, and two of them sharing an id would be one
/// table's rows rendered through the other's field names.
#[my_no_sql_entity(table_name: "twin")]
#[derive(Default)]
pub struct TwinEntity {
    #[proto_no(5)]
    pub text: String,
}

#[my_no_sql_entity(table_name: "other-twin")]
#[derive(Default)]
pub struct OtherTwinEntity {
    #[proto_no(5)]
    pub text: String,
}

#[test]
fn two_entities_of_one_shape_under_two_names_are_two_ids() {
    assert_ne!(
        <TwinEntity as MyNoSqlEntity>::SCHEMA_ID,
        <OtherTwinEntity as MyNoSqlEntity>::SCHEMA_ID
    );
}
