use serde_json::Value;

use crate::models::*;

pub struct RequestError {
    pub message: String,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl From<reqwest::Error> for RequestError {
    fn from(err: reqwest::Error) -> Self {
        Self {
            message: err.to_string(),
        }
    }
}

fn get_base_url() -> String {
    let settings = dioxus_utils::js::GlobalAppSettings::new();
    let origin = settings.get_origin();
    match origin.strip_suffix('/') {
        Some(trimmed) => trimmed.to_string(),
        None => origin.to_string(),
    }
}

/// Header naming the namespace a request works in. No header means the default
/// namespace — which is exactly what the UI sends when nothing is selected, so
/// the pre-namespace behaviour is preserved byte for byte.
const NAMESPACE_HEADER: &str = "ns";

/// Every request is built through here so the namespace can never be forgotten
/// at a call site. The value is read from localStorage on each call rather than
/// threaded through a `Signal`: these are free `async fn`s, not components, so
/// they cannot reach into the Dioxus context.
fn request(method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
    let builder = reqwest::Client::new().request(method, url);

    match crate::storage::load_namespace() {
        Some(namespace) => builder.header(NAMESPACE_HEADER, namespace),
        None => builder,
    }
}

/// Deliberately namespace-less: this is the call that tells us which namespaces
/// exist, and the server creates a namespace the moment it sees an unknown name
/// in the header. Sending a stale value here would conjure the very namespace we
/// are trying to check for.
fn request_without_namespace(method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new().request(method, url)
}

pub fn download_rows_url(table_name: &str, partition_key: &str) -> String {
    // A download is a top-level browser navigation, so it cannot carry the `ns`
    // header — the server accepts the namespace as a query parameter for exactly
    // this case.
    let namespace = match crate::storage::load_namespace() {
        Some(namespace) => format!("&ns={}", url_escape(namespace.as_str())),
        None => String::new(),
    };

    format!(
        "{}/api/Row/Download?tableName={}&partitionKey={}{}",
        get_base_url(),
        url_escape(table_name),
        url_escape(partition_key),
        namespace,
    )
}

pub async fn get_namespaces_list() -> Result<Vec<NamespaceApiModel>, RequestError> {
    let url = format!("{}/api/Namespaces/List", get_base_url());
    let response = request_without_namespace(reqwest::Method::GET, &url)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to load namespaces: {}", response.status()),
        });
    }
    let result: Vec<NamespaceApiModel> = response.json().await?;
    Ok(result)
}

fn url_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let c = *byte;
        let safe = matches!(
            c,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~'
        );
        if safe {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{:02X}", c));
        }
    }
    out
}

pub async fn get_status() -> Result<StatusApiModel, RequestError> {
    let url = format!("{}/api/Status", get_base_url());
    let response = request(reqwest::Method::GET, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to load status: {}", response.status()),
        });
    }
    let result: StatusApiModel = response.json().await?;
    Ok(result)
}

pub async fn get_connections() -> Result<ConnectionsApiModel, RequestError> {
    let url = format!("{}/api/Connections", get_base_url());
    let response = request(reqwest::Method::GET, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to load connections: {}", response.status()),
        });
    }
    let result: ConnectionsApiModel = response.json().await?;
    Ok(result)
}

pub async fn get_tables_list() -> Result<Vec<TableListItemApiModel>, RequestError> {
    let url = format!("{}/api/Tables/List", get_base_url());
    let response = request(reqwest::Method::GET, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to load tables: {}", response.status()),
        });
    }
    let result: Vec<TableListItemApiModel> = response.json().await?;
    Ok(result)
}

/// Per-partition metrics - records count and data size - for one window of a
/// table's partitions.
///
/// Answers the `{amount, data}` envelope: `amount` is the whole table's
/// partition count, so the pager knows how many pages there are without
/// fetching them. `skip`/`limit` absent means the whole table, which is what a
/// small table wants and what a big one must not ask for.
pub async fn get_partition_details(
    table_name: &str,
    skip: Option<usize>,
    limit: Option<usize>,
) -> Result<PagedApiModel<PartitionMetricApiModel>, RequestError> {
    let mut url = format!(
        "{}/api/Partitions/Details?tableName={}",
        get_base_url(),
        url_escape(table_name),
    );

    if let Some(skip) = skip {
        url.push_str(&format!("&skip={}", skip));
    }

    if let Some(limit) = limit {
        url.push_str(&format!("&limit={}", limit));
    }

    let response = request(reqwest::Method::GET, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to load partitions: {}", response.status()),
        });
    }
    let result: PagedApiModel<PartitionMetricApiModel> = response.json().await?;
    Ok(result)
}

/// One window of a partition's rows, rendered by the server through each row's
/// own schema.
///
/// `skip`/`limit` go to the server rather than slicing a fetched list: a big
/// partition is exactly the case where the difference matters.
pub async fn get_rows(
    table_name: &str,
    partition_key: &str,
    skip: Option<usize>,
    limit: Option<usize>,
) -> Result<Vec<Value>, RequestError> {
    let mut url = format!(
        "{}/api/Row?tableName={}&partitionKey={}",
        get_base_url(),
        url_escape(table_name),
        url_escape(partition_key),
    );

    if let Some(skip) = skip {
        url.push_str(&format!("&skip={}", skip));
    }

    if let Some(limit) = limit {
        url.push_str(&format!("&limit={}", limit));
    }

    let response = request(reqwest::Method::GET, &url).send().await?;

    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to load rows: {}", response.status()),
        });
    }

    // Plain JSON, no negotiated compression: this server keeps its rows
    // uncompressed by decision - a protobuf row is already the compact form -
    // so there is nothing for a `x-compress` request to do.
    let result: Vec<Value> = response.json().await?;

    Ok(result)
}

pub async fn delete_row(
    table_name: &str,
    partition_key: &str,
    row_key: &str,
) -> Result<(), RequestError> {
    ensure_ui_writes_enabled().await?;
    let url = format!(
        "{}/api/Row?tableName={}&partitionKey={}&rowKey={}",
        get_base_url(),
        url_escape(table_name),
        url_escape(partition_key),
        url_escape(row_key),
    );
    let response = request(reqwest::Method::DELETE, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to delete row: {}", response.status()),
        });
    }
    Ok(())
}

pub async fn get_ui_settings() -> Result<crate::settings::UiServerSettings, RequestError> {
    let url = format!("{}/api/Settings", get_base_url());
    let response = request(reqwest::Method::GET, &url).send().await?;
    if !response.status().is_success() {
        // Treat any non-success as "settings not available yet" — fall
        // back to defaults so the UI keeps working on an older server.
        return Ok(crate::settings::UiServerSettings::default());
    }
    #[derive(serde::Deserialize)]
    struct Payload {
        #[serde(rename = "warnMs")]
        warn_ms: u32,
        #[serde(rename = "badMs")]
        bad_ms: u32,
        #[serde(rename = "mcpWritesEnabled", default)]
        mcp_writes_enabled: bool,
        #[serde(rename = "mcpWritesRemainingSecs", default)]
        mcp_writes_remaining_secs: Option<u64>,
        #[serde(rename = "uiWritesEnabled", default)]
        ui_writes_enabled: bool,
        #[serde(rename = "uiWritesRemainingSecs", default)]
        ui_writes_remaining_secs: Option<u64>,
    }
    let p: Payload = response.json().await?;
    Ok(crate::settings::UiServerSettings {
        thresholds: crate::settings::HealthThresholds {
            warn_ms: p.warn_ms,
            bad_ms: p.bad_ms,
        },
        mcp_writes_enabled: p.mcp_writes_enabled,
        mcp_writes_remaining_secs: p.mcp_writes_remaining_secs,
        ui_writes_enabled: p.ui_writes_enabled,
        ui_writes_remaining_secs: p.ui_writes_remaining_secs,
    })
}

pub async fn get_health_thresholds() -> Result<crate::settings::HealthThresholds, RequestError> {
    Ok(get_ui_settings().await?.thresholds)
}

pub async fn set_health_thresholds(
    t: crate::settings::HealthThresholds,
) -> Result<(), RequestError> {
    let url = format!("{}/api/Settings", get_base_url());
    #[derive(serde::Serialize)]
    struct Payload {
        #[serde(rename = "warnMs")]
        warn_ms: u32,
        #[serde(rename = "badMs")]
        bad_ms: u32,
    }
    let response = request(reqwest::Method::POST, &url)
        .json(&Payload {
            warn_ms: t.warn_ms,
            bad_ms: t.bad_ms,
        })
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to save settings: {}", response.status()),
        });
    }
    Ok(())
}

/// Enables or disables the MCP write tools via POST
/// `/api/Settings/McpWrites`. Enabling opens a 10-minute window on the
/// server; disabling closes it immediately.
pub async fn set_mcp_writes(enabled: bool) -> Result<(), RequestError> {
    let url = format!("{}/api/Mcp/Writes?enabled={}", get_base_url(), enabled);
    let response = request(reqwest::Method::POST, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to update MCP writes: {}", response.status()),
        });
    }
    Ok(())
}

/// Enables or disables destructive UI writes via POST
/// `/api/Settings/UiWrites`. Enabling opens a 10-minute window on the
/// server; disabling closes it immediately.
pub async fn set_ui_writes(enabled: bool) -> Result<(), RequestError> {
    let url = format!(
        "{}/api/Settings/UiWrites?enabled={}",
        get_base_url(),
        enabled
    );
    let response = request(reqwest::Method::POST, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to update write access: {}", response.status()),
        });
    }
    Ok(())
}

/// Message shown when a write is attempted while write access is off.
pub const WRITE_ACCESS_DISABLED: &str = "Write access is DISABLED. \
    Open Settings \u{2192} Write access and click \"Enable\" to allow writes for 10 minutes.";

/// Gate every destructive UI write goes through. The server owns the
/// 10-minute window, so the flag is re-read per call rather than cached — a
/// page left open past the expiry cannot keep writing. Any failure to reach
/// the server also blocks the write (fail closed).
///
/// This is an admin guardrail, not a security boundary: the UI writes through
/// the same public REST API that SDK client apps use (`/api/Row`,
/// `/api/Bulk/Delete`, `/api/Backup/...`), so those endpoints cannot be gated
/// server-side without breaking every writer app.
async fn ensure_ui_writes_enabled() -> Result<(), RequestError> {
    if get_ui_settings().await?.ui_writes_enabled {
        return Ok(());
    }
    Err(RequestError {
        message: WRITE_ACCESS_DISABLED.to_string(),
    })
}

pub async fn get_snapshots_list() -> Result<Vec<SnapshotFileApiModel>, RequestError> {
    let url = format!("{}/api/Backup/List", get_base_url());
    let response = request(reqwest::Method::GET, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to load snapshots: {}", response.status()),
        });
    }
    let result: Vec<SnapshotFileApiModel> = response.json().await?;
    Ok(result)
}

/// Forces the server to create a snapshot (backup) right now, ignoring the
/// scheduled interval, via POST `/api/Backup/MakeBackup`.
pub async fn make_snapshot() -> Result<(), RequestError> {
    let url = format!("{}/api/Backup/MakeBackup", get_base_url());
    let response = request(reqwest::Method::POST, &url).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(RequestError {
            message: format!("Make snapshot failed ({}): {}", status, body),
        });
    }
    Ok(())
}

pub async fn get_snapshot_tables(
    file_name: &str,
) -> Result<Vec<SnapshotTableApiModel>, RequestError> {
    let url = format!(
        "{}/api/Backup/Tables?fileName={}",
        get_base_url(),
        url_escape(file_name),
    );
    let response = request(reqwest::Method::GET, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to load snapshot tables: {}", response.status()),
        });
    }
    let result: Vec<SnapshotTableApiModel> = response.json().await?;
    Ok(result)
}

pub async fn get_snapshot_partitions(
    file_name: &str,
    table_name: &str,
) -> Result<Vec<String>, RequestError> {
    let url = format!(
        "{}/api/Backup/Partitions?fileName={}&tableName={}",
        get_base_url(),
        url_escape(file_name),
        url_escape(table_name),
    );
    let response = request(reqwest::Method::GET, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to load snapshot partitions: {}", response.status()),
        });
    }
    let result: Vec<String> = response.json().await?;
    Ok(result)
}

pub async fn get_snapshot_rows(
    file_name: &str,
    table_name: &str,
    partition_key: &str,
) -> Result<Vec<Value>, RequestError> {
    let url = format!(
        "{}/api/Backup/Rows?fileName={}&tableName={}&partitionKey={}",
        get_base_url(),
        url_escape(file_name),
        url_escape(table_name),
        url_escape(partition_key),
    );
    let response = request(reqwest::Method::GET, &url).send().await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to load snapshot rows: {}", response.status()),
        });
    }
    let result: Vec<Value> = response.json().await?;
    Ok(result)
}

/// Restores a single table (or all tables when `table_name` is "*") from a
/// snapshot file in the server's backup folder via POST
/// `/api/Backup/RestoreFromBackup`.
pub async fn restore_table_from_backup(
    file_name: &str,
    table_name: &str,
    clean_table: bool,
) -> Result<(), RequestError> {
    ensure_ui_writes_enabled().await?;
    let url = format!("{}/api/Backup/RestoreFromBackup", get_base_url());
    let body = format!(
        "tableName={}&fileName={}&cleanTable={}",
        url_escape(table_name),
        url_escape(file_name),
        if clean_table { "true" } else { "false" },
    );
    let response = request(reqwest::Method::POST, &url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(RequestError {
            message: format!("Restore failed ({}): {}", status, body),
        });
    }
    Ok(())
}

/// Restores a single partition of a table from a snapshot file in the server's
/// backup folder via POST `/api/Backup/RestorePartition`. The table must already
/// exist on the server.
pub async fn restore_partition_from_backup(
    file_name: &str,
    table_name: &str,
    partition_key: &str,
) -> Result<(), RequestError> {
    ensure_ui_writes_enabled().await?;
    let url = format!("{}/api/Backup/RestorePartition", get_base_url());
    let body = format!(
        "fileName={}&tableName={}&partitionKey={}",
        url_escape(file_name),
        url_escape(table_name),
        url_escape(partition_key),
    );
    let response = request(reqwest::Method::POST, &url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(RequestError {
            message: format!("Restore failed ({}): {}", status, body),
        });
    }
    Ok(())
}

pub async fn bulk_delete_rows(
    table_name: &str,
    partition_key: &str,
    row_keys: &[String],
) -> Result<(), RequestError> {
    ensure_ui_writes_enabled().await?;
    let mut body = std::collections::BTreeMap::new();
    body.insert(partition_key.to_string(), row_keys.to_vec());

    let url = format!(
        "{}/api/Rows/BulkDelete?tableName={}",
        get_base_url(),
        url_escape(table_name),
    );
    let response = request(reqwest::Method::POST, &url)
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to bulk-delete rows: {}", response.status()),
        });
    }
    Ok(())
}

pub async fn bulk_delete_many(
    table_name: &str,
    grouped: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<(), RequestError> {
    ensure_ui_writes_enabled().await?;
    let url = format!(
        "{}/api/Rows/BulkDelete?tableName={}",
        get_base_url(),
        url_escape(table_name),
    );
    let response = request(reqwest::Method::POST, &url)
        .json(grouped)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(RequestError {
            message: format!("Failed to bulk-delete rows: {}", response.status()),
        });
    }
    Ok(())
}
