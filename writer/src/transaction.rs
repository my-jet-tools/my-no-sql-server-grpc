use std::marker::PhantomData;

use crate::my_no_sql_writer_grpc::*;
use my_no_sql_grpc_core::MyNoSqlEntity;

use crate::{MyNoSqlGrpcConnection, MyNoSqlWriterError};

/// A transaction being built against one table.
///
/// The actions are collected here and posted in one stream, so an ordinary
/// transaction is three round trips whatever it contains: start, post, commit.
/// [`Self::post`] exists for the case where the transaction is too big to hold
/// in memory - posting early changes nothing about when it lands.
///
/// Dropping the handle without committing does not cancel anything by itself.
/// Nothing was applied either, and the server forgets a transaction which went
/// quiet - but a client which knows it is giving up should say so with
/// [`Self::cancel`] rather than leave it to the timeout.
pub struct MyNoSqlGrpcTransaction<TEntity: MyNoSqlEntity> {
    connection: MyNoSqlGrpcConnection,
    id: String,
    schema: EntitySchemaGrpcModel,
    use_client_time_stamp: bool,
    actions: Vec<TransactionActionGrpcModel>,
    itm: PhantomData<TEntity>,
}

impl<TEntity: MyNoSqlEntity> MyNoSqlGrpcTransaction<TEntity> {
    pub(crate) fn new(
        connection: MyNoSqlGrpcConnection,
        id: String,
        schema: EntitySchemaGrpcModel,
        use_client_time_stamp: bool,
    ) -> Self {
        Self {
            connection,
            id,
            schema,
            use_client_time_stamp,
            actions: Vec::new(),
            itm: PhantomData,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Empties the table. The actions after it act on what is left, which is
    /// nothing - so this is what a "replace the whole table" transaction opens
    /// with.
    pub fn clean_table(&mut self) -> &mut Self {
        self.push(TransactionActionGrpcModel {
            clean_table: Some(TransactionCleanTableGrpcModel {}),
            ..self.empty_action()
        })
    }

    pub fn delete_partitions(&mut self, partition_keys: Vec<String>) -> &mut Self {
        self.push(TransactionActionGrpcModel {
            delete_partitions: Some(TransactionDeletePartitionsGrpcModel { partition_keys }),
            ..self.empty_action()
        })
    }

    pub fn delete_rows(&mut self, partition_key: &str, row_keys: Vec<String>) -> &mut Self {
        self.push(TransactionActionGrpcModel {
            delete_rows: Some(TransactionDeleteRowsGrpcModel {
                partition_key: partition_key.to_string(),
                row_keys,
            }),
            ..self.empty_action()
        })
    }

    /// The rows are cut into as many actions as the transport needs; they still
    /// land in the order they were added, next to each other.
    pub fn insert_or_replace(&mut self, entities: &[TEntity]) -> &mut Self {
        for rows in crate::cut_rows(entities.iter().map(|entity| entity.to_vec())) {
            self.push(TransactionActionGrpcModel {
                insert_or_replace: Some(TransactionInsertOrReplaceGrpcModel {
                    schema: Some(self.schema.clone()),
                    rows,
                    use_client_time_stamp: self.use_client_time_stamp,
                }),
                ..self.empty_action()
            });
        }

        self
    }

    /// Hands everything collected so far to the server. Nothing is applied by
    /// it - a post which fails leaves the transaction as it was, so it can
    /// simply be repeated.
    pub async fn post(&mut self) -> Result<(), MyNoSqlWriterError> {
        if self.actions.is_empty() {
            return Ok(());
        }

        let actions = std::mem::take(&mut self.actions);

        if let Err(err) = self
            .connection
            .writer_client()
            .post_transaction_actions(tokio_stream::iter(actions.clone()))
            .await
        {
            // Put them back: the server took none of them, so the caller still
            // holds the whole transaction and can post it again.
            self.actions = actions;
            return Err(err.into());
        }

        Ok(())
    }

    /// Posts whatever is left and applies the whole transaction in one entry
    /// into the table.
    ///
    /// It borrows rather than consumes, so a commit which fails leaves the
    /// handle - and with it the id - in the caller's hands. Taking `self` meant
    /// a failed commit dropped the only copy of an id the server still holds
    /// open, and there is no way back to it: cancelling needs the handle. That
    /// contradicted the thing the cancel contract is built on - that the client
    /// can always cancel in its `finally` - on the one path where cancelling is
    /// what a client wants to do.
    ///
    /// Committing twice is not refused here: the server forgets the transaction
    /// as it applies it, so the second call is answered `not_found`.
    pub async fn commit(&mut self) -> Result<(), MyNoSqlWriterError> {
        self.post().await?;

        self.connection
            .writer_client()
            .commit_transaction(self.connection.unary(TransactionGrpcRequest {
                transaction_id: self.id.clone(),
            }))
            .await?;

        Ok(())
    }

    /// Throws the transaction away. It does not mind a transaction the server
    /// has already forgotten, so it is safe in a cleanup path - including after
    /// a [`Self::commit`] which failed.
    pub async fn cancel(self) -> Result<(), MyNoSqlWriterError> {
        self.connection
            .writer_client()
            .cancel_transaction(self.connection.unary(TransactionGrpcRequest {
                transaction_id: self.id,
            }))
            .await?;

        Ok(())
    }

    fn empty_action(&self) -> TransactionActionGrpcModel {
        TransactionActionGrpcModel {
            transaction_id: self.id.clone(),
            clean_table: None,
            delete_partitions: None,
            delete_rows: None,
            insert_or_replace: None,
        }
    }

    fn push(&mut self, action: TransactionActionGrpcModel) -> &mut Self {
        self.actions.push(action);
        self
    }
}
