use serde::Deserialize;

pub const SETTINGS_FILE: &str = "~/.mynosqlservergrpc";

/// Deserialize only, and `Debug` written by hand below: this struct holds a
/// secret, and a derived one is a `{:?}` away from putting it in a log.
#[derive(Deserialize)]
pub struct SettingsModel {
    #[serde(rename = "PersistenceDest")]
    pub persistence_dest: String,

    #[serde(rename = "Location")]
    pub location: String,

    #[serde(rename = "CompressData")]
    pub compress_data: bool,

    #[serde(rename = "SkipBrokenPartitions")]
    pub skip_broken_partitions: bool,

    /// Where backups are written. **Not** inside `PersistenceDest`: every folder
    /// there is loaded as a namespace at start up. Absent means backups are not
    /// configured and every call about them says so - writing gigabytes into a
    /// path nobody named is worse than refusing.
    #[serde(rename = "BackupsDest", default)]
    pub backups_dest: Option<String>,

    /// How often a backup is taken. Absent means only by hand: how often to back
    /// up, and how many to keep, is the operator's policy and not something to
    /// guess at.
    #[serde(rename = "BackupIntervalSecs", default)]
    pub backup_interval_secs: Option<u64>,

    /// How many backups to keep. Absent means all of them.
    #[serde(rename = "MaxBackups", default)]
    pub max_backups: Option<usize>,

    /// The key the HTTP surface asks for. Absent means it asks for nothing and
    /// the surface is open, which is what it was until this setting existed -
    /// turning protection on is the operator's act, never a silent upgrade.
    ///
    /// gRPC deliberately has no key: it is the write transport, it is not meant
    /// to be exposed, and a key on it would be a second thing to rotate for no
    /// second guarantee.
    #[serde(rename = "ApiKey", default)]
    pub api_key: Option<String>,
}

impl std::fmt::Debug for SettingsModel {
    /// Everything except the key, and the key as whether it is set. A settings
    /// dump is the most likely place for a secret to escape.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsModel")
            .field("persistence_dest", &self.persistence_dest)
            .field("location", &self.location)
            .field("compress_data", &self.compress_data)
            .field("skip_broken_partitions", &self.skip_broken_partitions)
            .field("backups_dest", &self.backups_dest)
            .field("backup_interval_secs", &self.backup_interval_secs)
            .field("max_backups", &self.max_backups)
            .field("api_key", &self.api_key.as_ref().map(|_| "<set>"))
            .finish()
    }
}

impl SettingsModel {
    /// `BackupsDest` with `~` and the environment variables resolved.
    pub fn get_backups_dest(&self) -> Option<String> {
        let dest = self.backups_dest.as_ref()?;

        Some(rust_extensions::file_utils::format_path(dest.as_str()).to_string())
    }

    /// `PersistenceDest` with `~` and the environment variables resolved.
    pub fn get_persistence_dest(&self) -> String {
        rust_extensions::file_utils::format_path(self.persistence_dest.as_str()).to_string()
    }
}

pub async fn read_settings() -> SettingsModel {
    let file_name = rust_extensions::file_utils::format_path(SETTINGS_FILE);

    let file_content = tokio::fs::read(file_name.as_str()).await;

    // Nothing this process can do without its settings, and every later failure
    // would be a confusing consequence of this one.
    if let Err(err) = &file_content {
        panic!(
            "Can not open the settings file [{}]. Err: {}",
            file_name.as_str(),
            err
        );
    }

    serde_yaml::from_slice(file_content.unwrap().as_slice()).unwrap()
}
