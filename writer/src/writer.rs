use std::marker::PhantomData;

use my_no_sql_grpc_core::MyNoSqlEntity;

use crate::my_no_sql_writer_grpc::*;
use crate::{
    MAX_MESSAGE_SIZE, MyNoSqlGrpcConnection, MyNoSqlGrpcTransaction, MyNoSqlWriterError,
    build_schema,
};

/// Everything a server-streaming call sent back.
async fn collect_stream<TItem>(
    mut stream: tonic::Streaming<TItem>,
) -> Result<Vec<TItem>, MyNoSqlWriterError> {
    let mut result = Vec::new();

    while let Some(item) = stream.message().await? {
        result.push(item);
    }

    Ok(result)
}

/// Reads a stream of row chunks into entities.
async fn collect<TEntity: MyNoSqlEntity>(
    mut stream: tonic::Streaming<DbRowsChunkGrpcModel>,
) -> Result<Vec<TEntity>, MyNoSqlWriterError> {
    let mut result = Vec::new();

    while let Some(chunk) = stream.message().await? {
        for row in chunk.rows {
            result.push(TEntity::from_slice(&row)?);
        }
    }

    Ok(result)
}

/// Writes one entity type into one table.
///
/// It is generic over the entity because the schema is: it is built once, here,
/// instead of on every write - and its id costs nothing at all, being a constant
/// of the type - and the table name comes from the entity rather than from the
/// call site, so it can not be misspelled in one place out of ten.
pub struct MyNoSqlGrpcWriter<TEntity: MyNoSqlEntity> {
    connection: MyNoSqlGrpcConnection,
    name_space: String,
    schema: EntitySchemaGrpcModel,
    sync_period: SyncPeriodGrpcModel,
    use_client_time_stamp: bool,
    itm: PhantomData<TEntity>,
}

impl<TEntity: MyNoSqlEntity> MyNoSqlGrpcWriter<TEntity> {
    pub fn new(connection: MyNoSqlGrpcConnection) -> Self {
        Self {
            connection,
            name_space: String::new(),
            schema: build_schema::<TEntity>(),
            sync_period: SyncPeriodGrpcModel::SyncPeriodDefault,
            use_client_time_stamp: false,
            itm: PhantomData,
        }
    }

    /// An empty name is the default namespace, which is what a client that does
    /// not care about namespaces leaves it as.
    pub fn with_name_space(mut self, name_space: impl Into<String>) -> Self {
        self.name_space = name_space.into();
        self
    }

    pub fn with_sync_period(mut self, sync_period: SyncPeriodGrpcModel) -> Self {
        self.sync_period = sync_period;
        self
    }

    /// Keeps the `TimeStamp` the entity carries instead of letting the server
    /// stamp its own clock.
    pub fn with_client_time_stamp(mut self) -> Self {
        self.use_client_time_stamp = true;
        self
    }

    pub fn table_name(&self) -> &'static str {
        TEntity::TABLE_NAME
    }

    // ---- tables ----------------------------------------------------------

    pub async fn create_table_if_not_exists(
        &self,
        attributes: TableAttributesGrpcModel,
    ) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .create_table_if_not_exists(
                self.connection.unary(self.create_table_request(attributes)),
            )
            .await?;

        Ok(())
    }

    pub async fn create_table(
        &self,
        attributes: TableAttributesGrpcModel,
    ) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .create_table(self.connection.unary(self.create_table_request(attributes)))
            .await?;

        Ok(())
    }

    /// Empties the table, keeping the table itself.
    pub async fn clean_table(&self) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .clean_table(self.connection.unary(CleanTableGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                sync_period: self.sync_period as i32,
            }))
            .await?;

        Ok(())
    }

    /// Removes the table itself. Subscribed readers drop it from their cache.
    pub async fn delete_table(&self) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .delete_table(self.connection.unary(DeleteTableGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                sync_period: self.sync_period as i32,
            }))
            .await?;

        Ok(())
    }

    pub async fn delete_partitions(
        &self,
        partition_keys: Vec<String>,
    ) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .delete_partitions(self.connection.unary(DeletePartitionsGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                partition_keys,
                sync_period: self.sync_period as i32,
            }))
            .await?;

        Ok(())
    }

    // ---- operations ------------------------------------------------------

    /// Says nothing and proves the connection. Worth having because the channel
    /// is lazy: without a call, "connected" is a thing nobody has checked yet,
    /// and a service which wants to fail its own start up on an unreachable
    /// database has nothing cheaper to ask.
    pub async fn ping(&self) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .ping(self.connection.unary(()))
            .await?;

        Ok(())
    }

    /// Takes everything the server's persist queue is holding to disk now,
    /// whatever sync period each change asked for, and answers how many tasks
    /// that was.
    ///
    /// It covers what was queued when the call started - a write made while the
    /// flush runs belongs to the next one. Server-wide, like a backup: the
    /// question it answers ("is the disk current before I stop this thing") is
    /// never about one table.
    pub async fn flush_to_disk(&self) -> Result<usize, MyNoSqlWriterError> {
        let response = self
            .connection
            .writer_client()
            .flush_to_disk(self.connection.unary(()))
            .await?;

        Ok(response.into_inner().tasks_written as usize)
    }

    // ---- backups ---------------------------------------------------------
    //
    // A backup is of the whole server, not of one table, so these are not scoped
    // to this writer's own - they are here because this is where a connection
    // already is.

    /// Backs every namespace up, one zip each, and answers with what was taken.
    /// A namespace with no tables makes no file at all.
    pub async fn make_backup(&self) -> Result<Vec<TakenBackupGrpcModel>, MyNoSqlWriterError> {
        let stream = self
            .connection
            .writer_client()
            .make_backup(())
            .await?
            .into_inner();

        collect_stream(stream).await
    }

    pub async fn get_backups(
        &self,
        name_space: &str,
    ) -> Result<Vec<BackupGrpcModel>, MyNoSqlWriterError> {
        let stream = self
            .connection
            .writer_client()
            .get_backups(GetBackupsGrpcRequest {
                name_space: name_space.to_string(),
            })
            .await?
            .into_inner();

        collect_stream(stream).await
    }

    /// What is inside a backup, without restoring any of it.
    pub async fn inspect_backup(
        &self,
        name_space: &str,
        name: &str,
    ) -> Result<Vec<BackupTableGrpcModel>, MyNoSqlWriterError> {
        let stream = self
            .connection
            .writer_client()
            .inspect_backup(BackupGrpcRequest {
                name_space: name_space.to_string(),
                name: name.to_string(),
            })
            .await?
            .into_inner();

        collect_stream(stream).await
    }

    /// The rows of one backed up partition, decoded as this entity. They are
    /// read out of the backup and nothing is restored by it.
    pub async fn get_backup_rows(
        &self,
        name_space: &str,
        name: &str,
        table_name: &str,
        partition_key: &str,
    ) -> Result<Vec<TEntity>, MyNoSqlWriterError> {
        let stream = self
            .connection
            .writer_client()
            .get_backup_rows(GetBackupRowsGrpcRequest {
                name_space: name_space.to_string(),
                name: name.to_string(),
                table_name: table_name.to_string(),
                partition_key: partition_key.to_string(),
            })
            .await?
            .into_inner();

        collect(stream).await
    }

    /// The archive itself - an ordinary zip, which is what makes it worth
    /// downloading at all.
    pub async fn download_backup(
        &self,
        name_space: &str,
        name: &str,
    ) -> Result<Vec<u8>, MyNoSqlWriterError> {
        let mut stream = self
            .connection
            .writer_client()
            .download_backup(BackupGrpcRequest {
                name_space: name_space.to_string(),
                name: name.to_string(),
            })
            .await?
            .into_inner();

        let mut result = Vec::new();

        while let Some(message) = stream.message().await? {
            result.extend_from_slice(&message.chunk);
        }

        Ok(result)
    }

    /// The same bytes going the other way. It is kept, not restored - restoring
    /// is a separate call, and an archive worth keeping is worth looking inside
    /// first.
    pub async fn upload_backup(
        &self,
        name_space: &str,
        content: &[u8],
    ) -> Result<String, MyNoSqlWriterError> {
        let name_space = name_space.to_string();

        let messages: Vec<BackupChunkGrpcModel> = content
            .chunks(MAX_MESSAGE_SIZE)
            .map(|chunk| BackupChunkGrpcModel {
                name_space: name_space.clone(),
                chunk: chunk.to_vec(),
            })
            .collect();

        Ok(self
            .connection
            .writer_client()
            .upload_backup(tokio_stream::iter(messages))
            .await?
            .into_inner()
            .name)
    }

    /// Puts the whole backup back into its namespace. What is restored replaces
    /// what is there.
    pub async fn restore_backup(
        &self,
        name_space: &str,
        name: &str,
    ) -> Result<i32, MyNoSqlWriterError> {
        self.restore(name_space, name, None).await
    }

    /// Puts one partition back - what somebody who lost one thing wants, rather
    /// than rolling the whole namespace back to the moment of the backup.
    pub async fn restore_backup_partition(
        &self,
        name_space: &str,
        name: &str,
        table_name: &str,
        partition_key: &str,
    ) -> Result<i32, MyNoSqlWriterError> {
        self.restore(
            name_space,
            name,
            Some((table_name.to_string(), partition_key.to_string())),
        )
        .await
    }

    async fn restore(
        &self,
        name_space: &str,
        name: &str,
        only: Option<(String, String)>,
    ) -> Result<i32, MyNoSqlWriterError> {
        let (table_name, partition_key) = match only {
            Some((table_name, partition_key)) => (Some(table_name), Some(partition_key)),
            None => (None, None),
        };

        Ok(self
            .connection
            .writer_client()
            .restore_backup(self.connection.unary(RestoreBackupGrpcRequest {
                name_space: name_space.to_string(),
                name: name.to_string(),
                table_name,
                partition_key,
                sync_period: self.sync_period as i32,
            }))
            .await?
            .into_inner()
            .partitions_restored)
    }

    /// Pulls this writer's table from another server of this kind into this one.
    ///
    /// The schema travels with the rows, so the destination registers one it has
    /// never seen and every row keeps the reference to it - a migrated table is
    /// showable on the other side without anybody copying a schema by hand.
    /// What arrives replaces the local table.
    pub async fn migrate_from(
        &self,
        remote_url: &str,
        remote_name_space: &str,
        remote_table_name: &str,
    ) -> Result<i32, MyNoSqlWriterError> {
        Ok(self
            .connection
            .writer_client()
            .migrate_from(self.connection.unary(MigrateFromGrpcRequest {
                remote_url: remote_url.to_string(),
                remote_name_space: remote_name_space.to_string(),
                remote_table_name: remote_table_name.to_string(),
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                sync_period: self.sync_period as i32,
            }))
            .await?
            .into_inner()
            .rows_migrated)
    }

    /// Every namespace the server holds. Not scoped to this writer's own - a
    /// namespace is a thing of the server, not of one table.
    pub async fn get_namespaces(&self) -> Result<Vec<NamespaceGrpcModel>, MyNoSqlWriterError> {
        let mut stream = self
            .connection
            .writer_client()
            .get_namespaces(())
            .await?
            .into_inner();

        let mut result = Vec::new();

        while let Some(item) = stream.message().await? {
            result.push(item);
        }

        Ok(result)
    }

    /// Drops a whole namespace - every table it holds and its folder on disk.
    /// The default one is refused: it is where every write which names no
    /// namespace lands.
    pub async fn delete_namespace(&self, name_space: &str) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .delete_namespace(self.connection.unary(DeleteNamespaceGrpcRequest {
                name_space: name_space.to_string(),
            }))
            .await?;

        Ok(())
    }

    /// Moves this writer's table out of its namespace into another one, data,
    /// attributes and schemas included.
    pub async fn move_table_to_namespace(
        &self,
        destination: &str,
    ) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .move_table_to_namespace(self.connection.unary(MoveTableToNamespaceGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                destination_name_space: destination.to_string(),
                sync_period: self.sync_period as i32,
            }))
            .await?;

        Ok(())
    }

    /// Changes what the table does with itself. `Created` is not touched -
    /// setting a limit is not a new table.
    pub async fn set_table_attributes(
        &self,
        attributes: TableAttributesGrpcModel,
    ) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .set_table_attributes(self.connection.unary(SetTableAttributesGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                attributes: Some(attributes),
                sync_period: self.sync_period as i32,
            }))
            .await?;

        Ok(())
    }

    /// Keeps at most `max` partitions right now, dropping the ones nobody has
    /// read for the longest. The table's own `MaxPartitionsAmount` does this on
    /// a schedule; this is the same thing with a number of its own.
    pub async fn clean_and_keep_max_partitions(
        &self,
        max_partitions_amount: i32,
    ) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .clean_and_keep_max_partitions(self.connection.unary(
                CleanAndKeepMaxPartitionsGrpcRequest {
                    name_space: self.name_space.clone(),
                    table_name: TEntity::TABLE_NAME.to_string(),
                    max_partitions_amount,
                    sync_period: self.sync_period as i32,
                },
            ))
            .await?;

        Ok(())
    }

    /// The same for the rows of one partition.
    pub async fn clean_partition_and_keep_max_rows(
        &self,
        partition_key: &str,
        max_rows_amount: i32,
    ) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .clean_partition_and_keep_max_rows(self.connection.unary(
                CleanPartitionAndKeepMaxRowsGrpcRequest {
                    name_space: self.name_space.clone(),
                    table_name: TEntity::TABLE_NAME.to_string(),
                    partition_key: partition_key.to_string(),
                    max_rows_amount,
                    sync_period: self.sync_period as i32,
                },
            ))
            .await?;

        Ok(())
    }

    // ---- one row ---------------------------------------------------------

    /// Fails with `AlreadyExists` when the row is already stored.
    pub async fn insert(&self, entity: &TEntity) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .insert(self.connection.unary(self.write_row_request(entity)))
            .await?;

        Ok(())
    }

    pub async fn insert_or_replace(&self, entity: &TEntity) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .insert_or_replace(self.connection.unary(self.write_row_request(entity)))
            .await?;

        Ok(())
    }

    /// Keeps the stored row unless the entity is strictly newer, and answers
    /// whether it was taken. Declining is the normal outcome and not a failure.
    ///
    /// What is compared is the entity's own `TimeStamp`, whatever this writer
    /// was configured with - the same as the batch of the same name.
    pub async fn insert_or_replace_if_new(
        &self,
        entity: &TEntity,
    ) -> Result<bool, MyNoSqlWriterError> {
        let response = self
            .connection
            .writer_client()
            .insert_or_replace_if_new(self.connection.unary(self.write_row_request(entity)))
            .await?;

        Ok(response.into_inner().written)
    }

    /// Overwrites a stored row under an optimistic-concurrency check: the entity
    /// has to be one that was **read from the server**, because the `TimeStamp`
    /// it came back with is the version the check is made against.
    ///
    /// Fails with `NotFound` when there is nothing stored to replace, and with
    /// a conflict ([`MyNoSqlWriterError::is_conflict`]) when the stored row is
    /// not that version any more - somebody has written it in between. The
    /// answer to a conflict is to read the row again, apply the change to what
    /// came back and call this again; the loop ends because every write that
    /// lands leaves a version nobody else is holding.
    ///
    /// An entity built from nothing carries no `TimeStamp` and is refused
    /// outright - there is no version in it to check.
    pub async fn replace(&self, entity: &TEntity) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .replace(self.connection.unary(self.write_row_request(entity)))
            .await?;

        Ok(())
    }

    /// Answers whether there was a row under those keys. A key which is not
    /// there is not a failure - the same as inside [`Self::bulk_delete`], so a
    /// loop over keys does not stop on the first one that has already expired.
    pub async fn delete_row(
        &self,
        partition_key: &str,
        row_key: &str,
    ) -> Result<bool, MyNoSqlWriterError> {
        let response = self
            .connection
            .writer_client()
            .delete_row(self.connection.unary(DeleteRowGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                partition_key: partition_key.to_string(),
                row_key: row_key.to_string(),
                sync_period: self.sync_period as i32,
            }))
            .await?;

        Ok(response.into_inner().deleted)
    }

    // ---- reading ---------------------------------------------------------

    pub async fn get_row(
        &self,
        partition_key: &str,
        row_key: &str,
    ) -> Result<Option<TEntity>, MyNoSqlWriterError> {
        let response = self
            .connection
            .writer_client()
            .get_row(self.connection.unary(GetRowGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                partition_key: partition_key.to_string(),
                row_key: row_key.to_string(),
            }))
            .await?
            .into_inner();

        let Some(row) = response.row else {
            return Ok(None);
        };

        Ok(Some(TEntity::from_slice(&row)?))
    }

    /// Both keys absent means the whole table. The rows arrive as a stream of
    /// bounded chunks, so a table of any size fits.
    pub async fn get_rows(
        &self,
        partition_key: Option<&str>,
        row_key: Option<&str>,
    ) -> Result<Vec<TEntity>, MyNoSqlWriterError> {
        self.get_rows_with_limit(partition_key, row_key, None, None)
            .await
    }

    pub async fn get_rows_with_limit(
        &self,
        partition_key: Option<&str>,
        row_key: Option<&str>,
        skip: Option<i32>,
        limit: Option<i32>,
    ) -> Result<Vec<TEntity>, MyNoSqlWriterError> {
        let stream = self
            .connection
            .writer_client()
            .get_rows(GetRowsGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                partition_key: partition_key.map(|itm| itm.to_string()),
                row_key: row_key.map(|itm| itm.to_string()),
                skip,
                limit,
            })
            .await?
            .into_inner();

        collect(stream).await
    }

    /// Rows of one partition whose key is at or below the one asked for, the
    /// highest first - the shape a "what was in force at this moment" lookup
    /// has, when the row key is the moment.
    pub async fn get_highest_row_and_below(
        &self,
        partition_key: &str,
        row_key: &str,
        limit: Option<i32>,
    ) -> Result<Vec<TEntity>, MyNoSqlWriterError> {
        let stream = self
            .connection
            .writer_client()
            .get_highest_row_and_below(GetHighestRowAndBelowGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                partition_key: partition_key.to_string(),
                row_key: row_key.to_string(),
                limit,
            })
            .await?
            .into_inner();

        collect(stream).await
    }

    /// Several named rows of one partition, in one call instead of one each.
    pub async fn get_single_partition_multiple_rows(
        &self,
        partition_key: &str,
        row_keys: Vec<String>,
    ) -> Result<Vec<TEntity>, MyNoSqlWriterError> {
        let stream = self
            .connection
            .writer_client()
            .get_single_partition_multiple_rows(GetSinglePartitionMultipleRowsGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                partition_key: partition_key.to_string(),
                row_keys,
            })
            .await?
            .into_inner();

        collect(stream).await
    }

    /// This writer's table as the migration contract carries it: the rows
    /// grouped by the schema they were written under, in exactly the shape a
    /// write comes in as - so what one server hands out is what another takes
    /// in.
    ///
    /// The rows come back as bytes rather than as `TEntity` on purpose. A table
    /// written under several versions of the entity arrives as several chunks,
    /// and a chunk with no schema carries rows whose schema the source has
    /// lost - neither of those is this build's entity, and decoding them as one
    /// would be the migration quietly deciding they are.
    pub async fn get_rows_with_schema(
        &self,
    ) -> Result<Vec<DbRowsWithSchemaGrpcModel>, MyNoSqlWriterError> {
        let stream = self
            .connection
            .writer_client()
            .get_rows_with_schema(GetRowsWithSchemaGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
            })
            .await?
            .into_inner();

        collect_stream(stream).await
    }

    /// How much the table holds. All three counters in one answer, because each
    /// of them alone would be a round trip for a single number.
    pub async fn get_table_size(&self) -> Result<TableSizeGrpcResponse, MyNoSqlWriterError> {
        Ok(self
            .connection
            .writer_client()
            .get_table_size(self.connection.unary(GetTableSizeGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
            }))
            .await?
            .into_inner())
    }

    pub async fn get_tables(&self) -> Result<Vec<TableGrpcModel>, MyNoSqlWriterError> {
        let mut stream = self
            .connection
            .writer_client()
            .get_tables(GetTablesGrpcRequest {
                name_space: self.name_space.clone(),
            })
            .await?
            .into_inner();

        let mut result = Vec::new();

        while let Some(table) = stream.message().await? {
            result.push(table);
        }

        Ok(result)
    }

    // ---- batches ---------------------------------------------------------

    pub async fn bulk_insert_or_replace(
        &self,
        entities: &[TEntity],
    ) -> Result<(), MyNoSqlWriterError> {
        self.bulk_write(BulkWriteModeGrpcModel::BulkWriteInsertOrReplace, entities)
            .await
    }

    /// Keeps the stored row unless the entity is strictly newer, so the entity
    /// has to carry its own `TimeStamp` - the server uses it whatever this
    /// writer was configured with.
    pub async fn bulk_insert_or_replace_if_new(
        &self,
        entities: &[TEntity],
    ) -> Result<(), MyNoSqlWriterError> {
        self.bulk_write(
            BulkWriteModeGrpcModel::BulkWriteInsertOrReplaceIfNew,
            entities,
        )
        .await
    }

    /// The partitions the batch names end up holding the batch and nothing else.
    /// Partitions it does not name are not touched.
    pub async fn clean_partitions_and_insert(
        &self,
        entities: &[TEntity],
    ) -> Result<(), MyNoSqlWriterError> {
        self.bulk_write(
            BulkWriteModeGrpcModel::BulkWriteCleanPartitionsAndInsert,
            entities,
        )
        .await
    }

    /// The whole table is emptied first - an empty batch included, which is the
    /// same thing as [`Self::clean_table`].
    pub async fn clean_table_and_insert(
        &self,
        entities: &[TEntity],
    ) -> Result<(), MyNoSqlWriterError> {
        self.bulk_write(
            BulkWriteModeGrpcModel::BulkWriteCleanTableAndInsert,
            entities,
        )
        .await
    }

    /// Deletes rows named by key, across as many partitions as it takes, in one
    /// call and one entry into the table. Answers how many of them were there.
    pub async fn bulk_delete(
        &self,
        partitions: Vec<PartitionRowKeysGrpcModel>,
    ) -> Result<usize, MyNoSqlWriterError> {
        let response = self
            .connection
            .writer_client()
            .bulk_delete(self.connection.unary(BulkDeleteGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                partitions,
                sync_period: self.sync_period as i32,
            }))
            .await?;

        Ok(response.into_inner().rows_deleted as usize)
    }

    /// The batch is cut into messages, but it is still one operation: the server
    /// accumulates the whole stream and applies it in one entry into the table.
    pub async fn bulk_write(
        &self,
        mode: BulkWriteModeGrpcModel,
        entities: &[TEntity],
    ) -> Result<(), MyNoSqlWriterError> {
        let messages: Vec<BulkWriteGrpcRequest> = self
            .cut_into_messages(entities)
            .into_iter()
            .map(|rows| BulkWriteGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                mode: mode as i32,
                sync_period: self.sync_period as i32,
                use_client_time_stamp: self.use_client_time_stamp,
                schema: Some(self.schema.clone()),
                rows,
            })
            .collect();

        self.connection
            .writer_client()
            .bulk_write(tokio_stream::iter(messages))
            .await?;

        Ok(())
    }

    /// Opens a transaction against this writer's table: writes of several kinds
    /// applied together, in the order they were built.
    pub async fn begin_transaction(
        &self,
    ) -> Result<MyNoSqlGrpcTransaction<TEntity>, MyNoSqlWriterError> {
        let transaction_id = self
            .connection
            .writer_client()
            .start_transaction(self.connection.unary(StartTransactionGrpcRequest {
                name_space: self.name_space.clone(),
                table_name: TEntity::TABLE_NAME.to_string(),
                sync_period: self.sync_period as i32,
            }))
            .await?
            .into_inner()
            .transaction_id;

        Ok(MyNoSqlGrpcTransaction::new(
            self.connection.clone(),
            transaction_id,
            self.schema.clone(),
            self.use_client_time_stamp,
        ))
    }

    // ---- putting the requests together -----------------------------------

    fn create_table_request(&self, attributes: TableAttributesGrpcModel) -> CreateTableGrpcRequest {
        CreateTableGrpcRequest {
            name_space: self.name_space.clone(),
            table_name: TEntity::TABLE_NAME.to_string(),
            attributes: Some(attributes),
            sync_period: self.sync_period as i32,
        }
    }

    fn write_row_request(&self, entity: &TEntity) -> WriteRowGrpcRequest {
        WriteRowGrpcRequest {
            name_space: self.name_space.clone(),
            table_name: TEntity::TABLE_NAME.to_string(),
            schema: Some(self.schema.clone()),
            row: entity.to_vec(),
            sync_period: self.sync_period as i32,
            use_client_time_stamp: self.use_client_time_stamp,
        }
    }

    /// Serializes the batch and cuts it into messages the transport can carry.
    /// Which message a row lands in changes nothing: the server puts the whole
    /// stream back together before it touches the table.
    ///
    /// Always at least one message, because the stream is what names the table
    /// and the mode - and `CleanTableAndInsert` with no rows is an operation.
    fn cut_into_messages(&self, entities: &[TEntity]) -> Vec<Vec<Vec<u8>>> {
        crate::cut_rows_at_least_once(entities.iter().map(|entity| entity.to_vec()))
    }
}
