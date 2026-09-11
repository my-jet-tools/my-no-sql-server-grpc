use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_core::Stream;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use tonic::{Request, Response, Status};

use crate::app::{AppContext, DbNamespaces};
use crate::data_sync_period::DataSyncPeriod;
use crate::my_no_sql_reader_grpc::reader_server::Reader;
use crate::my_no_sql_reader_grpc::*;
use crate::reader::{ReaderSession, SyncChunk, split_rows};

/// How long `GetChange` holds before answering with nothing. The client's own
/// deadline has to be comfortably larger, or a normal empty answer would look
/// like a failed call and send the reader through a full re-subscribe.
const LONG_POLL: Duration = Duration::from_secs(5);

type GrpcStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

pub struct ReaderGrpcService {
    pub app: Arc<AppContext>,
}

impl ReaderGrpcService {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }

    /// Every failure a reader can get means the same thing to it - start over -
    /// so there is nothing to gain from separate codes here.
    fn get_session(&self, session_id: &str) -> Result<Arc<ReaderSession>, Status> {
        let Some(session) = self.app.reader_sessions.get(session_id) else {
            return Err(Status::not_found(format!(
                "Session '{session_id}' is unknown"
            )));
        };

        // A session whose backlog overflowed is forgotten here rather than by a
        // timer: it is the reader itself which comes back for it, and the answer
        // it gets is the one that sends it through the full re-subscribe the
        // failure model prescribes. Nothing else would ever free it - `touch`
        // below is on every call, so the TTL never reaches it.
        if session.has_overflowed() {
            self.app.reader_sessions.remove(session_id);

            println!(
                "Reader '{}' v{} fell too far behind in namespace '{}'. \
                 Session {} is dropped and it will start over",
                session.app_name, session.version, session.namespace, session.id
            );

            return Err(Status::not_found(format!(
                "Session '{session_id}' fell too far behind and was dropped"
            )));
        }

        session.touch();

        Ok(session)
    }
}

#[tonic::async_trait]
impl Reader for ReaderGrpcService {
    type SubscribeStream = GrpcStream<DbRowsChunkGrpcModel>;

    async fn ping(&self, _: Request<()>) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn greeting(
        &self,
        request: Request<GreetingGrpcRequest>,
    ) -> Result<Response<GreetingGrpcResponse>, Status> {
        if !self.app.states.is_initialized() {
            return Err(crate::db_operations::DbOperationError::NotInitialized.into());
        }

        // Taken before the request is consumed - `into_inner` drops the
        // extensions the address lives in, and there is no second chance at it.
        let ip = match request.remote_addr() {
            Some(addr) => addr.to_string(),
            // Not decorative: an incoming which is not a socket has no address,
            // and the monitoring views must not show an empty string as one.
            None => "unknown".to_string(),
        };

        let request = request.into_inner();

        let session = self.app.reader_sessions.create(
            request.app_name,
            request.version,
            DbNamespaces::resolve_name(&request.name_space).to_string(),
            ip,
        );

        println!(
            "Reader '{}' v{} greeted in namespace '{}' as session {}",
            session.app_name, session.version, session.namespace, session.id
        );

        Ok(Response::new(GreetingGrpcResponse {
            session_id: session.id.clone(),
        }))
    }

    async fn subscribe(
        &self,
        request: Request<SubscribeGrpcRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let request = request.into_inner();
        let session = self.get_session(&request.session_id)?;

        // Created rather than resolved, and the table below is allowed not to
        // exist, for the same reason: a reader deployed before its writer is the
        // ordinary cold start, not a failure. Refusing here wedges the reader
        // whole - it starts the session over and re-subscribes to everything, so
        // one table nobody has written yet stops every table behind it from ever
        // being subscribed. The JSON version answers a cold start the same way,
        // with an empty picture rather than an error.
        let db_namespace = self
            .app
            .namespaces
            .get_or_create(&session.namespace, &self.app.settings)
            .await?;

        let db_table = match db_namespace.tables.get_table(&request.table_name) {
            Some(db_table) => db_table,
            None => {
                // The subscription is registered all the same, so the first
                // write into the table reaches this session through the queue.
                // That is the rule a subscription already lives by at the other
                // end of a table's life - a `DeleteTable` keeps it too.
                session.subscribe(&request.table_name);

                // ...and a writer could have created the table between the
                // lookup and the registration, in which case its rows are
                // already there and nothing is going to announce them again.
                match db_namespace.tables.get_table(&request.table_name) {
                    Some(db_table) => db_table,
                    None => return Ok(Response::new(Box::pin(tokio_stream::iter(Vec::new())))),
                }
            }
        };

        // Registration and the snapshot happen under one lock: a write that lands
        // after the snapshot also lands after the registration, so it reaches this
        // session through the queue instead of being lost while we stream.
        let db_rows = db_table.register_and_snapshot(|| session.subscribe(&request.table_name));

        let chunks: Vec<Result<DbRowsChunkGrpcModel, Status>> = split_rows(db_rows)
            .into_iter()
            .map(|chunk| {
                Ok(DbRowsChunkGrpcModel {
                    rows: chunk.iter().map(|db_row| db_row.to_vec()).collect(),
                })
            })
            .collect();

        Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
    }

    async fn get_change(
        &self,
        request: Request<GetChangeGrpcRequest>,
    ) -> Result<Response<GetChangeGrpcResponse>, Status> {
        let request = request.into_inner();
        let session = self.get_session(&request.session_id)?;

        self.apply_read_statistics(&session, &request.read_statistics);

        let chunk = session.get_next_chunk(LONG_POLL).await;

        Ok(Response::new(to_response(chunk)))
    }
}

impl ReaderGrpcService {
    /// What the reader says it read.
    ///
    /// The last-read marks are a hint - a batch lost together with a failed call
    /// costs nothing. A new expiry is not: it is what keeps a row alive, and a
    /// row's expiry is part of the stored row, so that one reaches the disk.
    /// Not urgently, though - a reader refreshing a TTL is the most repetitive
    /// write there is, and a later moment lets a run of them collapse into one.
    fn apply_read_statistics(
        &self,
        session: &ReaderSession,
        statistics: &[PartitionReadStatisticsGrpcModel],
    ) {
        if statistics.is_empty() {
            return;
        }

        let Some(db_namespace) = self.app.namespaces.get(&session.namespace) else {
            return;
        };

        let now = DateTimeAsMicroseconds::now();
        let persist_moment = DataSyncPeriod::Sec15.get_sync_moment(now);

        for item in statistics {
            let Some(db_table) = db_namespace.tables.get_table(&item.table_name) else {
                continue;
            };

            let update = to_update_read_statistics(item);

            if update.is_empty() {
                continue;
            }

            db_table.apply_read_statistics(&item.partition_key, &item.row_keys, &update, now);

            if update.touches_the_disk() {
                crate::db_operations::write::mark_partition_to_persist(
                    &db_namespace,
                    &db_table,
                    &item.partition_key,
                    persist_moment,
                );
            }
        }
    }
}

/// A flag and a value rather than one number, because "leave it alone" and "set
/// it to never" are opposite instructions and `0` can only mean one of them.
fn to_update_read_statistics(
    src: &PartitionReadStatisticsGrpcModel,
) -> my_no_sql_grpc_core::db::UpdateReadStatistics {
    my_no_sql_grpc_core::db::UpdateReadStatistics {
        update_partition_last_read: src.update_partition_last_read_time,
        update_rows_last_read: src.update_rows_last_read_time,
        set_partition_expires: to_expires(src.update_partition_expires, src.partition_expires),
        set_rows_expires: to_expires(src.update_rows_expires, src.rows_expires),
    }
}

fn to_expires(update: bool, value: i64) -> Option<Option<DateTimeAsMicroseconds>> {
    if !update {
        return None;
    }

    if value == 0 {
        return Some(None);
    }

    Some(Some(DateTimeAsMicroseconds::new(value)))
}

/// Exactly one field of the answer is set - or none of them, which is what the
/// reader reads as "nothing to deliver" and uses as the ping.
fn to_response(chunk: Option<SyncChunk>) -> GetChangeGrpcResponse {
    let mut result = GetChangeGrpcResponse::default();

    let Some(chunk) = chunk else {
        return result;
    };

    match chunk {
        SyncChunk::CleanTable { table_name } => {
            result.clean_table = Some(CleanTableGrpcModel { table_name });
        }
        SyncChunk::UpdateTableAttributes {
            table_name,
            attributes,
        } => {
            result.update_table_attributes = Some(UpdateTableAttributesGrpcModel {
                table_name,
                attributes: Some(TableAttributesGrpcModel {
                    persist: attributes.persist,
                    max_partitions_amount: attributes.max_partitions_amount.map(|itm| itm as i32),
                    max_rows_per_partition_amount: attributes
                        .max_rows_per_partition_amount
                        .map(|itm| itm as i32),
                }),
            });
        }
        SyncChunk::DeleteTable { table_name } => {
            result.delete_table = Some(DeleteTableGrpcModel { table_name });
        }
        SyncChunk::CleanPartitions {
            table_name,
            partition_keys,
        } => {
            result.clean_partitions = Some(CleanPartitionsGrpcModel {
                table_name,
                partition_keys,
            });
        }
        SyncChunk::InitPartitions {
            table_name,
            partitions,
        } => {
            result.init_partitions = Some(InitPartitionsGrpcModel {
                table_name,
                partitions: partitions
                    .into_iter()
                    .map(|itm| PartitionRowsGrpcModel {
                        partition_key: itm.partition_key,
                        rows: itm.rows.iter().map(|db_row| db_row.to_vec()).collect(),
                    })
                    .collect(),
            });
        }
        SyncChunk::InitPartitionsEnd { table_name } => {
            result.init_partitions_end = Some(InitPartitionsEndGrpcModel { table_name });
        }
        SyncChunk::UpdateRows { table_name, rows } => {
            result.update_rows = Some(UpdateRowsGrpcModel {
                table_name,
                rows: rows.iter().map(|db_row| db_row.to_vec()).collect(),
            });
        }
        SyncChunk::UpdateRowsEnd { table_name } => {
            result.update_rows_end = Some(UpdateRowsEndGrpcModel { table_name });
        }
        SyncChunk::DeleteRows {
            table_name,
            partitions,
        } => {
            result.delete_rows = Some(DeleteRowsGrpcModel {
                table_name,
                partitions: partitions
                    .into_iter()
                    .map(|itm| PartitionRowKeysGrpcModel {
                        partition_key: itm.partition_key,
                        row_keys: itm.row_keys,
                    })
                    .collect(),
            });
        }
        SyncChunk::DeleteRowsEnd { table_name } => {
            result.delete_rows_end = Some(DeleteRowsEndGrpcModel { table_name });
        }
    }

    result
}
