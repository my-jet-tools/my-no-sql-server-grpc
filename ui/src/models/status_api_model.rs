use serde::*;

use super::{ReaderApiModel, TableApiModel};

/// `GET /api/Status`.
///
/// Shaped by this server rather than by the JSON version: what it answers is
/// keyed by namespace, it has no notion of a connected writer - a write is a
/// unary call that is over by the time it is counted - and what it has instead
/// is the open transactions.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct StatusApiModel {
    #[serde(default)]
    pub server: ServerApiModel,
    #[serde(default)]
    pub namespaces: Vec<NamespaceStatusApiModel>,
    #[serde(default)]
    pub readers: Vec<ReaderApiModel>,
    #[serde(default)]
    pub transactions: Vec<TransactionApiModel>,
}

impl StatusApiModel {
    /// The namespace the UI is pointed at, or the first one the server reported
    /// so a fresh browser shows data instead of an empty shell.
    pub fn namespace(&self, selected: Option<&str>) -> Option<&NamespaceStatusApiModel> {
        match selected {
            Some(name) => self
                .namespaces
                .iter()
                .find(|namespace| namespace.name == name),
            None => self.namespaces.first(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ServerApiModel {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub location: String,
    #[serde(rename = "startedAt", default)]
    pub started_at: String,
    #[serde(rename = "upTimeSecs", default)]
    pub up_time_secs: f64,
    #[serde(rename = "grpcPort", default)]
    pub grpc_port: u16,
    #[serde(rename = "httpPort", default)]
    pub http_port: u16,
    #[serde(rename = "compressData", default)]
    pub compress_data: bool,
    #[serde(rename = "persistenceDest", default)]
    pub persistence_dest: String,
    #[serde(default)]
    pub backups: BackupsApiModel,
    /// The MCP write window. The UI shows it but cannot open it - that is
    /// deliberate on the server side, and the UI has its own window.
    #[serde(rename = "mcpWrites", default)]
    pub mcp_writes: WriteWindowApiModel,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct BackupsApiModel {
    #[serde(default)]
    pub configured: bool,
    #[serde(rename = "intervalSecs", default)]
    pub interval_secs: Option<u64>,
    #[serde(rename = "maxBackups", default)]
    pub max_backups: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct WriteWindowApiModel {
    #[serde(default)]
    pub open: bool,
    #[serde(rename = "remainingSecs", default)]
    pub remaining_secs: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct NamespaceStatusApiModel {
    #[serde(default)]
    pub name: String,
    #[serde(rename = "tablesCount", default)]
    pub tables_count: u64,
    #[serde(rename = "partitionsCount", default)]
    pub partitions_count: u64,
    #[serde(rename = "rowsCount", default)]
    pub rows_count: u64,
    #[serde(rename = "dataSize", default)]
    pub data_size: u64,
    #[serde(rename = "persistQueue", default)]
    pub persist_queue: PersistQueueApiModel,
    #[serde(default)]
    pub tables: Vec<TableApiModel>,
}

/// What is still waiting to be written to disk. Two numbers rather than one:
/// a partition and a table's metadata are different things to lose.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct PersistQueueApiModel {
    #[serde(default)]
    pub partitions: u64,
    #[serde(rename = "tablesMetadata", default)]
    pub tables_metadata: u64,
    #[serde(rename = "lastPersistedAt", default)]
    pub last_persisted_at: Option<String>,
}

/// An open transaction. It holds accumulated actions that no table has seen
/// yet, which is why the UI shows how old it is.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct TransactionApiModel {
    #[serde(default)]
    pub id: String,
    #[serde(default = "crate::models::default_namespace")]
    pub namespace: String,
    #[serde(default)]
    pub table: String,
    #[serde(default)]
    pub actions: u64,
    #[serde(rename = "startedAt", default)]
    pub started_at: String,
    #[serde(rename = "lastIncomingSecsAgo", default)]
    pub last_incoming_secs_ago: f64,
}
