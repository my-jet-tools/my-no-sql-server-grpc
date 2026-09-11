//! The four field numbers every entity of this server reserves, and the wire
//! types they are encoded with.

/// `string PartitionKey = 1;`
pub const FIELD_PARTITION_KEY: u32 = 1;
/// `string RowKey = 2;`
pub const FIELD_ROW_KEY: u32 = 2;
/// `int64 TimeStamp = 3;` - unix microseconds.
pub const FIELD_TIME_STAMP: u32 = 3;
/// `int64 Expires = 4;` - unix microseconds, 0 (or absent) means "never".
pub const FIELD_EXPIRES: u32 = 4;

/// The first field number an entity may use for its own data.
pub const FIRST_USER_FIELD: u32 = 5;

pub const WIRE_TYPE_VARINT: u8 = 0;
pub const WIRE_TYPE_I64: u8 = 1;
pub const WIRE_TYPE_LEN: u8 = 2;
pub const WIRE_TYPE_START_GROUP: u8 = 3;
pub const WIRE_TYPE_END_GROUP: u8 = 4;
pub const WIRE_TYPE_I32: u8 = 5;

/// Pre-computed tag byte of `TimeStamp` (`field 3`, wire type varint).
pub const TAG_TIME_STAMP: u8 = ((FIELD_TIME_STAMP as u8) << 3) | WIRE_TYPE_VARINT;
/// Pre-computed tag byte of `Expires` (`field 4`, wire type varint).
pub const TAG_EXPIRES: u8 = ((FIELD_EXPIRES as u8) << 3) | WIRE_TYPE_VARINT;
