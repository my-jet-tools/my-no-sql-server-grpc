use std::sync::Arc;

use my_no_sql_grpc_core::db::{DbRow, DbTableAttributes, PartitionRowKeys};

/// Rows of one partition, as `InitPartitions` carries them.
#[derive(Clone)]
pub struct PartitionRows {
    pub partition_key: String,
    pub rows: Vec<Arc<DbRow>>,
}

/// One item of a session's queue - exactly what one `GetChange` answer carries.
///
/// A batch of records is queued as several chunks followed by its `End`, so the
/// reader applies it in one go and never ends up holding half of it. A partition
/// may appear in more than one chunk of the same batch when it did not fit into
/// one: the reader is accumulating until the `End` anyway, so it has to merge
/// what it collected rather than let the later chunk overwrite the earlier one.
///
/// `CleanTable`, `DeleteTable` and `CleanPartitions` carry no records, so they
/// need no `End`.
///
/// Everything a session is told is pushed in one `enqueue` call, which is what
/// keeps a concurrent writer from slipping its own chunks between a `CleanTable`
/// and the rows that were meant to follow it.
#[derive(Clone)]
pub enum SyncChunk {
    CleanTable {
        table_name: String,
    },
    /// The table is the same table, but what it does with itself changed.
    UpdateTableAttributes {
        table_name: String,
        attributes: Arc<DbTableAttributes>,
    },
    /// The table is gone, not emptied - the reader drops it instead of keeping
    /// an empty one, and stays subscribed in case it is created again.
    DeleteTable {
        table_name: String,
    },
    CleanPartitions {
        table_name: String,
        partition_keys: Vec<String>,
    },
    InitPartitions {
        table_name: String,
        partitions: Vec<PartitionRows>,
    },
    InitPartitionsEnd {
        table_name: String,
    },
    UpdateRows {
        table_name: String,
        rows: Vec<Arc<DbRow>>,
    },
    UpdateRowsEnd {
        table_name: String,
    },
    DeleteRows {
        table_name: String,
        partitions: Vec<PartitionRowKeys>,
    },
    DeleteRowsEnd {
        table_name: String,
    },
}
