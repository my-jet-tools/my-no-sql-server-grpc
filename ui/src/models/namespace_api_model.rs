use serde::{Deserialize, Serialize};

/// Name the server gives the namespace every pre-namespace client works in.
pub const DEFAULT_NAMESPACE: &str = "default";

/// Name the server gives the namespace every pre-namespace client works in.
/// Used as the serde default so a connection reported by an older server —
/// which sends no `namespace` field at all — still renders as `default` rather
/// than as an empty cell.
pub fn default_namespace() -> String {
    DEFAULT_NAMESPACE.to_string()
}

/// One entry of `GET /api/Namespaces/List`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NamespaceApiModel {
    pub name: String,
    #[serde(rename = "tablesAmount")]
    pub tables_amount: u64,
}
