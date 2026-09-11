use rust_extensions::date_time::{AtomicDateTimeAsMicroseconds, DateTimeAsMicroseconds};
use rust_extensions::sorted_vec::EntityWithStrKey;

use crate::db_entity::{ContentRange, ParsedEntity, consts, varint_len, write_varint};

/// A row of a table.
///
/// `raw` is the client's payload with `TimeStamp` and `Expires` taken out, and it
/// never changes after the row is built - which is what makes the keys readable
/// with no copying and `update_expires` a plain atomic store. Both values are
/// written back when the row is handed out, so what a reader gets is the entity
/// exactly as it was sent, with the server's own view of those two fields.
pub struct DbRow {
    raw: Vec<u8>,
    partition_key: ContentRange,
    row_key: ContentRange,
    schema_id: u64,
    time_stamp: DateTimeAsMicroseconds,
    /// `0` means the row never expires.
    expires: AtomicDateTimeAsMicroseconds,
    last_read_access: AtomicDateTimeAsMicroseconds,
}

impl DbRow {
    pub fn new(parsed: ParsedEntity, schema_id: u64, time_stamp: DateTimeAsMicroseconds) -> Self {
        Self {
            raw: parsed.raw,
            partition_key: parsed.partition_key,
            row_key: parsed.row_key,
            schema_id,
            time_stamp,
            expires: AtomicDateTimeAsMicroseconds::new(parsed.expires),
            last_read_access: AtomicDateTimeAsMicroseconds::new(time_stamp.unix_microseconds),
        }
    }

    pub fn get_partition_key(&self) -> &str {
        self.partition_key.get_str(&self.raw)
    }

    pub fn get_row_key(&self) -> &str {
        self.row_key.get_str(&self.raw)
    }

    pub fn get_schema_id(&self) -> u64 {
        self.schema_id
    }

    pub fn get_time_stamp(&self) -> DateTimeAsMicroseconds {
        self.time_stamp
    }

    pub fn get_expires(&self) -> Option<DateTimeAsMicroseconds> {
        let result = self.expires.as_date_time();

        if result.unix_microseconds == 0 {
            return None;
        }

        Some(result)
    }

    /// Returns the value which was there before.
    pub fn update_expires(
        &self,
        expires: Option<DateTimeAsMicroseconds>,
    ) -> Option<DateTimeAsMicroseconds> {
        let before = self.get_expires();

        match expires {
            Some(expires) => self.expires.update(expires),
            None => self.expires.update(DateTimeAsMicroseconds::new(0)),
        }

        before
    }

    pub fn get_last_read_access(&self) -> DateTimeAsMicroseconds {
        self.last_read_access.as_date_time()
    }

    pub fn update_last_read_access(&self, value: DateTimeAsMicroseconds) {
        self.last_read_access.update(value);
    }

    /// Appends the row in its emit form: the stored payload followed by
    /// `TimeStamp` and - when the row has one - `Expires`. Protobuf puts no
    /// requirement on field order, so appending them is as valid as having them
    /// in the middle, and it costs one write instead of a rebuild.
    pub fn write_to(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&self.raw);

        dest.push(consts::TAG_TIME_STAMP);
        write_varint(dest, self.time_stamp.unix_microseconds as u64);

        if let Some(expires) = self.get_expires() {
            dest.push(consts::TAG_EXPIRES);
            write_varint(dest, expires.unix_microseconds as u64);
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(self.get_content_size());
        self.write_to(&mut result);
        result
    }

    /// Size of the emit form, without building it. What a chunk of the wire is
    /// measured in.
    pub fn get_content_size(&self) -> usize {
        let mut result = self.get_stored_size();

        if let Some(expires) = self.get_expires() {
            result += 1 + varint_len(expires.unix_microseconds as u64);
        }

        result
    }

    /// The same, minus `Expires`.
    ///
    /// This is what a partition accounts its rows by, and it has to be the part
    /// which **can not change**: `Expires` is an atomic that a reader pushes
    /// forward on every call, and a size that moves after a row was added is a
    /// size the partition would subtract a different number for when the row
    /// leaves.
    pub fn get_stored_size(&self) -> usize {
        self.raw.len() + 1 + varint_len(self.time_stamp.unix_microseconds as u64)
    }
}

impl EntityWithStrKey for DbRow {
    fn get_key(&self) -> &str {
        self.get_row_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db_entity::ProtobufReader;

    fn push_str_field(dest: &mut Vec<u8>, field_no: u32, value: &str) {
        write_varint(
            dest,
            u64::from(field_no) << 3 | u64::from(consts::WIRE_TYPE_LEN),
        );
        write_varint(dest, value.len() as u64);
        dest.extend_from_slice(value.as_bytes());
    }

    fn build_row(expires: i64) -> DbRow {
        let mut src = Vec::new();
        push_str_field(&mut src, 1, "pk");
        push_str_field(&mut src, 2, "rk");
        push_str_field(&mut src, 5, "hello");

        let mut parsed = ParsedEntity::parse(&src).unwrap();
        parsed.expires = expires;

        DbRow::new(
            parsed,
            42,
            DateTimeAsMicroseconds::new(1_700_000_000_000_000),
        )
    }

    fn read_reserved_fields(bytes: &[u8]) -> (Option<i64>, Option<i64>) {
        let mut reader = ProtobufReader::new(bytes);
        let mut time_stamp = None;
        let mut expires = None;

        while let Some(field) = reader.get_next().unwrap() {
            match field.field_no {
                consts::FIELD_TIME_STAMP => time_stamp = Some(field.value_as_u64 as i64),
                consts::FIELD_EXPIRES => expires = Some(field.value_as_u64 as i64),
                _ => {}
            }
        }

        (time_stamp, expires)
    }

    #[test]
    fn emitted_row_carries_time_stamp_and_no_expires() {
        let row = build_row(0);
        let bytes = row.to_vec();

        assert_eq!(bytes.len(), row.get_content_size());
        assert_eq!(
            read_reserved_fields(&bytes),
            (Some(1_700_000_000_000_000), None)
        );
    }

    #[test]
    fn emitted_row_carries_expires_when_set() {
        let row = build_row(1_800_000_000_000_000);
        let bytes = row.to_vec();

        assert_eq!(bytes.len(), row.get_content_size());
        assert_eq!(
            read_reserved_fields(&bytes),
            (Some(1_700_000_000_000_000), Some(1_800_000_000_000_000))
        );
    }

    #[test]
    fn emitted_row_re_parses_into_the_same_row() {
        let row = build_row(1_800_000_000_000_000);
        let parsed = ParsedEntity::parse(&row.to_vec()).unwrap();

        assert_eq!(parsed.get_partition_key(), "pk");
        assert_eq!(parsed.get_row_key(), "rk");
        assert_eq!(parsed.time_stamp, Some(1_700_000_000_000_000));
        assert_eq!(parsed.expires, 1_800_000_000_000_000);
    }

    #[test]
    fn update_expires_changes_what_is_emitted() {
        let row = build_row(0);

        let before = row.update_expires(Some(DateTimeAsMicroseconds::new(555)));
        assert!(before.is_none());
        assert_eq!(row.get_expires().unwrap().unix_microseconds, 555);
        assert_eq!(read_reserved_fields(&row.to_vec()).1, Some(555));

        let before = row.update_expires(None);
        assert_eq!(before.unwrap().unix_microseconds, 555);
        assert!(row.get_expires().is_none());
        assert_eq!(read_reserved_fields(&row.to_vec()).1, None);
    }
}
