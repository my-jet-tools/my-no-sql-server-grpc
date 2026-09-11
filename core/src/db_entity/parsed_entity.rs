use super::{ContentRange, DbEntityParseFail, ProtobufReader, consts};

/// An entity taken apart into the four reserved fields and everything else.
///
/// `raw` is the client's payload **with fields 3 and 4 removed**. TimeStamp and
/// Expires are not kept inside the stored bytes at all: they are values the
/// server owns and re-generates when the row is handed out. That keeps `raw`
/// immutable, so `update_expires` is a plain atomic store instead of a rewrite
/// of the payload, and the key ranges stay valid for the whole life of the row.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedEntity {
    pub raw: Vec<u8>,
    pub partition_key: ContentRange,
    pub row_key: ContentRange,
    /// `None` - the entity carried no TimeStamp.
    pub time_stamp: Option<i64>,
    /// Unix microseconds; `0` means the row never expires.
    pub expires: i64,
}

/// The earliest and the latest moment the server can turn back into a date.
///
/// The calendar behind `DateTimeAsMicroseconds::to_rfc3339` runs from year
/// -262143 to year 262142, and a value outside it does not render - it panics.
/// The numbers are written out rather than derived because there is nothing to
/// derive them from: they are a property of the date library, and the test below
/// is what keeps them equal to it.
pub const MIN_MOMENT_MICROSECONDS: i64 = -8_334_601_228_800_000_000;
pub const MAX_MOMENT_MICROSECONDS: i64 = 8_210_266_876_799_999_999;

/// Whether a unix-microsecond moment is one that can be shown.
///
/// It lives here because both places a moment enters the server from a client
/// have to ask the same question: the entity's own TimeStamp and Expires, and
/// the expiration a reader posts with its read statistics. A moment which got
/// past either of them poisons every later read of the row it landed on, and
/// nothing on the read side can undo that.
pub fn moment_is_in_range(unix_microseconds: i64) -> bool {
    (MIN_MOMENT_MICROSECONDS..=MAX_MOMENT_MICROSECONDS).contains(&unix_microseconds)
}

impl ParsedEntity {
    pub fn parse(src: &[u8]) -> Result<Self, DbEntityParseFail> {
        let mut raw = Vec::with_capacity(src.len());

        let mut partition_key = None;
        let mut row_key = None;
        let mut time_stamp = None;
        let mut expires = 0;

        let mut reader = ProtobufReader::new(src);

        while let Some(field) = reader.get_next()? {
            match field.field_no {
                consts::FIELD_TIME_STAMP => {
                    expect_varint(&field)?;
                    time_stamp = Some(expect_moment(field.value_as_u64 as i64, field.field_no)?);
                    continue;
                }
                consts::FIELD_EXPIRES => {
                    expect_varint(&field)?;
                    expires = expect_moment(field.value_as_u64 as i64, field.field_no)?;
                    continue;
                }
                _ => {}
            }

            // Everything else is copied through byte for byte - the server has no
            // opinion about the entity's own fields.
            let dest_offset = raw.len();
            raw.extend_from_slice(field.field.get_slice(src));

            if field.field_no != consts::FIELD_PARTITION_KEY
                && field.field_no != consts::FIELD_ROW_KEY
            {
                continue;
            }

            if field.wire_type != consts::WIRE_TYPE_LEN {
                return Err(DbEntityParseFail::UnexpectedWireType {
                    field_no: field.field_no,
                    wire_type: field.wire_type,
                });
            }

            // The field was appended as a whole, so the value keeps its offset
            // relative to the start of the field.
            let value_offset = field.value.start - field.field.start;
            let range = ContentRange::new(
                dest_offset + value_offset,
                dest_offset + value_offset + field.value.len(),
            );

            // Protobuf semantics for a repeated occurrence of a scalar field are
            // "the last one wins", and a re-parse of `raw` would resolve it that
            // way too - so the range has to follow the last one as well.
            if field.field_no == consts::FIELD_PARTITION_KEY {
                partition_key = Some(range);
            } else {
                row_key = Some(range);
            }
        }

        // Proto3 leaves an empty string off the wire entirely, so "no field" and
        // "empty field" are indistinguishable here - and neither is a key.
        let partition_key = match partition_key {
            Some(range) if !range.is_empty() => range,
            _ => return Err(DbEntityParseFail::PartitionKeyIsRequired),
        };

        let row_key = match row_key {
            Some(range) if !range.is_empty() => range,
            _ => return Err(DbEntityParseFail::RowKeyIsRequired),
        };

        if std::str::from_utf8(partition_key.get_slice(&raw)).is_err() {
            return Err(DbEntityParseFail::PartitionKeyIsNotUtf8);
        }

        if std::str::from_utf8(row_key.get_slice(&raw)).is_err() {
            return Err(DbEntityParseFail::RowKeyIsNotUtf8);
        }

        Ok(Self {
            raw,
            partition_key,
            row_key,
            time_stamp,
            expires,
        })
    }

    pub fn get_partition_key(&self) -> &str {
        self.partition_key.get_str(&self.raw)
    }

    pub fn get_row_key(&self) -> &str {
        self.row_key.get_str(&self.raw)
    }
}

/// The two moments are checked where the payload stops being the client's and
/// becomes the server's. Later is too late: they are written into the row, the
/// row is persisted, and every read of it from then on - HTTP JSON and
/// `/api/Row/Statistics` alike - would go through a date that can not be built.
fn expect_moment(value: i64, field_no: u32) -> Result<i64, DbEntityParseFail> {
    if moment_is_in_range(value) {
        return Ok(value);
    }

    Err(DbEntityParseFail::MomentIsOutOfRange { field_no, value })
}

fn expect_varint(field: &super::ProtobufField) -> Result<(), DbEntityParseFail> {
    if field.wire_type == consts::WIRE_TYPE_VARINT {
        return Ok(());
    }

    Err(DbEntityParseFail::UnexpectedWireType {
        field_no: field.field_no,
        wire_type: field.wire_type,
    })
}

#[cfg(test)]
mod tests {
    use super::super::write_varint;
    use super::*;
    use rust_extensions::date_time::DateTimeAsMicroseconds;

    fn push_str_field(dest: &mut Vec<u8>, field_no: u32, value: &str) {
        write_varint(
            dest,
            u64::from(field_no) << 3 | u64::from(consts::WIRE_TYPE_LEN),
        );
        write_varint(dest, value.len() as u64);
        dest.extend_from_slice(value.as_bytes());
    }

    fn push_varint_field(dest: &mut Vec<u8>, field_no: u32, value: i64) {
        write_varint(
            dest,
            u64::from(field_no) << 3 | u64::from(consts::WIRE_TYPE_VARINT),
        );
        write_varint(dest, value as u64);
    }

    #[test]
    fn reserved_fields_are_extracted_and_stripped() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "my-partition");
        push_str_field(&mut src, 2, "my-row");
        push_varint_field(&mut src, 3, 1_700_000_000_000_000);
        push_varint_field(&mut src, 4, 1_800_000_000_000_000);
        push_str_field(&mut src, 5, "payload");

        let parsed = ParsedEntity::parse(&src).unwrap();

        assert_eq!(parsed.get_partition_key(), "my-partition");
        assert_eq!(parsed.get_row_key(), "my-row");
        assert_eq!(parsed.time_stamp, Some(1_700_000_000_000_000));
        assert_eq!(parsed.expires, 1_800_000_000_000_000);

        // raw keeps fields 1, 2 and 5 - and nothing else.
        let mut expected = Vec::new();
        push_str_field(&mut expected, 1, "my-partition");
        push_str_field(&mut expected, 2, "my-row");
        push_str_field(&mut expected, 5, "payload");
        assert_eq!(parsed.raw, expected);
    }

    #[test]
    fn entity_without_time_stamp_and_expires() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "pk");
        push_str_field(&mut src, 2, "rk");

        let parsed = ParsedEntity::parse(&src).unwrap();

        assert_eq!(parsed.time_stamp, None);
        assert_eq!(parsed.expires, 0);
        assert_eq!(parsed.raw, src);
    }

    #[test]
    fn user_fields_of_every_wire_type_pass_through() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "pk");
        push_str_field(&mut src, 2, "rk");
        push_varint_field(&mut src, 5, -7);
        // field 6, wire type i64
        write_varint(&mut src, 6 << 3 | u64::from(consts::WIRE_TYPE_I64));
        src.extend_from_slice(&1234u64.to_le_bytes());
        // field 7, wire type i32
        write_varint(&mut src, 7 << 3 | u64::from(consts::WIRE_TYPE_I32));
        src.extend_from_slice(&99u32.to_le_bytes());

        let parsed = ParsedEntity::parse(&src).unwrap();

        assert_eq!(parsed.raw, src);
        assert_eq!(parsed.get_partition_key(), "pk");
    }

    #[test]
    fn missing_partition_key_is_rejected() {
        let mut src = Vec::new();
        push_str_field(&mut src, 2, "rk");

        assert_eq!(
            ParsedEntity::parse(&src),
            Err(DbEntityParseFail::PartitionKeyIsRequired)
        );
    }

    #[test]
    fn empty_partition_key_is_rejected() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "");
        push_str_field(&mut src, 2, "rk");

        assert_eq!(
            ParsedEntity::parse(&src),
            Err(DbEntityParseFail::PartitionKeyIsRequired)
        );
    }

    #[test]
    fn missing_row_key_is_rejected() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "pk");

        assert_eq!(
            ParsedEntity::parse(&src),
            Err(DbEntityParseFail::RowKeyIsRequired)
        );
    }

    #[test]
    fn repeated_key_field_takes_the_last_occurrence() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "first");
        push_str_field(&mut src, 2, "rk");
        push_str_field(&mut src, 1, "second");

        let parsed = ParsedEntity::parse(&src).unwrap();

        assert_eq!(parsed.get_partition_key(), "second");
    }

    #[test]
    fn partition_key_with_wrong_wire_type_is_rejected() {
        let mut src = Vec::new();
        push_varint_field(&mut src, 1, 42);
        push_str_field(&mut src, 2, "rk");

        assert_eq!(
            ParsedEntity::parse(&src),
            Err(DbEntityParseFail::UnexpectedWireType {
                field_no: 1,
                wire_type: consts::WIRE_TYPE_VARINT,
            })
        );
    }

    #[test]
    fn time_stamp_with_wrong_wire_type_is_rejected() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "pk");
        push_str_field(&mut src, 2, "rk");
        push_str_field(&mut src, 3, "not-a-number");

        assert_eq!(
            ParsedEntity::parse(&src),
            Err(DbEntityParseFail::UnexpectedWireType {
                field_no: 3,
                wire_type: consts::WIRE_TYPE_LEN,
            })
        );
    }

    #[test]
    fn truncated_payload_is_rejected() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "pk");
        push_str_field(&mut src, 2, "row-key-value");
        src.truncate(src.len() - 3);

        assert!(matches!(
            ParsedEntity::parse(&src),
            Err(DbEntityParseFail::BrokenProtobufPayload(_))
        ));
    }

    #[test]
    fn group_encoding_is_rejected() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "pk");
        push_str_field(&mut src, 2, "rk");
        write_varint(&mut src, 8 << 3 | u64::from(consts::WIRE_TYPE_START_GROUP));

        assert!(matches!(
            ParsedEntity::parse(&src),
            Err(DbEntityParseFail::BrokenProtobufPayload(_))
        ));
    }

    /// `0` already means "never", so an application asking for "never" with
    /// `i64::MAX` is a natural mistake - and it used to be stored, after which
    /// every read of that row panicked inside the date renderer, forever and
    /// across restarts.
    #[test]
    fn a_moment_no_calendar_has_a_date_for_is_rejected() {
        for field_no in [consts::FIELD_TIME_STAMP, consts::FIELD_EXPIRES] {
            for value in [i64::MAX, i64::MIN, MAX_MOMENT_MICROSECONDS + 1] {
                let mut src = Vec::new();
                push_str_field(&mut src, 1, "pk");
                push_str_field(&mut src, 2, "rk");
                push_varint_field(&mut src, field_no, value);

                assert_eq!(
                    ParsedEntity::parse(&src),
                    Err(DbEntityParseFail::MomentIsOutOfRange { field_no, value })
                );
            }
        }
    }

    /// The edge itself is data, not an error: the check refuses what can not be
    /// rendered and nothing more.
    #[test]
    fn the_widest_renderable_moment_is_accepted() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "pk");
        push_str_field(&mut src, 2, "rk");
        push_varint_field(&mut src, consts::FIELD_EXPIRES, MAX_MOMENT_MICROSECONDS);

        let parsed = ParsedEntity::parse(&src).unwrap();

        assert_eq!(parsed.expires, MAX_MOMENT_MICROSECONDS);
        assert_eq!(
            DateTimeAsMicroseconds::new(parsed.expires).to_rfc3339(),
            DateTimeAsMicroseconds::new(MAX_MOMENT_MICROSECONDS).to_rfc3339()
        );
    }

    /// Both ends of the range are pinned against the renderer itself: the
    /// constants are only worth anything if they are exactly where it stops.
    #[test]
    fn the_range_is_pinned_to_what_the_renderer_accepts() {
        for value in [MIN_MOMENT_MICROSECONDS, MAX_MOMENT_MICROSECONDS] {
            assert!(moment_is_in_range(value));
            // Panics if the constant is off by one microsecond.
            DateTimeAsMicroseconds::new(value).to_rfc3339();
        }

        assert!(!moment_is_in_range(MIN_MOMENT_MICROSECONDS - 1));
        assert!(!moment_is_in_range(MAX_MOMENT_MICROSECONDS + 1));

        for value in [MIN_MOMENT_MICROSECONDS - 1, MAX_MOMENT_MICROSECONDS + 1] {
            let rendered = std::panic::catch_unwind(|| {
                DateTimeAsMicroseconds::new(value).to_rfc3339();
            });

            assert!(rendered.is_err(), "{value} rendered, so the range is wider");
        }
    }

    #[test]
    fn negative_time_stamp_round_trips() {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "pk");
        push_str_field(&mut src, 2, "rk");
        push_varint_field(&mut src, 3, -1);

        let parsed = ParsedEntity::parse(&src).unwrap();

        assert_eq!(parsed.time_stamp, Some(-1));
    }
}
