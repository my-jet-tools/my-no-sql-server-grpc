use std::pin::Pin;
use std::sync::Arc;

use futures_core::Stream;
use my_no_sql_grpc_abstractions::db_entity::{DbEntityParseFail, ParsedEntity};
use my_no_sql_grpc_core::db::{BulkWriteMode, DbRow, DbTable, GetRowsFilter};
use rust_extensions::date_time::DateTimeAsMicroseconds;
use tonic::{Request, Response, Status};

use crate::app::DbNamespace;
use crate::data_sync_period::DataSyncPeriod;
use crate::db_operations::{DbOperationError, WriteRowRequest};
use crate::transactions::Transaction;

use crate::my_no_sql_writer_grpc::writer_server::Writer;
use crate::my_no_sql_writer_grpc::*;

use super::{WriterGrpcService, mappers};

/// Rows are handed out in chunks so a table of any size fits into a stream of
/// bounded messages. The limit is on bytes, not on rows: entities differ in size
/// by orders of magnitude between tables.
const CHUNK_SIZE_LIMIT: usize = 1024 * 1024;

type GrpcStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl Writer for WriterGrpcService {
    type GetTablesStream = GrpcStream<TableGrpcModel>;
    type GetRowsStream = GrpcStream<DbRowsChunkGrpcModel>;
    type GetNamespacesStream = GrpcStream<NamespaceGrpcModel>;
    type MakeBackupStream = GrpcStream<TakenBackupGrpcModel>;
    type GetBackupsStream = GrpcStream<BackupGrpcModel>;
    type DownloadBackupStream = GrpcStream<BackupChunkGrpcModel>;
    type InspectBackupStream = GrpcStream<BackupTableGrpcModel>;
    type GetBackupRowsStream = GrpcStream<DbRowsChunkGrpcModel>;
    type GetRowsWithSchemaStream = GrpcStream<DbRowsWithSchemaGrpcModel>;
    type GetHighestRowAndBelowStream = GrpcStream<DbRowsChunkGrpcModel>;
    type GetSinglePartitionMultipleRowsStream = GrpcStream<DbRowsChunkGrpcModel>;

    async fn ping(&self, _: Request<()>) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn create_table(
        &self,
        request: Request<CreateTableGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();
        let db_namespace = self.get_or_create_namespace(&request.name_space).await?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::create_table(
            &db_namespace,
            &request.table_name,
            mappers::to_db_table_attributes(request.attributes),
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        )?;

        Ok(Response::new(()))
    }

    async fn create_table_if_not_exists(
        &self,
        request: Request<CreateTableGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();
        let db_namespace = self.get_or_create_namespace(&request.name_space).await?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::create_table_if_not_exists(
            &self.app,
            &db_namespace,
            &request.table_name,
            mappers::to_db_table_attributes(request.attributes),
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        );

        Ok(Response::new(()))
    }

    /// The one operational call that changes nothing in memory: it takes what
    /// the persist queue is holding to disk ahead of the sync periods each
    /// change asked for. Nothing is told to a reader, because nothing changed
    /// for one.
    async fn flush_to_disk(
        &self,
        _: Request<()>,
    ) -> Result<Response<FlushToDiskGrpcResponse>, Status> {
        // Not optional: flushing while the tables are still being loaded would
        // write half a table over the whole one on disk.
        check_if_initialized(&self.app.states)?;

        let tasks_written = crate::operations::flush(&self.app).await;

        Ok(Response::new(FlushToDiskGrpcResponse {
            tasks_written: tasks_written as i32,
        }))
    }

    async fn make_backup(
        &self,
        _: Request<()>,
    ) -> Result<Response<Self::MakeBackupStream>, Status> {
        check_if_initialized(&self.app.states)?;

        let taken = crate::db_operations::backup::make(&self.app, DateTimeAsMicroseconds::now())
            .await
            .map_err(DbOperationError::BackupFailed)?;

        let models: Vec<Result<TakenBackupGrpcModel, Status>> = taken
            .into_iter()
            .map(|itm| {
                Ok(TakenBackupGrpcModel {
                    name_space: itm.name_space,
                    name: itm.name,
                })
            })
            .collect();

        Ok(Response::new(Box::pin(tokio_stream::iter(models))))
    }

    async fn get_backups(
        &self,
        request: Request<GetBackupsGrpcRequest>,
    ) -> Result<Response<Self::GetBackupsStream>, Status> {
        let backups =
            crate::db_operations::backup::get_all(&self.app, &request.into_inner().name_space)
                .await
                .map_err(DbOperationError::BackupFailed)?;

        let models: Vec<Result<BackupGrpcModel, Status>> = backups
            .into_iter()
            .map(|itm| {
                Ok(BackupGrpcModel {
                    name: itm.name,
                    size: itm.size as i64,
                })
            })
            .collect();

        Ok(Response::new(Box::pin(tokio_stream::iter(models))))
    }

    async fn inspect_backup(
        &self,
        request: Request<BackupGrpcRequest>,
    ) -> Result<Response<Self::InspectBackupStream>, Status> {
        let request = request.into_inner();

        let content =
            crate::db_operations::backup::inspect(&self.app, &request.name_space, &request.name)
                .await
                .map_err(DbOperationError::BackupFailed)?;

        let mut models: Vec<Result<BackupTableGrpcModel, Status>> = Vec::new();

        for table in content.tables {
            let mut partitions = Vec::new();

            for partition in table.partitions {
                // Unpacked only far enough to count the rows - nothing here
                // restores anything.
                let rows_amount = crate::persist::partition_blob::deserialize(&partition.blob)
                    .map_err(DbOperationError::BackupFailed)?
                    .len();

                partitions.push(BackupPartitionGrpcModel {
                    partition_key: partition.partition_key,
                    rows_amount: rows_amount as i32,
                });
            }

            models.push(Ok(BackupTableGrpcModel {
                table_name: table.table_name,
                partitions,
            }));
        }

        Ok(Response::new(Box::pin(tokio_stream::iter(models))))
    }

    async fn get_backup_rows(
        &self,
        request: Request<GetBackupRowsGrpcRequest>,
    ) -> Result<Response<Self::GetBackupRowsStream>, Status> {
        let request = request.into_inner();

        let rows = crate::db_operations::backup::get_rows(
            &self.app,
            &request.name_space,
            &request.name,
            &request.table_name,
            &request.partition_key,
        )
        .await
        .map_err(DbOperationError::BackupFailed)?;

        let chunks: Vec<Result<DbRowsChunkGrpcModel, Status>> =
            vec![Ok(DbRowsChunkGrpcModel { rows })];

        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }

    /// The archive itself, cut into messages the transport can carry.
    async fn download_backup(
        &self,
        request: Request<BackupGrpcRequest>,
    ) -> Result<Response<Self::DownloadBackupStream>, Status> {
        let request = request.into_inner();

        let content =
            crate::db_operations::backup::download(&self.app, &request.name_space, &request.name)
                .await
                .map_err(DbOperationError::BackupFailed)?;

        let chunks: Vec<Result<BackupChunkGrpcModel, Status>> = content
            .chunks(CHUNK_SIZE_LIMIT)
            .map(|chunk| {
                Ok(BackupChunkGrpcModel {
                    name_space: request.name_space.clone(),
                    chunk: chunk.to_vec(),
                })
            })
            .collect();

        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }

    /// The same bytes going the other way. It is kept rather than restored:
    /// restoring is a separate call, and an archive worth keeping is worth
    /// looking inside first.
    async fn upload_backup(
        &self,
        request: Request<tonic::Streaming<BackupChunkGrpcModel>>,
    ) -> Result<Response<UploadBackupGrpcResponse>, Status> {
        check_if_initialized(&self.app.states)?;

        self.apply_upload_backup(request.into_inner()).await
    }

    async fn restore_backup(
        &self,
        request: Request<RestoreBackupGrpcRequest>,
    ) -> Result<Response<RestoreBackupGrpcResponse>, Status> {
        check_if_initialized(&self.app.states)?;

        let request = request.into_inner();
        let now = DateTimeAsMicroseconds::now();

        let restored = crate::db_operations::backup::restore(
            &self.app,
            &request.name_space,
            &request.name,
            to_restore_one(&request)?,
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        )
        .await?;

        Ok(Response::new(RestoreBackupGrpcResponse {
            partitions_restored: restored as i32,
        }))
    }

    async fn get_namespaces(
        &self,
        _: Request<()>,
    ) -> Result<Response<Self::GetNamespacesStream>, Status> {
        check_if_initialized(&self.app.states)?;

        let models: Vec<Result<NamespaceGrpcModel, Status>> = self
            .app
            .namespaces
            .get_all()
            .iter()
            .map(|db_namespace| {
                Ok(NamespaceGrpcModel {
                    name: db_namespace.name.clone(),
                    tables_amount: db_namespace.tables.get_tables().len() as i32,
                })
            })
            .collect();

        Ok(Response::new(Box::pin(tokio_stream::iter(models))))
    }

    async fn delete_namespace(
        &self,
        request: Request<DeleteNamespaceGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        check_if_initialized(&self.app.states)?;

        let request = request.into_inner();

        crate::db_operations::write::delete_namespace(
            &self.app,
            &request.name_space,
            DateTimeAsMicroseconds::now(),
        )
        .await?;

        Ok(Response::new(()))
    }

    async fn move_table_to_namespace(
        &self,
        request: Request<MoveTableToNamespaceGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let destination = self
            .get_or_create_namespace(&request.destination_name_space)
            .await?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::move_table_to_namespace(
            &self.app,
            &db_namespace,
            &request.table_name,
            &destination,
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        )
        .await?;

        Ok(Response::new(()))
    }

    async fn get_highest_row_and_below(
        &self,
        request: Request<GetHighestRowAndBelowGrpcRequest>,
    ) -> Result<Response<Self::GetHighestRowAndBelowStream>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let db_rows = db_table.get_highest_row_and_below(
            &request.partition_key,
            &request.row_key,
            to_optional_amount(request.limit),
        );

        Ok(Response::new(into_stream(db_rows)))
    }

    async fn get_single_partition_multiple_rows(
        &self,
        request: Request<GetSinglePartitionMultipleRowsGrpcRequest>,
    ) -> Result<Response<Self::GetSinglePartitionMultipleRowsStream>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let db_rows =
            db_table.get_single_partition_multiple_rows(&request.partition_key, &request.row_keys);

        Ok(Response::new(into_stream(db_rows)))
    }

    async fn get_table_size(
        &self,
        request: Request<GetTableSizeGrpcRequest>,
    ) -> Result<Response<TableSizeGrpcResponse>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        // One acquisition of the read lock for the three of them, which is what
        // the contract already promises: they are read together far more often
        // than apart, and taken one at a time they can belong to three moments.
        let metrics = db_table.get_metrics();

        Ok(Response::new(TableSizeGrpcResponse {
            partitions_amount: metrics.partitions_amount as i64,
            rows_amount: metrics.rows_amount as i64,
            content_size: metrics.content_size as i64,
        }))
    }

    /// Rows together with the schema they were written under - the shape a write
    /// comes in as, so what one server hands out is what another one takes in.
    async fn get_rows_with_schema(
        &self,
        request: Request<GetRowsWithSchemaGrpcRequest>,
    ) -> Result<Response<Self::GetRowsWithSchemaStream>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let mut chunks: Vec<Result<DbRowsWithSchemaGrpcModel, Status>> = Vec::new();

        for group in crate::db_operations::migrate::get_rows_with_schema(&db_table) {
            let schema = group.schema.map(|schema| EntitySchemaGrpcModel {
                schema_id: schema.id,
                schema: schema.schema.clone(),
            });

            for rows in into_chunks(group.rows) {
                chunks.push(Ok(DbRowsWithSchemaGrpcModel {
                    schema: schema.clone(),
                    rows: rows.rows,
                }));
            }
        }

        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }

    async fn migrate_from(
        &self,
        request: Request<MigrateFromGrpcRequest>,
    ) -> Result<Response<MigrateFromGrpcResponse>, Status> {
        check_if_initialized(&self.app.states)?;

        let request = request.into_inner();
        let now = DateTimeAsMicroseconds::now();
        let persist_moment = mappers::to_data_sync_period(request.sync_period).get_sync_moment(now);

        let migrated =
            crate::db_operations::migrate::migrate_from(&self.app, request, persist_moment).await?;

        Ok(Response::new(MigrateFromGrpcResponse {
            rows_migrated: migrated as i32,
        }))
    }

    async fn get_tables(
        &self,
        request: Request<GetTablesGrpcRequest>,
    ) -> Result<Response<Self::GetTablesStream>, Status> {
        let request = request.into_inner();
        let db_namespace = self.get_or_create_namespace(&request.name_space).await?;

        let tables = crate::db_operations::read::get_tables(&db_namespace);

        let models: Vec<Result<TableGrpcModel, Status>> = tables
            .iter()
            .map(|db_table| Ok(mappers::to_table_grpc_model(db_table)))
            .collect();

        Ok(Response::new(Box::pin(tokio_stream::iter(models))))
    }

    async fn insert(&self, request: Request<WriteRowGrpcRequest>) -> Result<Response<()>, Status> {
        let write = self.prepare_write(request.into_inner()).await?;

        crate::db_operations::write::insert(
            &self.app,
            &write.db_namespace,
            &write.db_table,
            write.db_row,
            write.persist_moment,
        )?;

        Ok(Response::new(()))
    }

    async fn insert_or_replace(
        &self,
        request: Request<WriteRowGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let write = self.prepare_write(request.into_inner()).await?;

        crate::db_operations::write::insert_or_replace(
            &self.app,
            &write.db_namespace,
            &write.db_table,
            write.db_row,
            write.persist_moment,
        );

        Ok(Response::new(()))
    }

    /// `UseClientTimeStamp` is forced on for the same reason the batch mode
    /// forces it: what decides is the entity's own TimeStamp against the stored
    /// one, and stamping the server's clock on the incoming row would make every
    /// row the newer one and the whole call an `InsertOrReplace`.
    async fn insert_or_replace_if_new(
        &self,
        request: Request<WriteRowGrpcRequest>,
    ) -> Result<Response<InsertOrReplaceIfNewGrpcResponse>, Status> {
        let mut request = request.into_inner();
        request.use_client_time_stamp = true;

        let write = self.prepare_write(request).await?;

        let written = crate::db_operations::write::insert_or_replace_if_new(
            &self.app,
            &write.db_namespace,
            &write.db_table,
            write.db_row,
            write.persist_moment,
        );

        Ok(Response::new(InsertOrReplaceIfNewGrpcResponse { written }))
    }

    /// Optimistic concurrency, and the reason the entity has to carry a
    /// TimeStamp: it is the version the client read the row at, and the write is
    /// refused unless the stored row is still that version. The row that lands
    /// is stamped with the server's clock like every other write, so the next
    /// reader gets a new version - which is what makes a read-modify-write loop
    /// end rather than spin.
    async fn replace(&self, request: Request<WriteRowGrpcRequest>) -> Result<Response<()>, Status> {
        let mut request = request.into_inner();

        let expected_time_stamp = expected_version(&request.row)?;

        // Forced off for the opposite reason InsertOrReplaceIfNew forces it on:
        // keeping the entity's own TimeStamp would store the version the client
        // read back as the stored version, and the next writer holding the same
        // read would be let through as well - the check would pass twice for one
        // version.
        request.use_client_time_stamp = false;

        let write = self.prepare_write(request).await?;

        crate::db_operations::write::replace(
            &self.app,
            &write.db_namespace,
            &write.db_table,
            write.db_row,
            expected_time_stamp,
            write.persist_moment,
        )?;

        Ok(Response::new(()))
    }

    /// A key which is not there is answered `Deleted = false`, not with an
    /// error: the same input inside a `BulkDelete` is not an error either, and
    /// one of the two would have to be wrong.
    async fn delete_row(
        &self,
        request: Request<DeleteRowGrpcRequest>,
    ) -> Result<Response<DeleteRowGrpcResponse>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let now = DateTimeAsMicroseconds::now();

        let deleted = crate::db_operations::write::delete_row(
            &self.app,
            &db_namespace,
            &db_table,
            &request.partition_key,
            &request.row_key,
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        );

        Ok(Response::new(DeleteRowGrpcResponse { deleted }))
    }

    /// Deleting many rows at once. A transaction could do the same, but this is
    /// what it looks like when deleting is all there is: one call rather than
    /// three, and no id to keep alive in between.
    async fn bulk_delete(
        &self,
        request: Request<BulkDeleteGrpcRequest>,
    ) -> Result<Response<BulkDeleteGrpcResponse>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let now = DateTimeAsMicroseconds::now();

        let deleted = crate::db_operations::write::bulk_delete(
            &self.app,
            &db_namespace,
            &db_table,
            request
                .partitions
                .into_iter()
                .map(mappers::to_partition_row_keys)
                .collect(),
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        );

        Ok(Response::new(BulkDeleteGrpcResponse {
            rows_deleted: deleted as i32,
        }))
    }

    /// The stream is transport and nothing else: every entity is collected
    /// first, and the batch is applied in one entry into the table once the
    /// client is done sending. Nothing is written before that, so a stream which
    /// breaks - or a message the server refuses - leaves the table exactly as it
    /// was. That is what stands in for the transaction this server does not have.
    async fn bulk_write(
        &self,
        request: Request<tonic::Streaming<BulkWriteGrpcRequest>>,
    ) -> Result<Response<()>, Status> {
        self.apply_bulk_write(request.into_inner()).await
    }

    /// Opens a transaction against one table. The table is resolved here so a
    /// client learns straight away that it does not exist, and again at commit,
    /// which is the only moment that counts.
    async fn start_transaction(
        &self,
        request: Request<StartTransactionGrpcRequest>,
    ) -> Result<Response<StartTransactionGrpcResponse>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let transaction = self.app.transactions.create(
            db_namespace.name.clone(),
            db_table.name.clone(),
            mappers::to_data_sync_period(request.sync_period),
        );

        Ok(Response::new(StartTransactionGrpcResponse {
            transaction_id: transaction.id.clone(),
        }))
    }

    async fn post_transaction_actions(
        &self,
        request: Request<tonic::Streaming<TransactionActionGrpcModel>>,
    ) -> Result<Response<()>, Status> {
        self.apply_transaction_actions(request.into_inner()).await
    }

    /// The only moment a transaction touches a table. It leaves the registry
    /// first: from here on nothing can post to it, so what is applied is exactly
    /// what was accumulated.
    async fn commit_transaction(
        &self,
        request: Request<TransactionGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let Some(transaction) = self.app.transactions.remove(&request.transaction_id) else {
            return Err(unknown_transaction(&request.transaction_id));
        };

        let Some(db_namespace) = self.app.namespaces.get(&transaction.namespace) else {
            return Err(DbOperationError::NamespaceNotFound(transaction.namespace.clone()).into());
        };

        let db_table =
            crate::db_operations::read::get_table(&db_namespace, &transaction.table_name)?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::commit_transaction(
            &self.app,
            &db_namespace,
            &db_table,
            transaction.take_actions(),
            transaction.sync_period.get_sync_moment(now),
        );

        Ok(Response::new(()))
    }

    /// Throwing a transaction away is the one call which does not mind that it
    /// is already gone: nothing it accumulated ever reached a table, so a client
    /// can always cancel in its cleanup path without checking first.
    async fn cancel_transaction(
        &self,
        request: Request<TransactionGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        self.app
            .transactions
            .remove(&request.into_inner().transaction_id);

        Ok(Response::new(()))
    }

    async fn clean_table(
        &self,
        request: Request<CleanTableGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::clean_table(
            &self.app,
            &db_namespace,
            &db_table,
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        );

        Ok(Response::new(()))
    }

    async fn delete_table(
        &self,
        request: Request<DeleteTableGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::delete_table(
            &self.app,
            &db_namespace,
            &request.table_name,
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        )?;

        Ok(Response::new(()))
    }

    async fn delete_partitions(
        &self,
        request: Request<DeletePartitionsGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::delete_partitions(
            &self.app,
            &db_namespace,
            &db_table,
            &request.partition_keys,
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        );

        Ok(Response::new(()))
    }

    async fn set_table_attributes(
        &self,
        request: Request<SetTableAttributesGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::write::set_table_attributes(
            &self.app,
            &db_namespace,
            &db_table,
            mappers::to_db_table_attributes(request.attributes),
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        );

        Ok(Response::new(()))
    }

    async fn clean_and_keep_max_partitions(
        &self,
        request: Request<CleanAndKeepMaxPartitionsGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::gc::keep_max_partitions_amount(
            &self.app,
            &db_namespace,
            &db_table,
            to_limit(request.max_partitions_amount, "MaxPartitionsAmount")?,
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        );

        Ok(Response::new(()))
    }

    async fn clean_partition_and_keep_max_rows(
        &self,
        request: Request<CleanPartitionAndKeepMaxRowsGrpcRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let now = DateTimeAsMicroseconds::now();

        crate::db_operations::gc::keep_max_rows_in_partition(
            &self.app,
            &db_namespace,
            &db_table,
            &request.partition_key,
            to_limit(request.max_rows_amount, "MaxRowsAmount")?,
            mappers::to_data_sync_period(request.sync_period).get_sync_moment(now),
        );

        Ok(Response::new(()))
    }

    async fn get_row(
        &self,
        request: Request<GetRowGrpcRequest>,
    ) -> Result<Response<GetRowGrpcResponse>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let db_row = crate::db_operations::read::get_row(
            &db_table,
            &request.partition_key,
            &request.row_key,
        );

        Ok(Response::new(GetRowGrpcResponse {
            row: db_row.map(|db_row| db_row.to_vec()),
        }))
    }

    async fn get_rows(
        &self,
        request: Request<GetRowsGrpcRequest>,
    ) -> Result<Response<Self::GetRowsStream>, Status> {
        let request = request.into_inner();

        let db_namespace = self.get_namespace(&request.name_space)?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        // The snapshot is taken here, once: the stream is fed from these `Arc`s
        // and never touches the table again, so a concurrent write can not make
        // the reader see half of one state and half of another.
        let db_rows = crate::db_operations::read::get_rows(
            &db_table,
            &GetRowsFilter {
                partition_key: request.partition_key.as_deref(),
                row_key: request.row_key.as_deref(),
                skip: to_optional_amount(request.skip),
                limit: to_optional_amount(request.limit),
            },
        );

        let chunks: Vec<Result<DbRowsChunkGrpcModel, Status>> =
            into_chunks(db_rows).into_iter().map(Ok).collect();

        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }
}

/// The header a bulk write repeats on every message of its stream.
///
/// It describes the operation, not the message, so every copy has to say the
/// same thing: a message which disagrees is refused. Refusing costs nothing
/// here, because the batch is applied only after the stream ends, so a refused
/// message leaves the table untouched instead of half written.
struct BulkWriteHeader {
    name_space: String,
    table_name: String,
    mode: BulkWriteMode,
    sync_period: DataSyncPeriod,
    /// `InsertOrReplaceIfNew` compares the entity's own TimeStamp against the
    /// stored one, so the server's clock has no place in it and the flag is
    /// forced on whatever the client asked for.
    use_client_time_stamp: bool,
}

impl BulkWriteHeader {
    fn new(request: &BulkWriteGrpcRequest) -> Result<Self, Status> {
        let mode = mappers::to_bulk_write_mode(request.mode)?;

        Ok(Self {
            // Resolved, not raw: an empty name and "default" are the same
            // namespace, and refusing a batch over that would be refusing over
            // nothing.
            name_space: crate::app::DbNamespaces::resolve_name(&request.name_space).to_string(),
            table_name: request.table_name.clone(),
            mode,
            sync_period: mappers::to_data_sync_period(request.sync_period),
            use_client_time_stamp: request.use_client_time_stamp
                || mode == BulkWriteMode::InsertOrReplaceIfNew,
        })
    }

    /// What is compared is what the server made of the fields rather than the
    /// raw ones, so two spellings of the same thing are the same thing here.
    fn check_matches(&self, request: &BulkWriteGrpcRequest) -> Result<(), Status> {
        let other = Self::new(request)?;

        if self.name_space != other.name_space
            || self.table_name != other.table_name
            || self.mode != other.mode
            || self.sync_period != other.sync_period
            || self.use_client_time_stamp != other.use_client_time_stamp
        {
            return Err(Status::invalid_argument(
                "Every message of a bulk write repeats the same header, and this one disagrees with the first",
            ));
        }

        Ok(())
    }
}

/// Takes the entities of one message apart and appends them to the batch. The
/// schema is per message, so a batch may carry several entity versions and every
/// row remembers the one it arrived with.
fn append_rows(
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    header: &BulkWriteHeader,
    request: BulkWriteGrpcRequest,
    dest: &mut Vec<Arc<DbRow>>,
) -> Result<(), Status> {
    let Some(schema) = request.schema else {
        return Err(Status::invalid_argument(
            "The entity schema is required on every message of a bulk write",
        ));
    };

    dest.extend(crate::db_operations::write::build_db_rows(
        db_namespace,
        db_table,
        schema.into(),
        &request.rows,
        header.use_client_time_stamp,
        DateTimeAsMicroseconds::now(),
    )?);

    Ok(())
}

/// Everything a write needs, resolved once so the handlers stay one call long.
struct PreparedWrite {
    db_namespace: Arc<DbNamespace>,
    db_table: Arc<DbTable>,
    db_row: Arc<DbRow>,
    persist_moment: DateTimeAsMicroseconds,
}

impl WriterGrpcService {
    async fn get_or_create_namespace(&self, name: &str) -> Result<Arc<DbNamespace>, Status> {
        check_if_initialized(&self.app.states)?;

        Ok(self
            .app
            .namespaces
            .get_or_create(name, &self.app.settings)
            .await?)
    }

    /// A namespace which does not exist holds no tables, so a request against it
    /// fails the same way a request against a missing table does - and creating
    /// it as a side effect of a read would be surprising.
    fn get_namespace(&self, name: &str) -> Result<Arc<DbNamespace>, Status> {
        check_if_initialized(&self.app.states)?;

        match self.app.namespaces.get(name) {
            Some(db_namespace) => Ok(db_namespace),
            None => Err(Status::not_found(format!(
                "Namespace '{}' is not found",
                crate::app::DbNamespaces::resolve_name(name)
            ))),
        }
    }

    /// The stream is transport and nothing else: every entity is collected
    /// first, and the batch is applied in one entry into the table once the
    /// client is done sending. Nothing is written before that, so a stream which
    /// breaks - or a message the server refuses - leaves the table exactly as it
    /// was. That is what stands in for the transaction this server does not have.
    ///
    /// It takes any stream of requests rather than `tonic::Streaming` because
    /// the transport is genuinely irrelevant to it, which is also what makes it
    /// testable without a socket.
    pub(crate) async fn apply_bulk_write(
        &self,
        stream: impl futures_core::Stream<Item = Result<BulkWriteGrpcRequest, Status>>,
    ) -> Result<Response<()>, Status> {
        use tokio_stream::StreamExt;

        let mut stream = std::pin::pin!(stream);

        let Some(first) = stream.next().await.transpose()? else {
            return Err(Status::invalid_argument(
                "A bulk write carries its header on every message, so a stream with no messages names nothing to write to",
            ));
        };

        let header = BulkWriteHeader::new(&first)?;

        let db_namespace = self.get_or_create_namespace(&first.name_space).await?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &first.table_name)?;

        let mut rows = Vec::new();
        append_rows(&db_namespace, &db_table, &header, first, &mut rows)?;

        while let Some(message) = stream.next().await.transpose()? {
            header.check_matches(&message)?;
            append_rows(&db_namespace, &db_table, &header, message, &mut rows)?;
        }

        crate::db_operations::write::bulk_write(
            &self.app,
            &db_namespace,
            &db_table,
            header.mode,
            rows,
            header
                .sync_period
                .get_sync_moment(DateTimeAsMicroseconds::now()),
        );

        Ok(Response::new(()))
    }

    /// The bulky part of a transaction still streams. Nothing reaches the
    /// transaction until the stream ends cleanly, so a stream which breaks - or
    /// a message the server refuses - leaves the transaction exactly as it was
    /// and the whole post can simply be repeated.
    /// The upload is a stream because an archive is not a message. Nothing is
    /// kept until it ends cleanly, so a stream which breaks leaves no half a
    /// backup behind looking like a whole one.
    pub(crate) async fn apply_upload_backup(
        &self,
        stream: impl futures_core::Stream<Item = Result<BackupChunkGrpcModel, Status>>,
    ) -> Result<Response<UploadBackupGrpcResponse>, Status> {
        use tokio_stream::StreamExt;

        let mut stream = std::pin::pin!(stream);

        let Some(first) = stream.next().await.transpose()? else {
            return Err(Status::invalid_argument(
                "An upload carries the namespace on its first message, so a stream with no messages names none",
            ));
        };

        let name_space = first.name_space.clone();
        let mut content = first.chunk;

        while let Some(message) = stream.next().await.transpose()? {
            content.extend_from_slice(&message.chunk);
        }

        let name = crate::db_operations::backup::upload(
            &self.app,
            &name_space,
            &content,
            DateTimeAsMicroseconds::now(),
        )
        .await
        .map_err(DbOperationError::BackupFailed)?;

        Ok(Response::new(UploadBackupGrpcResponse { name }))
    }

    pub(crate) async fn apply_transaction_actions(
        &self,
        stream: impl futures_core::Stream<Item = Result<TransactionActionGrpcModel, Status>>,
    ) -> Result<Response<()>, Status> {
        use tokio_stream::StreamExt;

        let mut stream = std::pin::pin!(stream);

        let Some(first) = stream.next().await.transpose()? else {
            return Err(Status::invalid_argument(
                "Every message of a post names its transaction, so a stream with no messages names none",
            ));
        };

        let transaction = self.get_transaction(&first.transaction_id)?;

        let Some(db_namespace) = self.app.namespaces.get(&transaction.namespace) else {
            return Err(DbOperationError::NamespaceNotFound(transaction.namespace.clone()).into());
        };

        // The table is resolved here as well as on the commit, because a row
        // arriving now is parsed now - and the schema it carries belongs to the
        // table it is aimed at, which is the transaction's, named once when it
        // was opened.
        let db_table =
            crate::db_operations::read::get_table(&db_namespace, &transaction.table_name)?;

        let now = DateTimeAsMicroseconds::now();

        let mut actions = vec![mappers::to_transaction_action(
            &db_namespace,
            &db_table,
            first,
            now,
        )?];

        while let Some(message) = stream.next().await.transpose()? {
            if message.transaction_id != transaction.id {
                return Err(Status::invalid_argument(
                    "Every message of a post names the same transaction, and this one names another",
                ));
            }

            actions.push(mappers::to_transaction_action(
                &db_namespace,
                &db_table,
                message,
                now,
            )?);
        }

        transaction.append(actions);
        transaction.touch();

        Ok(Response::new(()))
    }

    /// A transaction the server has forgotten is the same to a client as one it
    /// never had: nothing it accumulated was applied, so starting over is the
    /// only thing to do about it.
    fn get_transaction(&self, transaction_id: &str) -> Result<Arc<Transaction>, Status> {
        check_if_initialized(&self.app.states)?;

        match self.app.transactions.get(transaction_id) {
            Some(transaction) => {
                transaction.touch();
                Ok(transaction)
            }
            None => Err(unknown_transaction(transaction_id)),
        }
    }

    async fn prepare_write(&self, request: WriteRowGrpcRequest) -> Result<PreparedWrite, Status> {
        let Some(schema) = request.schema else {
            return Err(Status::invalid_argument(
                "The entity schema is required on every write",
            ));
        };

        let db_namespace = self.get_or_create_namespace(&request.name_space).await?;
        let db_table = crate::db_operations::read::get_table(&db_namespace, &request.table_name)?;

        let prepared = crate::db_operations::write::build_db_row(
            &db_namespace,
            &db_table,
            WriteRowRequest {
                schema: schema.into(),
                row: request.row,
                use_client_time_stamp: request.use_client_time_stamp,
                sync_period: mappers::to_data_sync_period(request.sync_period),
            },
            DateTimeAsMicroseconds::now(),
        )?;

        Ok(PreparedWrite {
            db_namespace,
            db_table,
            db_row: prepared.db_row,
            persist_moment: prepared.persist_moment,
        })
    }
}

/// The server states carry the "still loading from disk" flag, and every request
/// has to bounce off it rather than read a half-loaded table.
fn check_if_initialized(states: &rust_extensions::AppStates) -> Result<(), Status> {
    if states.is_initialized() {
        return Ok(());
    }

    Err(DbOperationError::NotInitialized.into())
}

/// Naming a partition restores that one alone. Both parts of the name have to be
/// there or neither - half a name is a request nobody can act on.
fn to_restore_one(
    src: &RestoreBackupGrpcRequest,
) -> Result<Option<crate::db_operations::backup::RestoreOne>, Status> {
    match (src.table_name.as_ref(), src.partition_key.as_ref()) {
        (None, None) => Ok(None),
        (Some(table_name), Some(partition_key)) => {
            Ok(Some(crate::db_operations::backup::RestoreOne {
                table_name: table_name.clone(),
                partition_key: partition_key.clone(),
            }))
        }
        _ => Err(Status::invalid_argument(
            "Restoring one partition needs both TableName and PartitionKey; restoring the whole namespace needs neither",
        )),
    }
}

/// The version a `Replace` is based on, read out of the entity the client sent.
///
/// The entity is parsed here as well as on its way to becoming a row, because
/// what `Replace` needs from it is not the row: it is the moment the client read
/// at, and that moment is not in the row being built - that one carries this
/// write's own TimeStamp.
fn expected_version(row: &[u8]) -> Result<DateTimeAsMicroseconds, Status> {
    let parsed = ParsedEntity::parse(row).map_err(DbOperationError::from)?;

    let Some(time_stamp) = parsed.time_stamp else {
        return Err(
            DbOperationError::from(DbEntityParseFail::TimeStampIsRequired {
                partition_key: parsed.get_partition_key().to_string(),
                row_key: parsed.get_row_key().to_string(),
            })
            .into(),
        );
    };

    Ok(DateTimeAsMicroseconds::new(time_stamp))
}

fn unknown_transaction(transaction_id: &str) -> Status {
    Status::not_found(format!("Transaction '{transaction_id}' is unknown"))
}

/// A limit somebody asked for by hand. `0` is "keep nothing", which is a
/// coherent thing to ask; a negative one is not, and treating it as "no limit"
/// would turn a caller's mistake into an operation that quietly did nothing.
fn to_limit(src: i32, name: &str) -> Result<usize, Status> {
    if src < 0 {
        return Err(Status::invalid_argument(format!(
            "{name} can not be negative"
        )));
    }

    Ok(src as usize)
}

fn to_optional_amount(src: Option<i32>) -> Option<usize> {
    let value = src?;

    if value <= 0 {
        return None;
    }

    Some(value as usize)
}

/// A snapshot taken once and streamed from these `Arc`s - the stream never
/// touches the table again, so a concurrent write can not make the reader see
/// half of one state and half of another.
fn into_stream(db_rows: Vec<Arc<DbRow>>) -> GrpcStream<DbRowsChunkGrpcModel> {
    let chunks: Vec<Result<DbRowsChunkGrpcModel, Status>> =
        into_chunks(db_rows).into_iter().map(Ok).collect();

    Box::pin(tokio_stream::iter(chunks))
}

fn into_chunks(db_rows: Vec<Arc<DbRow>>) -> Vec<DbRowsChunkGrpcModel> {
    let mut result = Vec::new();

    let mut rows = Vec::new();
    let mut chunk_size = 0;

    for db_row in db_rows {
        chunk_size += db_row.get_content_size();
        rows.push(db_row.to_vec());

        if chunk_size >= CHUNK_SIZE_LIMIT {
            result.push(DbRowsChunkGrpcModel {
                rows: std::mem::take(&mut rows),
            });
            chunk_size = 0;
        }
    }

    if !rows.is_empty() {
        result.push(DbRowsChunkGrpcModel { rows });
    }

    result
}
