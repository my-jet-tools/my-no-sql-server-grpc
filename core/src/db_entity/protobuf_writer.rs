/// Appends a protobuf varint.
///
/// A negative `int64` is sign-extended to 64 bits before encoding, which is what
/// casting through `u64` does - so `write_varint(dest, value as u64)` is the
/// correct encoding of an `int64` field for any value.
pub fn write_varint(dest: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;

        if value != 0 {
            byte |= 0x80;
        }

        dest.push(byte);

        if value == 0 {
            return;
        }
    }
}

/// How many bytes [`write_varint`] would append.
pub fn varint_len(mut value: u64) -> usize {
    let mut result = 1;

    while value >= 0x80 {
        value >>= 7;
        result += 1;
    }

    result
}

/// The tag in front of every field: its number and how the value after it is
/// encoded.
pub fn write_tag(dest: &mut Vec<u8>, field_no: u32, wire_type: u8) {
    write_varint(dest, (u64::from(field_no) << 3) | u64::from(wire_type));
}

/// A varint field - `bool`, every `int`/`uint` and every enum. A negative
/// integer is sign-extended through `u64`, which is what protobuf does for
/// `int32`/`int64`.
pub fn write_varint_field(dest: &mut Vec<u8>, field_no: u32, value: u64) {
    write_tag(dest, field_no, super::consts::WIRE_TYPE_VARINT);
    write_varint(dest, value);
}

/// A length-delimited field - `string`, `bytes` and a nested message.
pub fn write_len_field(dest: &mut Vec<u8>, field_no: u32, value: &[u8]) {
    write_tag(dest, field_no, super::consts::WIRE_TYPE_LEN);
    write_varint(dest, value.len() as u64);
    dest.extend_from_slice(value);
}

/// A fixed 64 bit field - `double`, `fixed64`, `sfixed64`.
pub fn write_i64_field(dest: &mut Vec<u8>, field_no: u32, value: u64) {
    write_tag(dest, field_no, super::consts::WIRE_TYPE_I64);
    dest.extend_from_slice(&value.to_le_bytes());
}

/// A fixed 32 bit field - `float`, `fixed32`, `sfixed32`.
pub fn write_i32_field(dest: &mut Vec<u8>, field_no: u32, value: u32) {
    write_tag(dest, field_no, super::consts::WIRE_TYPE_I32);
    dest.extend_from_slice(&value.to_le_bytes());
}

/// Every value of a repeated numeric field which arrived **packed** - protoc
/// emits repeated numerics that way by default, so anything reading a field
/// somebody else wrote has to understand it.
pub fn read_packed_varints(src: &[u8]) -> Result<Vec<u64>, super::DbEntityParseFail> {
    let mut reader = super::ProtobufReader::new(src);
    reader.read_varints_until_the_end()
}

/// The same for a packed field of fixed 64 bit values.
pub fn read_packed_i64(src: &[u8]) -> Result<Vec<u64>, super::DbEntityParseFail> {
    read_packed_fixed(src, 8)
}

/// The same for a packed field of fixed 32 bit values.
pub fn read_packed_i32(src: &[u8]) -> Result<Vec<u64>, super::DbEntityParseFail> {
    read_packed_fixed(src, 4)
}

fn read_packed_fixed(src: &[u8], width: usize) -> Result<Vec<u64>, super::DbEntityParseFail> {
    if !src.len().is_multiple_of(width) {
        return Err(super::DbEntityParseFail::BrokenProtobufPayload(format!(
            "a packed field of {width}-byte values is {} bytes long",
            src.len()
        )));
    }

    Ok(src
        .chunks_exact(width)
        .map(|chunk| {
            let mut result: u64 = 0;

            for (shift, byte) in chunk.iter().enumerate() {
                result |= u64::from(*byte) << (shift * 8);
            }

            result
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::ProtobufReader;
    use super::*;

    fn round_trip(value: u64) {
        let mut buf = Vec::new();
        // Field 3, wire type varint - the shape the server emits TimeStamp with.
        buf.push(crate::db_entity::consts::TAG_TIME_STAMP);
        write_varint(&mut buf, value);

        assert_eq!(buf.len(), 1 + varint_len(value));

        let mut reader = ProtobufReader::new(&buf);
        let field = reader.get_next().unwrap().unwrap();

        assert_eq!(field.field_no, 3);
        assert_eq!(field.value_as_u64, value);
        assert!(reader.is_eof());
    }

    #[test]
    fn varints_round_trip() {
        for value in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            round_trip(value);
        }
    }

    #[test]
    fn negative_i64_round_trips_through_u64() {
        let value: i64 = -1;
        round_trip(value as u64);
        assert_eq!(varint_len(value as u64), 10);
    }
}
