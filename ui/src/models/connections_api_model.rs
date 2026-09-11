use serde::*;

use super::ReaderApiModel;

/// `GET /api/Connections` - readers and nothing else.
///
/// There is no writers array here and no traffic counters: on this server a
/// write is a unary gRPC call with no session behind it, so there is nothing to
/// list between two writes.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct ConnectionsApiModel {
    #[serde(default)]
    pub readers: Vec<ReaderApiModel>,
}
