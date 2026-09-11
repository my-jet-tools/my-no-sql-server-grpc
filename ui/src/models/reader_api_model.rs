use serde::*;

/// One reader session, as `GET /api/Status` and `GET /api/Connections` both
/// report it - the same row in both places on purpose.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct ReaderApiModel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default = "crate::models::default_namespace")]
    pub namespace: String,
    #[serde(default)]
    pub ip: String,
    #[serde(rename = "connectedAt", default)]
    pub connected_at: String,
    /// Seconds since the session last asked for changes, not a rendered
    /// duration: the question is whether it is still asking, and that is
    /// something to compare against a threshold.
    #[serde(rename = "lastIncomingSecsAgo", default)]
    pub last_incoming_secs_ago: f64,
    /// Chunks the server is holding for this session. A number that keeps
    /// growing is a reader that stopped reading.
    #[serde(rename = "pendingChunks", default)]
    pub pending_chunks: u64,
    #[serde(default)]
    pub tables: Vec<String>,
}
