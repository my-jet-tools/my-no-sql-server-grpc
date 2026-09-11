use super::consts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbEntityParseFail {
    /// The payload is not a well-formed protobuf message.
    BrokenProtobufPayload(String),
    /// A reserved field arrived with a wire type the contract does not allow -
    /// e.g. `PartitionKey` encoded as a varint instead of a string.
    UnexpectedWireType {
        field_no: u32,
        wire_type: u8,
    },
    /// Proto3 does not put an empty string on the wire, so "absent" and "empty"
    /// are the same thing here - and neither is a usable key.
    PartitionKeyIsRequired,
    RowKeyIsRequired,
    PartitionKeyIsNotUtf8,
    RowKeyIsNotUtf8,
    /// TimeStamp or Expires arrived as a number no calendar has a date for.
    /// Both of them are rendered on every HTTP read, and the renderer is the
    /// place where such a value stops being data and starts being a panic - so
    /// it is refused here, at the edge, rather than stored and read forever.
    MomentIsOutOfRange {
        field_no: u32,
        value: i64,
    },
    /// This `SchemaId` is already known here, and it was known by a different
    /// schema.
    ///
    /// It sits among the entity failures because it is the same envelope and the
    /// same trust boundary: what the client sent with its row is not something
    /// the server can take. The id is a constant the client folds out of its own
    /// type and the server never recomputes, so this is the one check that keeps
    /// two shapes from sharing a number - and sharing one would show one table's
    /// rows through another table's field names, silently and for as long as the
    /// rows live, because the id goes inside them.
    SchemaIdIsAlreadyTakenByAnotherSchema {
        schema_id: u64,
    },
    /// `UseClientTimeStamp` was asked for, but the entity carries no TimeStamp.
    TimeStampIsRequired {
        partition_key: String,
        row_key: String,
    },
}

impl std::fmt::Display for DbEntityParseFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbEntityParseFail::BrokenProtobufPayload(reason) => {
                write!(
                    f,
                    "Entity payload is not a valid protobuf message: {reason}"
                )
            }
            DbEntityParseFail::UnexpectedWireType {
                field_no,
                wire_type,
            } => {
                write!(
                    f,
                    "Field {field_no} of the entity has wire type {wire_type} which the contract does not allow"
                )
            }
            DbEntityParseFail::PartitionKeyIsRequired => write!(
                f,
                "Entity has no PartitionKey (field {}) or it is empty",
                consts::FIELD_PARTITION_KEY
            ),
            DbEntityParseFail::RowKeyIsRequired => write!(
                f,
                "Entity has no RowKey (field {}) or it is empty",
                consts::FIELD_ROW_KEY
            ),
            DbEntityParseFail::PartitionKeyIsNotUtf8 => {
                write!(f, "PartitionKey of the entity is not a valid UTF-8 string")
            }
            DbEntityParseFail::RowKeyIsNotUtf8 => {
                write!(f, "RowKey of the entity is not a valid UTF-8 string")
            }
            DbEntityParseFail::MomentIsOutOfRange { field_no, value } => write!(
                f,
                "Field {field_no} of the entity carries {value} unix microseconds, which is not a moment the server can represent"
            ),
            DbEntityParseFail::SchemaIdIsAlreadyTakenByAnotherSchema { schema_id } => {
                write!(
                    f,
                    "SchemaId {schema_id} is already known here, and the schema sent with it is not the one it is known by"
                )
            }
            DbEntityParseFail::TimeStampIsRequired {
                partition_key,
                row_key,
            } => write!(
                f,
                "Entity ['{partition_key}', '{row_key}'] carries no TimeStamp, which is required for this operation"
            ),
        }
    }
}

impl std::error::Error for DbEntityParseFail {}
