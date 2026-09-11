use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ahash::AHashMap;
use arc_swap::ArcSwap;
use my_no_sql_grpc_core::MyNoSqlEntity;
use parking_lot::Mutex;
use tonic::transport::{Channel, Endpoint};

use crate::my_no_sql_reader_grpc::reader_client::ReaderClient;
use crate::{MyNoSqlReaderError, ReadStatistics, TableCache};

/// The client's own deadline has to be comfortably larger than the server's
/// 5-second long poll, or an ordinary empty answer would look like a failed call
/// and send this reader through a full re-subscribe every time.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct ReaderInner {
    pub channel: Channel,
    pub app_name: String,
    pub version: String,
    pub name_space: String,
    tables: ArcSwap<AHashMap<String, Arc<TableCache>>>,
    write_lock: Mutex<()>,
    stopped: AtomicBool,
}

impl ReaderInner {
    pub fn get_tables(&self) -> Vec<Arc<TableCache>> {
        self.tables.load().values().cloned().collect()
    }

    pub fn get_table(&self, table_name: &str) -> Option<Arc<TableCache>> {
        self.tables.load().get(table_name).cloned()
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    fn get_or_create(&self, table_name: &str) -> Arc<TableCache> {
        let _guard = self.write_lock.lock();

        if let Some(table) = self.get_table(table_name) {
            return table;
        }

        let table = Arc::new(TableCache::new(table_name.to_string()));

        let mut map = self.tables.load().as_ref().clone();
        map.insert(table_name.to_string(), table.clone());
        self.tables.store(Arc::new(map));

        table
    }

    pub fn reader_client(&self) -> ReaderClient<Channel> {
        ReaderClient::new(self.channel.clone())
    }
}

/// Keeps an in-process image of the tables it was told to subscribe to.
///
/// The whole image of a table arrives exactly one way - streamed back from
/// `Subscribe`. Everything after that is instructions delivered by `GetChange`,
/// which long-polls. Any failed call, or a session the server has forgotten,
/// means the same thing here as a dropped TCP connection used to: greet again
/// and re-subscribe to everything. That is why the contract has no packet ids
/// and no acknowledgements, and why this loop does not try to tell one failure
/// from another.
///
/// While it is re-subscribing the cache keeps answering with what it already
/// has. A reader which served stale rows for a second is doing its job; one
/// which suddenly answers "no rows" is not.
pub struct MyNoSqlGrpcReader {
    inner: Arc<ReaderInner>,
}

impl MyNoSqlGrpcReader {
    pub fn new(
        url: impl Into<String>,
        app_name: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, MyNoSqlReaderError> {
        let url = url.into();

        let Ok(endpoint) = Endpoint::from_shared(url.clone()) else {
            return Err(MyNoSqlReaderError::InvalidUrl(url));
        };

        Ok(Self {
            inner: Arc::new(ReaderInner {
                channel: endpoint.timeout(DEFAULT_TIMEOUT).connect_lazy(),
                app_name: app_name.into(),
                version: version.into(),
                name_space: String::new(),
                tables: ArcSwap::from_pointee(AHashMap::new()),
                write_lock: Mutex::new(()),
                stopped: AtomicBool::new(false),
            }),
        })
    }

    /// An empty name is the default namespace, which is what a client that does
    /// not care about namespaces leaves it as.
    pub fn with_name_space(mut self, name_space: impl Into<String>) -> Self {
        let inner = Arc::get_mut(&mut self.inner)
            .expect("the namespace has to be set before the reader is started");

        inner.name_space = name_space.into();
        self
    }

    /// Registers a table and hands back a typed view of its cache. Subscribing
    /// after [`Self::start`] is allowed - the running session picks the table up
    /// on its next turn.
    pub fn subscribe<TEntity: MyNoSqlEntity>(&self) -> MyNoSqlGrpcReaderTable<TEntity> {
        MyNoSqlGrpcReaderTable {
            cache: self.inner.get_or_create(TEntity::TABLE_NAME),
            itm: PhantomData,
        }
    }

    pub fn start(&self) {
        tokio::spawn(crate::read_loop(self.inner.clone()));
    }

    /// Ends the loop after the call it is inside of finishes. The caches keep
    /// whatever they held - they are simply not updated any more.
    pub fn stop(&self) {
        self.inner.stopped.store(true, Ordering::Relaxed);
    }
}

/// A typed view of one table's cache. Cloning it is cheap and gives another
/// handle on the same cache.
pub struct MyNoSqlGrpcReaderTable<TEntity: MyNoSqlEntity> {
    cache: Arc<TableCache>,
    itm: PhantomData<TEntity>,
}

impl<TEntity: MyNoSqlEntity> Clone for MyNoSqlGrpcReaderTable<TEntity> {
    fn clone(&self) -> Self {
        Self {
            cache: self.cache.clone(),
            itm: PhantomData,
        }
    }
}

impl<TEntity: MyNoSqlEntity> MyNoSqlGrpcReaderTable<TEntity> {
    /// `false` until the first snapshot has landed. Reading before that answers
    /// "no rows", which is indistinguishable from an empty table - so anything
    /// which would act on emptiness has to wait first.
    pub fn is_initialized(&self) -> bool {
        self.cache.is_initialized()
    }

    pub async fn wait_until_initialized(&self) {
        self.cache.wait_until_initialized().await;
    }

    pub fn get_row(
        &self,
        partition_key: &str,
        row_key: &str,
    ) -> Result<Option<TEntity>, MyNoSqlReaderError> {
        let Some(db_row) = self.cache.get_row(partition_key, row_key) else {
            return Ok(None);
        };

        Ok(Some(decode(&db_row.to_vec())?))
    }

    pub fn get_by_partition_key(
        &self,
        partition_key: &str,
    ) -> Result<Vec<TEntity>, MyNoSqlReaderError> {
        decode_all(self.cache.get_by_partition_key(partition_key))
    }

    pub fn get_all(&self) -> Result<Vec<TEntity>, MyNoSqlReaderError> {
        decode_all(self.cache.get_all())
    }

    pub fn get_partition_keys(&self) -> Vec<String> {
        self.cache.get_partition_keys()
    }

    pub fn get_rows_amount(&self) -> usize {
        self.cache.get_rows_amount()
    }

    /// What the server last said the table does with itself. Until the first
    /// `UpdateTableAttributes` arrives these are the defaults - the snapshot
    /// carries rows, not attributes.
    pub fn get_attributes(&self) -> Arc<my_no_sql_grpc_core::db::DbTableAttributes> {
        self.cache.get_attributes()
    }

    /// Tells the server what this reader actually read. It rides along with the
    /// next `GetChange`, so it costs no round trip of its own.
    ///
    /// Two things at once, and they are two halves of one idea: the last-read
    /// marks keep a partition everybody reads but nobody rewrites from being
    /// evicted, and the expiry makes the reading itself what keeps a row alive.
    pub fn report_read(&self, statistics: ReadStatistics) {
        self.cache.push_statistics(statistics);
    }
}

fn decode_all<TEntity: MyNoSqlEntity>(
    db_rows: Vec<Arc<my_no_sql_grpc_core::db::DbRow>>,
) -> Result<Vec<TEntity>, MyNoSqlReaderError> {
    let mut result = Vec::with_capacity(db_rows.len());

    for db_row in db_rows {
        result.push(decode(&db_row.to_vec())?);
    }

    Ok(result)
}

/// A row which does not decode was written under a schema this build does not
/// have. Saying so is the only honest answer - dropping it silently would look
/// like the row is not there.
fn decode<TEntity: MyNoSqlEntity>(row: &[u8]) -> Result<TEntity, MyNoSqlReaderError> {
    TEntity::from_slice(row).map_err(|err| MyNoSqlReaderError::CanNotParseRow(err.to_string()))
}
