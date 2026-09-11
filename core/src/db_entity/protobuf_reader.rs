use super::{ContentRange, DbEntityParseFail, consts};

/// The largest field number protobuf has: the tag packs the number into
/// everything above the three wire-type bits of a 32 bit value.
const MAX_FIELD_NO: u64 = (1 << 29) - 1;

/// One field read off the wire.
pub struct ProtobufField {
    pub field_no: u32,
    pub wire_type: u8,
    /// Where the whole field sits in the source - tag byte included. Copying
    /// this range reproduces the field verbatim.
    pub field: ContentRange,
    /// The value payload. For a length-delimited field this is the content
    /// without its length prefix; otherwise it is the raw value bytes.
    pub value: ContentRange,
    /// The decoded number for a varint or a fixed-width field, `0` otherwise.
    pub value_as_u64: u64,
}

/// Walks a protobuf message without knowing its schema. That is all the server
/// needs: the wire format carries a field number and a wire type in front of
/// every value, so the four reserved fields can be picked out and everything
/// else copied through untouched.
pub struct ProtobufReader<'s> {
    src: &'s [u8],
    pos: usize,
}

impl<'s> ProtobufReader<'s> {
    pub fn new(src: &'s [u8]) -> Self {
        Self { src, pos: 0 }
    }

    pub fn is_eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn broken(reason: impl Into<String>) -> DbEntityParseFail {
        DbEntityParseFail::BrokenProtobufPayload(reason.into())
    }

    fn read_varint(&mut self) -> Result<u64, DbEntityParseFail> {
        let mut result: u64 = 0;

        for shift in 0..10u32 {
            let Some(byte) = self.src.get(self.pos).copied() else {
                return Err(Self::broken("payload ended in the middle of a varint"));
            };

            self.pos += 1;
            result |= u64::from(byte & 0x7F) << (shift * 7);

            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }

        Err(Self::broken("varint is longer than 10 bytes"))
    }

    fn take(&mut self, len: usize) -> Result<ContentRange, DbEntityParseFail> {
        let start = self.pos;
        let Some(end) = start.checked_add(len) else {
            return Err(Self::broken("field length overflows the payload"));
        };

        if end > self.src.len() {
            return Err(Self::broken("field runs past the end of the payload"));
        }

        self.pos = end;
        Ok(ContentRange::new(start, end))
    }

    fn read_fixed(&mut self, len: usize) -> Result<ProtobufFixedValue, DbEntityParseFail> {
        let value = self.take(len)?;

        let mut as_u64: u64 = 0;
        for (shift, byte) in value.get_slice(self.src).iter().enumerate() {
            as_u64 |= u64::from(*byte) << (shift * 8);
        }

        Ok(ProtobufFixedValue { value, as_u64 })
    }

    /// Reads varints back to back until the source runs out - the body of a
    /// packed repeated field, which carries no tags of its own.
    pub fn read_varints_until_the_end(&mut self) -> Result<Vec<u64>, DbEntityParseFail> {
        let mut result = Vec::new();

        while !self.is_eof() {
            result.push(self.read_varint()?);
        }

        Ok(result)
    }

    /// Reads the next field, or `None` at the end of the message.
    pub fn get_next(&mut self) -> Result<Option<ProtobufField>, DbEntityParseFail> {
        if self.is_eof() {
            return Ok(None);
        }

        let field_start = self.pos;

        let tag = self.read_varint()?;
        let wire_type = (tag & 0x07) as u8;
        let field_no = tag >> 3;

        // A tag is a varint, so the wire can carry a number far past what
        // protobuf allows. Truncating it into `u32` would index the row under a
        // field number no conformant library will ever decode out of those same
        // bytes - so the payload is refused instead.
        if field_no == 0 || field_no > MAX_FIELD_NO {
            return Err(Self::broken(format!(
                "field number {field_no} is outside the 1..={MAX_FIELD_NO} protobuf allows"
            )));
        }

        let field_no = field_no as u32;

        let (value, value_as_u64) = match wire_type {
            consts::WIRE_TYPE_VARINT => {
                let start = self.pos;
                let as_u64 = self.read_varint()?;
                (ContentRange::new(start, self.pos), as_u64)
            }
            consts::WIRE_TYPE_I64 => {
                let read = self.read_fixed(8)?;
                (read.value, read.as_u64)
            }
            consts::WIRE_TYPE_I32 => {
                let read = self.read_fixed(4)?;
                (read.value, read.as_u64)
            }
            consts::WIRE_TYPE_LEN => {
                let len = self.read_varint()?;
                (self.take(len as usize)?, 0)
            }
            consts::WIRE_TYPE_START_GROUP | consts::WIRE_TYPE_END_GROUP => {
                // Groups were removed in proto3 and no generator we support emits
                // them. Refusing beats guessing at a nesting depth we can not
                // validate.
                return Err(Self::broken(format!(
                    "field {field_no} uses the deprecated group encoding"
                )));
            }
            other => {
                return Err(Self::broken(format!("unknown wire type {other}")));
            }
        };

        Ok(Some(ProtobufField {
            field_no,
            wire_type,
            field: ContentRange::new(field_start, self.pos),
            value,
            value_as_u64,
        }))
    }
}

struct ProtobufFixedValue {
    value: ContentRange,
    as_u64: u64,
}

/// Reading one value of a field whose number is known.
///
/// A field the entity knows about but which arrived encoded as something else
/// was written under a different schema, and every one of these says so instead
/// of guessing: a silently defaulted value is the worst of the three outcomes.
/// A field number the entity does **not** know is a different matter and is
/// skipped by the caller, which is what keeps a row written by a newer build
/// readable by an older one.
impl ProtobufField {
    pub fn read_string(&self, src: &[u8]) -> Result<String, DbEntityParseFail> {
        String::from_utf8(self.read_bytes(src)?).map_err(|_| {
            DbEntityParseFail::BrokenProtobufPayload(format!(
                "field {} is not valid utf8",
                self.field_no
            ))
        })
    }

    pub fn read_bytes(&self, src: &[u8]) -> Result<Vec<u8>, DbEntityParseFail> {
        self.expect(consts::WIRE_TYPE_LEN)?;
        Ok(self.value.get_slice(src).to_vec())
    }

    /// The payload of a length-delimited field, borrowed rather than copied -
    /// what a nested message is read out of.
    ///
    /// A sub-message is the same wire format one level down, so the caller opens
    /// a reader of its own over this slice. The offsets of that reader are its
    /// own and do not compose with the outer ones, which is why this hands back
    /// a slice instead of a range.
    pub fn read_message_slice<'s>(&self, src: &'s [u8]) -> Result<&'s [u8], DbEntityParseFail> {
        self.expect(consts::WIRE_TYPE_LEN)?;
        Ok(self.value.get_slice(src))
    }

    pub fn read_varint(&self) -> Result<u64, DbEntityParseFail> {
        self.expect(consts::WIRE_TYPE_VARINT)?;
        Ok(self.value_as_u64)
    }

    pub fn read_i64(&self) -> Result<u64, DbEntityParseFail> {
        self.expect(consts::WIRE_TYPE_I64)?;
        Ok(self.value_as_u64)
    }

    pub fn read_i32(&self) -> Result<u32, DbEntityParseFail> {
        self.expect(consts::WIRE_TYPE_I32)?;
        Ok(self.value_as_u64 as u32)
    }

    fn expect(&self, wire_type: u8) -> Result<(), DbEntityParseFail> {
        if self.wire_type == wire_type {
            return Ok(());
        }

        Err(DbEntityParseFail::UnexpectedWireType {
            field_no: self.field_no,
            wire_type: self.wire_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::write_varint;
    use super::*;

    fn varint_field(field_no: u64, value: u64) -> Vec<u8> {
        let mut result = Vec::new();
        write_varint(
            &mut result,
            field_no << 3 | u64::from(consts::WIRE_TYPE_VARINT),
        );
        write_varint(&mut result, value);
        result
    }

    #[test]
    fn the_largest_field_number_protobuf_has_is_read() {
        let src = varint_field(MAX_FIELD_NO, 7);

        let field = ProtobufReader::new(&src).get_next().unwrap().unwrap();

        assert_eq!(u64::from(field.field_no), MAX_FIELD_NO);
        assert_eq!(field.value_as_u64, 7);
    }

    /// The number used to be taken by casting the tag into `u32`, so a tag one
    /// bit too wide came back as a different field entirely - here as field 5,
    /// which is where an entity's own data starts.
    #[test]
    fn a_field_number_past_the_end_of_the_range_is_refused_rather_than_truncated() {
        for field_no in [MAX_FIELD_NO + 1, (1u64 << 32) + 5] {
            let src = varint_field(field_no, 1);

            assert!(matches!(
                ProtobufReader::new(&src).get_next(),
                Err(DbEntityParseFail::BrokenProtobufPayload(_))
            ));
        }
    }

    #[test]
    fn field_number_zero_is_refused() {
        let src = varint_field(0, 1);

        assert!(matches!(
            ProtobufReader::new(&src).get_next(),
            Err(DbEntityParseFail::BrokenProtobufPayload(_))
        ));
    }
}
