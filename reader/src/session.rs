use std::sync::Arc;
use std::time::Duration;

use ahash::{AHashMap, AHashSet};
use my_no_sql_grpc_core::db::{DbRow, PartitionRowKeys};
use my_no_sql_grpc_core::rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::my_no_sql_reader_grpc::*;
use crate::reader::ReaderInner;
use crate::{MyNoSqlReaderError, to_db_row};

/// How long to wait before greeting again after a session ended. Long enough not
/// to hammer a server which is still loading from disk, short enough that a
/// restart is invisible to whoever is reading the cache.
const RETRY_DELAY: Duration = Duration::from_secs(1);

pub(crate) async fn read_loop(inner: Arc<ReaderInner>) {
    while !inner.is_stopped() {
        if let Err(err) = run_session(&inner).await {
            println!("MyNoSql reader session is over: {err}. Greeting again.");
        }

        tokio::time::sleep(RETRY_DELAY).await;
    }
}

/// One session, from `Greeting` until something goes wrong.
///
/// Everything accumulated for an unfinished batch lives in this function, so a
/// session which ends half way through one drops it by simply returning: an
/// unfinished batch is never applied, and the fresh snapshot makes it moot.
async fn run_session(inner: &Arc<ReaderInner>) -> Result<(), MyNoSqlReaderError> {
    let mut client = inner.reader_client();

    let session_id = client
        .greeting(GreetingGrpcRequest {
            app_name: inner.app_name.clone(),
            version: inner.version.clone(),
            name_space: inner.name_space.clone(),
        })
        .await?
        .into_inner()
        .session_id;

    let mut subscribed: AHashSet<String> = AHashSet::new();
    let mut pending = PendingBatches::default();

    while !inner.is_stopped() {
        // Tables registered after this session started are picked up here, so
        // subscribing does not have to wait for a reconnect.
        for table in inner.get_tables() {
            if subscribed.contains(&table.table_name) {
                continue;
            }

            match subscribe(&mut client, &session_id, &table.table_name).await {
                Ok(rows) => {
                    table.apply_snapshot(rows);
                    subscribed.insert(table.table_name.clone());
                }

                // Defence in depth for a server which still refuses a table it
                // does not have: ending the session over it would take every
                // other table down with it, because the session starts over and
                // the tables behind this one never get their turn. So the table
                // is left unsubscribed - the next turn asks for it again - and
                // whoever is waiting for it is let go with an empty cache,
                // which is the truth: the table is not there.
                Err(MyNoSqlReaderError::Grpc(status)) if status.code() == tonic::Code::NotFound => {
                    // Only when nothing has ever arrived. A `not found` on a
                    // cache which already has rows is the session being gone,
                    // not the table, and answering "no rows" to that is the one
                    // thing this reader must never do.
                    if !table.is_initialized() {
                        table.apply_snapshot(Vec::new());
                    }
                }

                Err(err) => return Err(err),
            }
        }

        let change = client
            .get_change(GetChangeGrpcRequest {
                session_id: session_id.clone(),
                read_statistics: take_read_statistics(inner),
            })
            .await?
            .into_inner();

        apply(inner, &mut pending, change)?;
    }

    Ok(())
}

/// Everything the application said it read since the last call, riding along
/// with a call the reader was going to make anyway.
fn take_read_statistics(inner: &Arc<ReaderInner>) -> Vec<PartitionReadStatisticsGrpcModel> {
    let mut result = Vec::new();

    for table in inner.get_tables() {
        for statistics in table.take_statistics() {
            let (update_partition_expires, partition_expires) =
                from_expires(statistics.set_partition_expires);
            let (update_rows_expires, rows_expires) = from_expires(statistics.set_rows_expires);

            result.push(PartitionReadStatisticsGrpcModel {
                table_name: table.table_name.clone(),
                partition_key: statistics.partition_key,
                update_partition_last_read_time: statistics.update_partition_last_read,
                row_keys: statistics.row_keys,
                update_rows_last_read_time: statistics.update_rows_last_read,
                update_partition_expires,
                partition_expires,
                update_rows_expires,
                rows_expires,
            });
        }
    }

    result
}

/// A flag and a value, because `0` can only mean one of "leave it alone" and
/// "set it to never", and they are opposite instructions.
fn from_expires(src: Option<Option<DateTimeAsMicroseconds>>) -> (bool, i64) {
    match src {
        None => (false, 0),
        Some(None) => (true, 0),
        Some(Some(expires)) => (true, expires.unix_microseconds),
    }
}

/// The full image of a table, streamed back in bounded chunks.
async fn subscribe(
    client: &mut reader_client::ReaderClient<tonic::transport::Channel>,
    session_id: &str,
    table_name: &str,
) -> Result<Vec<Arc<DbRow>>, MyNoSqlReaderError> {
    let mut stream = client
        .subscribe(SubscribeGrpcRequest {
            session_id: session_id.to_string(),
            table_name: table_name.to_string(),
        })
        .await?
        .into_inner();

    let mut result = Vec::new();

    while let Some(chunk) = stream.message().await? {
        for row in chunk.rows {
            result.push(to_db_row(&row)?);
        }
    }

    Ok(result)
}

/// What a batch has collected so far, per table.
///
/// A batch is cut into several chunks and closed by its `End`, so it is applied
/// in one go when the `End` arrives and never half way. The server pushes all
/// the chunks of one operation into the queue under a single lock, so nobody
/// else's chunks can appear in the middle of one.
#[derive(Default)]
struct PendingBatches {
    update_rows: AHashMap<String, Vec<Arc<DbRow>>>,
    init_partitions: AHashMap<String, InitPartitionsBatch>,
    delete_rows: AHashMap<String, Vec<PartitionRowKeys>>,
}

#[derive(Default)]
struct InitPartitionsBatch {
    rows: Vec<Arc<DbRow>>,
    /// Every partition the batch named. A partition which was named but brought
    /// no rows is one the batch empties, and only this list says so - the rows
    /// alone could not.
    partition_keys: Vec<String>,
}

fn apply(
    inner: &Arc<ReaderInner>,
    pending: &mut PendingBatches,
    change: GetChangeGrpcResponse,
) -> Result<(), MyNoSqlReaderError> {
    if let Some(model) = change.clean_table
        && let Some(table) = inner.get_table(&model.table_name)
    {
        table.clean();
    }

    if let Some(model) = change.update_table_attributes
        && let Some(table) = inner.get_table(&model.table_name)
        && let Some(attributes) = model.attributes
    {
        table.set_attributes(my_no_sql_grpc_core::db::DbTableAttributes {
            persist: attributes.persist,
            max_partitions_amount: to_limit(attributes.max_partitions_amount),
            max_rows_per_partition_amount: to_limit(attributes.max_rows_per_partition_amount),
            created: table.get_attributes().created,
            // The event carries what the table is set to, not what its rows were
            // written under: a schema exists to show a row as JSON, which is the
            // server's job and no part of what a reader caches.
            schemas: Default::default(),
        });
    }

    // The table is gone rather than emptied. The cache entry stays: it is what
    // the handles an application is holding point at, and a table created under
    // the same name again has to fill the same one.
    if let Some(model) = change.delete_table
        && let Some(table) = inner.get_table(&model.table_name)
    {
        table.clean();
    }

    if let Some(model) = change.clean_partitions
        && let Some(table) = inner.get_table(&model.table_name)
    {
        table.delete_partitions(&model.partition_keys);
    }

    if let Some(model) = change.init_partitions {
        let batch = pending.init_partitions.entry(model.table_name).or_default();

        for partition in model.partitions {
            batch.partition_keys.push(partition.partition_key);

            for row in partition.rows {
                batch.rows.push(to_db_row(&row)?);
            }
        }
    }

    if let Some(model) = change.init_partitions_end
        && let Some(batch) = pending.init_partitions.remove(&model.table_name)
        && let Some(table) = inner.get_table(&model.table_name)
    {
        let emptied = partitions_without_rows(&batch);

        table.apply_init_partitions(batch.rows);

        if !emptied.is_empty() {
            table.delete_partitions(&emptied);
        }
    }

    if let Some(model) = change.update_rows {
        let batch = pending.update_rows.entry(model.table_name).or_default();

        for row in model.rows {
            batch.push(to_db_row(&row)?);
        }
    }

    if let Some(model) = change.update_rows_end
        && let Some(rows) = pending.update_rows.remove(&model.table_name)
        && let Some(table) = inner.get_table(&model.table_name)
    {
        table.apply_updated_rows(rows);
    }

    if let Some(model) = change.delete_rows {
        let batch = pending.delete_rows.entry(model.table_name).or_default();

        for partition in model.partitions {
            batch.push(PartitionRowKeys {
                partition_key: partition.partition_key,
                row_keys: partition.row_keys,
            });
        }
    }

    if let Some(model) = change.delete_rows_end
        && let Some(partitions) = pending.delete_rows.remove(&model.table_name)
        && let Some(table) = inner.get_table(&model.table_name)
    {
        table.apply_deleted_rows(partitions);
    }

    Ok(())
}

/// `0` is what a client which can not leave the field out sends for "no limit",
/// and it is read as one rather than as "no partitions".
fn to_limit(src: Option<i32>) -> Option<usize> {
    let value = src?;

    if value <= 0 {
        return None;
    }

    Some(value as usize)
}

/// Partitions the batch named and then put nothing into: emptying them is what
/// it asked for, and applying the rows alone would leave them as they were.
fn partitions_without_rows(batch: &InitPartitionsBatch) -> Vec<String> {
    let with_rows: AHashSet<&str> = batch
        .rows
        .iter()
        .map(|db_row| db_row.get_partition_key())
        .collect();

    let mut result: Vec<String> = batch
        .partition_keys
        .iter()
        .filter(|partition_key| !with_rows.contains(partition_key.as_str()))
        .cloned()
        .collect();

    result.sort_unstable();
    result.dedup();

    result
}
