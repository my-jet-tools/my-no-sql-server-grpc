//! The schema is built here through `SchemaBuilder`, which is exactly what the
//! entity macro expands into - so these tests exercise the shape a client's
//! schema arrives in, without a client in the picture.

use my_no_sql_grpc_core::db_entity::{consts, write_varint};
use my_no_sql_grpc_core::schemas::{DeclaredField, Scalar, SchemaBuilder};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use my_json::json_writer::JsonObjectWriter;

use super::{SchemaIndex, write_row_as_json};

const TIME_STAMP: i64 = 1_700_000_000_000_000;
const EXPIRES: i64 = 1_800_000_000_000_000;

/// One entity carrying everything the macro can produce, because everything it
/// can produce is what the renderer has to keep working on: all nine scalars, a
/// carried message, a message carried through *that* one, an array of messages,
/// numeric arrays in their packed spelling at each of the three widths, a
/// `Vec<u8>` (which is `bytes`, not an array of numbers), and - by its absence -
/// a field number the schema does not know.
///
/// ```text
/// message Instrument { string Id = 1; }
/// message Limits { double MaxLots = 1; string Currency = 2; Instrument Best = 3; }
///
/// message TraderEntity {
///     string PartitionKey = 1;  string RowKey = 2;
///     bool  Flag = 5;  int32  Small  = 6;  int64  Big   = 7;
///     uint32 USmall = 8;  uint64 UBig = 9;  float  Ratio = 10;
///     double Amount = 11;  string Name = 12;  bytes Blob = 13;
///     Limits Limits = 14;  repeated Limits History = 15;
///     repeated int32 Counts = 16;  repeated double Prices = 17;
///     repeated string Tags = 18;   repeated float Rates = 19;
/// }
/// ```
///
/// `TimeStamp` (3) and `Expires` (4) are deliberately not described: they are not
/// in a stored row, the server puts them back on the way out, and this is where
/// that shows.
///
/// Declared out of order on purpose - the builder is what puts messages and
/// fields into the canonical order, and a render that depended on the order they
/// were written down in would be a render that changed with the source file.
pub(crate) fn build_schema() -> Vec<u8> {
    SchemaBuilder::new("TraderEntity")
        .add_field(DeclaredField::scalar("Blob", 13, Scalar::Bytes, false))
        .add_field(DeclaredField::scalar("Name", 12, Scalar::String, false))
        .add_field(DeclaredField::scalar("Amount", 11, Scalar::F64, false))
        .add_field(DeclaredField::scalar("Ratio", 10, Scalar::F32, false))
        .add_field(DeclaredField::scalar("UBig", 9, Scalar::U64, false))
        .add_field(DeclaredField::scalar("USmall", 8, Scalar::U32, false))
        .add_field(DeclaredField::scalar("Big", 7, Scalar::I64, false))
        .add_field(DeclaredField::scalar("Small", 6, Scalar::I32, false))
        .add_field(DeclaredField::scalar("Flag", 5, Scalar::Bool, false))
        .add_field(DeclaredField::scalar("RowKey", 2, Scalar::String, false))
        .add_field(DeclaredField::scalar(
            "PartitionKey",
            1,
            Scalar::String,
            false,
        ))
        .add_field(DeclaredField::object("Limits", 14, "Limits", false))
        .add_field(DeclaredField::object("History", 15, "Limits", true))
        .add_field(DeclaredField::scalar("Counts", 16, Scalar::I32, true))
        .add_field(DeclaredField::scalar("Prices", 17, Scalar::F64, true))
        .add_field(DeclaredField::scalar("Tags", 18, Scalar::String, true))
        .add_field(DeclaredField::scalar("Rates", 19, Scalar::F32, true))
        .add_message(
            "Limits",
            vec![
                DeclaredField::scalar("MaxLots", 1, Scalar::F64, false),
                DeclaredField::scalar("Currency", 2, Scalar::String, false),
                DeclaredField::object("Best", 3, "Instrument", false),
            ],
        )
        .add_message(
            "Instrument",
            vec![DeclaredField::scalar("Id", 1, Scalar::String, false)],
        )
        .build()
        .serialize()
}

fn schema() -> SchemaIndex {
    SchemaIndex::build(&build_schema()).unwrap()
}

// ---- wire building helpers -------------------------------------------------

fn tag(dest: &mut Vec<u8>, field_no: u32, wire_type: u8) {
    write_varint(dest, u64::from(field_no) << 3 | u64::from(wire_type));
}

fn write_len_field(dest: &mut Vec<u8>, field_no: u32, payload: &[u8]) {
    tag(dest, field_no, consts::WIRE_TYPE_LEN);
    write_varint(dest, payload.len() as u64);
    dest.extend_from_slice(payload);
}

fn write_string(dest: &mut Vec<u8>, field_no: u32, value: &str) {
    write_len_field(dest, field_no, value.as_bytes());
}

fn write_varint_field(dest: &mut Vec<u8>, field_no: u32, value: u64) {
    tag(dest, field_no, consts::WIRE_TYPE_VARINT);
    write_varint(dest, value);
}

fn write_double(dest: &mut Vec<u8>, field_no: u32, value: f64) {
    tag(dest, field_no, consts::WIRE_TYPE_I64);
    dest.extend_from_slice(&value.to_bits().to_le_bytes());
}

fn write_float(dest: &mut Vec<u8>, field_no: u32, value: f32) {
    tag(dest, field_no, consts::WIRE_TYPE_I32);
    dest.extend_from_slice(&value.to_bits().to_le_bytes());
}

fn iso(unix_microseconds: i64) -> String {
    DateTimeAsMicroseconds::new(unix_microseconds).to_rfc3339()
}

/// One row of that entity, written the way a client's serializer writes one: the
/// declared fields, then the two moments the server appends when it hands the row
/// out.
pub(crate) fn build_row() -> Vec<u8> {
    let mut row = Vec::new();

    write_string(&mut row, 1, "acc-1");
    write_string(&mut row, 2, "eur-usd");
    write_varint_field(&mut row, 5, 1); // Flag = true
    // A negative int32 goes on the wire sign-extended to ten bytes, which is
    // what tells `Int32` from `Uint32` apart on the way back.
    write_varint_field(&mut row, 6, -3i64 as u64);
    write_varint_field(&mut row, 7, -9_000_000_000i64 as u64);
    write_varint_field(&mut row, 8, 7);
    write_varint_field(&mut row, 9, u64::MAX);
    write_float(&mut row, 10, 0.5);
    write_double(&mut row, 11, 1.5);
    write_string(&mut row, 12, "trader");
    write_len_field(&mut row, 13, &[1, 2, 3]); // Blob

    let mut best = Vec::new();
    write_string(&mut best, 1, "EURUSD");

    let mut limits = Vec::new();
    write_double(&mut limits, 1, 2.5);
    write_string(&mut limits, 2, "USD");
    write_len_field(&mut limits, 3, &best);
    write_len_field(&mut row, 14, &limits);

    // The elements of the array carry no `Best`, so the same message is walked
    // once with a reference to follow and twice without one.
    for currency in ["EUR", "GBP"] {
        let mut entry = Vec::new();
        write_double(&mut entry, 1, 0.25);
        write_string(&mut entry, 2, currency);
        write_len_field(&mut row, 15, &entry);
    }

    // Packed, which is the spelling a repeated numeric field arrives in.
    let mut counts = Vec::new();
    for value in [1u64, 2, 3] {
        write_varint(&mut counts, value);
    }
    write_len_field(&mut row, 16, &counts);

    let mut prices = Vec::new();
    for value in [1.25f64, 2.5] {
        prices.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    write_len_field(&mut row, 17, &prices);

    write_string(&mut row, 18, "vip");
    write_string(&mut row, 18, "eu");

    // Packed too, and four bytes wide rather than eight - the width comes from
    // the declared type, since a packed payload carries no tags.
    let mut rates = Vec::new();
    for value in [0.5f32, 0.25] {
        rates.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    write_len_field(&mut row, 19, &rates);

    // A field written by a build which had one more field than this schema.
    write_string(&mut row, 77, "added-later");

    write_varint_field(&mut row, consts::FIELD_TIME_STAMP, TIME_STAMP as u64);
    write_varint_field(&mut row, consts::FIELD_EXPIRES, EXPIRES as u64);

    row
}

/// Written out rather than only asserted piecewise: this is the string the UI
/// receives, and every part of it is somebody's decision.
pub(crate) fn expected_json() -> String {
    format!(
        concat!(
            r#"{{"PartitionKey":"acc-1","RowKey":"eur-usd","Flag":true,"Small":-3,"#,
            r#""Big":-9000000000,"USmall":7,"UBig":18446744073709551615,"#,
            r#""Ratio":0.5,"Amount":1.5,"Name":"trader","Blob":"AQID","#,
            r#""Limits":{{"MaxLots":2.5,"Currency":"USD","Best":{{"Id":"EURUSD"}}}},"#,
            r#""History":[{{"MaxLots":0.25,"Currency":"EUR"}},{{"MaxLots":0.25,"Currency":"GBP"}}],"#,
            r#""Counts":[1,2,3],"Prices":[1.25,2.5],"Tags":["vip","eu"],"Rates":[0.5,0.25],"#,
            r#""77":"added-later","TimeStamp":"{}","Expires":"{}"}}"#
        ),
        iso(TIME_STAMP),
        iso(EXPIRES)
    )
}

#[test]
fn a_row_renders_through_its_schema() {
    let json = write_row_as_json(JsonObjectWriter::new(), &build_row(), Some(&schema())).build();

    assert_eq!(json, expected_json());
}

#[test]
fn without_a_schema_the_row_still_renders_by_field_number() {
    let json = write_row_as_json(JsonObjectWriter::new(), &build_row(), None).build();

    // Strings come out as strings, varints as numbers, and the two fields whose
    // meaning the server owns are still dates.
    assert!(json.contains(r#""1":"acc-1""#));
    assert!(json.contains(r#""2":"eur-usd""#));
    assert!(json.contains(&format!(r#""TimeStamp":"{}""#, iso(TIME_STAMP))));
    assert!(json.contains(&format!(r#""Expires":"{}""#, iso(EXPIRES))));
    // Repeated field, seen twice, becomes an array.
    assert!(json.contains(r#""18":["vip","eu"]"#));
}

/// The two moments are not in the schema and must not need to be: nothing
/// describes them, and they still come out as dates under the names the contract
/// gives them.
#[test]
fn the_two_moments_are_named_by_the_contract_and_not_by_the_schema() {
    let json = write_row_as_json(JsonObjectWriter::new(), &build_row(), Some(&schema())).build();

    assert!(json.contains(&format!(r#""TimeStamp":"{}""#, iso(TIME_STAMP))));
    assert!(json.contains(&format!(r#""Expires":"{}""#, iso(EXPIRES))));

    // ...and by their numbers they are not, which is what says the fallback is
    // what named them.
    assert!(!json.contains(r#""3":"#));
    assert!(!json.contains(r#""4":"#));
}

#[test]
fn a_row_without_expires_does_not_show_the_field() {
    let mut row = Vec::new();
    write_string(&mut row, 1, "pk");
    write_string(&mut row, 2, "rk");
    write_varint_field(&mut row, consts::FIELD_TIME_STAMP, TIME_STAMP as u64);

    let json = write_row_as_json(JsonObjectWriter::new(), &row, Some(&schema())).build();

    assert!(!json.contains("Expires"));
    assert!(json.contains(&format!(r#""TimeStamp":"{}""#, iso(TIME_STAMP))));
}

/// A field the client added after this schema was registered is unknown to it -
/// it still has to be shown rather than dropped.
#[test]
fn a_field_missing_from_the_schema_falls_back_to_its_number() {
    let mut row = Vec::new();
    write_string(&mut row, 1, "pk");
    write_string(&mut row, 2, "rk");
    write_string(&mut row, 77, "added-later");

    let json = write_row_as_json(JsonObjectWriter::new(), &row, Some(&schema())).build();

    assert!(json.contains(r#""77":"added-later""#));
}

#[test]
fn an_unpacked_repeated_scalar_is_read_too() {
    let mut row = Vec::new();
    write_string(&mut row, 1, "pk");
    write_string(&mut row, 2, "rk");
    // The same repeated int32, one field per value instead of packed.
    write_varint_field(&mut row, 16, 7);
    write_varint_field(&mut row, 16, 8);

    let json = write_row_as_json(JsonObjectWriter::new(), &row, Some(&schema())).build();

    assert!(json.contains(r#""Counts":[7,8]"#));
}

#[test]
fn a_truncated_row_is_shown_as_far_as_it_reads() {
    let mut row = build_row();
    row.truncate(20);

    let json = write_row_as_json(JsonObjectWriter::new(), &row, Some(&schema())).build();

    assert!(json.contains(r#""PartitionKey":"acc-1""#));
}

/// The schema arrives as bytes from whoever wrote the row, so the constructor
/// has to answer a blob which is not one - the renderer then shows the row by
/// field number, which is what it already does when no schema is known.
#[test]
fn a_blob_which_is_not_a_schema_is_refused() {
    assert!(SchemaIndex::build(&[1, 2, 3, 4, 5]).is_err());
}

/// Regression: the row writer must build into the writer my-json hands it.
/// Creating a fresh one instead dropped the enclosing array's brackets, and the
/// endpoint answered `{...}]` - an object followed by a stray bracket.
#[test]
fn rows_compose_into_a_valid_json_array() {
    let schema = schema();
    let row = build_row();

    let mut array = my_json::json_writer::JsonArrayWriter::new();

    for _ in 0..2 {
        array = array.write_json_object(|writer| write_row_as_json(writer, &row, Some(&schema)));
    }

    let json = array.build();

    assert!(json.starts_with('['), "json was: {json}");
    assert!(json.ends_with(']'), "json was: {json}");
    assert_eq!(json.matches(r#""PartitionKey":"acc-1""#).count(), 2);

    // And it has to read back as an array of two well-formed objects.
    let iterator = my_json::json_reader::JsonArrayIterator::new(json.as_bytes()).unwrap();

    let mut rows_read = 0;
    while let Some(row) = iterator.get_next() {
        row.unwrap();
        rows_read += 1;
    }

    assert_eq!(rows_read, 2);
}
