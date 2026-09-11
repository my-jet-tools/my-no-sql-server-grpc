use my_no_sql_grpc_core::db::{
    BulkWriteMode, DbTable, DbTableAttributes, PartitionRowKeys, TransactionAction,
};
use my_no_sql_grpc_core::schemas::EntitySchema;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::DbNamespace;
use crate::data_sync_period::DataSyncPeriod;
use crate::db_operations::DbOperationError;
use crate::my_no_sql_writer_grpc::*;

impl From<DbOperationError> for tonic::Status {
    fn from(src: DbOperationError) -> Self {
        let message = src.to_string();

        match src {
            DbOperationError::NotInitialized => tonic::Status::unavailable(message),
            DbOperationError::NamespaceNotFound(_) => tonic::Status::not_found(message),
            DbOperationError::DefaultNamespaceCanNotBeDeleted => {
                tonic::Status::invalid_argument(message)
            }
            DbOperationError::InvalidNamespaceName(_) => tonic::Status::invalid_argument(message),
            // The namespace is gone from the server and the folder is not: the
            // caller asked for something the server could not finish, which is
            // not something the caller can fix by asking differently.
            DbOperationError::NamespaceFolderNotDeleted(_) => tonic::Status::internal(message),
            DbOperationError::TableNotFound(_) => tonic::Status::not_found(message),
            DbOperationError::PartitionNotFound(_) => tonic::Status::not_found(message),
            DbOperationError::TableAlreadyExists(_) => tonic::Status::already_exists(message),
            DbOperationError::RowNotFound => tonic::Status::not_found(message),
            DbOperationError::RowAlreadyExists => tonic::Status::already_exists(message),
            // `Aborted` is what gRPC reserves for exactly this: a call which
            // failed on a concurrency check and which the caller is expected to
            // repeat from a fresh read.
            DbOperationError::OptimisticConcurrencyUpdateFails => tonic::Status::aborted(message),
            DbOperationError::EntityParseFail(_) => tonic::Status::invalid_argument(message),
            DbOperationError::BackupFailed(_) => tonic::Status::failed_precondition(message),
            DbOperationError::MigrationFailed(_) => tonic::Status::failed_precondition(message),
        }
    }
}

/// An enum value the server does not know maps to the default rather than to an
/// error: a newer client asking for a period this build never heard of still has
/// its write accepted, it is only flushed on the default schedule.
pub fn to_data_sync_period(src: i32) -> DataSyncPeriod {
    let Ok(value) = SyncPeriodGrpcModel::try_from(src) else {
        return DataSyncPeriod::default();
    };

    match value {
        SyncPeriodGrpcModel::SyncPeriodDefault => DataSyncPeriod::default(),
        SyncPeriodGrpcModel::SyncPeriodImmediately => DataSyncPeriod::Immediately,
        SyncPeriodGrpcModel::SyncPeriodSec1 => DataSyncPeriod::Sec1,
        SyncPeriodGrpcModel::SyncPeriodSec5 => DataSyncPeriod::Sec5,
        SyncPeriodGrpcModel::SyncPeriodSec15 => DataSyncPeriod::Sec15,
        SyncPeriodGrpcModel::SyncPeriodSec30 => DataSyncPeriod::Sec30,
        SyncPeriodGrpcModel::SyncPeriodMin1 => DataSyncPeriod::Min1,
        SyncPeriodGrpcModel::SyncPeriodAsap => DataSyncPeriod::Asap,
    }
}

/// A mode the server does not know is refused, and that is the opposite of what
/// an unknown sync period does on purpose: the modes differ in what they
/// destroy, so falling back to a default would be guessing whether the caller
/// asked to empty the table.
pub fn to_bulk_write_mode(src: i32) -> Result<BulkWriteMode, tonic::Status> {
    let Ok(value) = BulkWriteModeGrpcModel::try_from(src) else {
        return Err(tonic::Status::invalid_argument(format!(
            "Unknown bulk write mode {src}"
        )));
    };

    Ok(match value {
        BulkWriteModeGrpcModel::BulkWriteInsertOrReplace => BulkWriteMode::InsertOrReplace,
        BulkWriteModeGrpcModel::BulkWriteInsertOrReplaceIfNew => {
            BulkWriteMode::InsertOrReplaceIfNew
        }
        BulkWriteModeGrpcModel::BulkWriteCleanPartitionsAndInsert => {
            BulkWriteMode::CleanPartitionsAndInsert
        }
        BulkWriteModeGrpcModel::BulkWriteCleanTableAndInsert => BulkWriteMode::CleanTableAndInsert,
    })
}

pub fn to_partition_row_keys(src: PartitionRowKeysGrpcModel) -> PartitionRowKeys {
    PartitionRowKeys {
        partition_key: src.partition_key,
        row_keys: src.row_keys,
    }
}

/// Turns one posted action into the database's own. Exactly one field of the
/// envelope may be set: no field is a message which asks for nothing, and two
/// fields is a message whose order against itself is undefined - and order is
/// the one thing a transaction may not guess at.
///
/// The rows are built here rather than at commit, so an entity the server can
/// not read is refused while the client is still posting.
pub fn to_transaction_action(
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    src: TransactionActionGrpcModel,
    now: DateTimeAsMicroseconds,
) -> Result<TransactionAction, tonic::Status> {
    let set = usize::from(src.clean_table.is_some())
        + usize::from(src.delete_partitions.is_some())
        + usize::from(src.delete_rows.is_some())
        + usize::from(src.insert_or_replace.is_some());

    Ok(
        match (
            src.clean_table,
            src.delete_partitions,
            src.delete_rows,
            src.insert_or_replace,
        ) {
            (Some(_), None, None, None) => TransactionAction::CleanTable,

            (None, Some(action), None, None) => {
                TransactionAction::DeletePartitions(action.partition_keys)
            }

            (None, None, Some(action), None) => TransactionAction::DeleteRows(PartitionRowKeys {
                partition_key: action.partition_key,
                row_keys: action.row_keys,
            }),

            (None, None, None, Some(action)) => {
                let Some(schema) = action.schema else {
                    return Err(tonic::Status::invalid_argument(
                        "The entity schema is required on every insert of a transaction",
                    ));
                };

                TransactionAction::InsertOrReplace(crate::db_operations::write::build_db_rows(
                    db_namespace,
                    db_table,
                    schema.into(),
                    &action.rows,
                    action.use_client_time_stamp,
                    now,
                )?)
            }

            _ => {
                return Err(tonic::Status::invalid_argument(format!(
                    "A transaction action carries exactly one instruction, this one carries {set}"
                )));
            }
        },
    )
}

pub fn to_db_table_attributes(src: Option<TableAttributesGrpcModel>) -> DbTableAttributes {
    let Some(src) = src else {
        return DbTableAttributes::create_default();
    };

    DbTableAttributes {
        persist: src.persist,
        // 0 is what a client which does not want a limit sends when it can not
        // leave the field out - treat it as "no limit", not as "no partitions".
        max_partitions_amount: to_optional_limit(src.max_partitions_amount),
        max_rows_per_partition_amount: to_optional_limit(src.max_rows_per_partition_amount),
        created: DateTimeAsMicroseconds::now(),
        // Nothing a caller sends names a schema: they are what the table's rows
        // were written under, and `DbTable::set_attributes` merges rather than
        // takes them for exactly this reason.
        schemas: Default::default(),
    }
}

fn to_optional_limit(src: Option<i32>) -> Option<usize> {
    let value = src?;

    if value <= 0 {
        return None;
    }

    Some(value as usize)
}

pub fn to_table_grpc_model(db_table: &DbTable) -> TableGrpcModel {
    let attributes = db_table.get_attributes();

    TableGrpcModel {
        name: db_table.name.clone(),
        attributes: Some(TableAttributesGrpcModel {
            persist: attributes.persist,
            max_partitions_amount: attributes.max_partitions_amount.map(|itm| itm as i32),
            max_rows_per_partition_amount: attributes
                .max_rows_per_partition_amount
                .map(|itm| itm as i32),
        }),
    }
}

/// The id is carried over as the client declared it, and it is the client's to
/// declare: it is a constant folded out of the entity's type at compile time,
/// and this server never recomputes it. What it does check is that an id it
/// already knows arrives with the bytes it knows it by, on the one path every
/// write goes through - `db_operations::write::build_db_row`. This conversion has
/// nowhere to say no, and a `From` which quietly rewrote the id would leave the
/// client believing its rows are stored under the number it named.
impl From<EntitySchemaGrpcModel> for EntitySchema {
    fn from(src: EntitySchemaGrpcModel) -> Self {
        EntitySchema::new(src.schema_id, src.schema)
    }
}
