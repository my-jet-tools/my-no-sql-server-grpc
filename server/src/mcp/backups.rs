//! Reading inside an archive, shared by the four backup tools.
//!
//! A backup is a zip of one namespace, so every one of them starts the same
//! way - name the namespace, name the file, read it into memory - and then goes
//! one level deeper than the last. Doing that in one place is what keeps the
//! four of them saying the same thing when a name is wrong.

use std::sync::Arc;

use crate::app::AppContext;
use crate::persist::backup::{BackupContent, BackupTableContent};

/// The namespace a backup call names.
///
/// **Not** resolved through the live namespaces: an archive outlives the
/// namespace it was taken of, and refusing to look inside the snapshots of a
/// namespace somebody has just deleted would hide the only copy left.
pub fn backup_namespace_name(namespace: Option<&str>) -> String {
    crate::app::DbNamespaces::resolve_name(namespace.unwrap_or("")).to_string()
}

pub async fn read_backup(
    app: &Arc<AppContext>,
    namespace: Option<&str>,
    file_name: &str,
) -> Result<BackupContent, String> {
    super::check_if_initialized(app)?;

    let name_space = backup_namespace_name(namespace);

    crate::db_operations::backup::inspect(app, &name_space, file_name)
        .await
        .map_err(|err| {
            format!("{err}. Call get_list_of_backups to see which snapshots '{name_space}' has.")
        })
}

pub fn find_backup_table<'s>(
    content: &'s BackupContent,
    file_name: &str,
    table_name: &str,
) -> Result<&'s BackupTableContent, String> {
    content
        .tables
        .iter()
        .find(|itm| itm.table_name == table_name)
        .ok_or_else(|| {
            format!(
                "The backup '{file_name}' has no table '{table_name}'. It has: {}",
                names(content)
            )
        })
}

fn names(content: &BackupContent) -> String {
    if content.tables.is_empty() {
        return "none".to_string();
    }

    content
        .tables
        .iter()
        .map(|itm| itm.table_name.as_str())
        .collect::<Vec<&str>>()
        .join(", ")
}
