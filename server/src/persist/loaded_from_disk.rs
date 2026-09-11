use my_no_sql_grpc_core::db::DbTableAttributes;

/// One partition as the recovery scan found it - the payload is still the raw
/// slot body, decoded by [`crate::persist::partition_blob`].
pub struct LoadedPartition {
    pub table_name: String,
    pub partition_key: String,
    pub payload: Vec<u8>,
}

pub struct LoadedTableAttrs {
    pub table_name: String,
    pub attr: DbTableAttributes,
}
