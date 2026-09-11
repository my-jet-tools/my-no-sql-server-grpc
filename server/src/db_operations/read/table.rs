use std::sync::Arc;

use my_no_sql_grpc_core::db::DbTable;

use crate::app::DbNamespace;
use crate::db_operations::DbOperationError;

pub fn get_table(
    db_namespace: &DbNamespace,
    table_name: &str,
) -> Result<Arc<DbTable>, DbOperationError> {
    match db_namespace.tables.get_table(table_name) {
        Some(db_table) => Ok(db_table),
        None => Err(DbOperationError::TableNotFound(table_name.to_string())),
    }
}

pub fn get_tables(db_namespace: &DbNamespace) -> Arc<Vec<Arc<DbTable>>> {
    db_namespace.tables.get_tables()
}
