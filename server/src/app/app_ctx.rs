use std::sync::Arc;

use rust_extensions::AppStates;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::settings_reader::SettingsModel;

use crate::json_view::JsonSchemasCache;
use crate::persist::backup::BackupsRepo;
use crate::reader::ReaderSessions;
use crate::transactions::Transactions;

use super::{DbNamespaces, WriteWindow};

pub const APP_NAME: &str = env!("CARGO_PKG_NAME");
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct AppContext {
    pub namespaces: DbNamespaces,
    /// Resolved schemas used to render rows as JSON.
    pub json_schemas: JsonSchemasCache,
    pub reader_sessions: ReaderSessions,
    /// Transactions which are still being built - nothing here has reached a
    /// table yet.
    pub transactions: Transactions,
    /// Where backups are written and read. Not configured is a normal state -
    /// every call about them says so.
    pub backups: BackupsRepo,
    pub settings: Arc<SettingsModel>,
    pub states: Arc<AppStates>,
    /// When this process came up - the moment an uptime is measured from.
    pub created: DateTimeAsMicroseconds,
    /// Where the two listeners bound. Resolved once, here, because the answer
    /// has to be the same for the listener and for whoever asks the server
    /// where it is.
    pub http_endpoint: std::net::SocketAddr,
    pub grpc_endpoint: std::net::SocketAddr,
    /// One persist pass at a time. The timer, the flush call and the shutdown
    /// drain all write through the same page-files, and a partition landing
    /// before the `tables.meta` entry that names its table is a table restored
    /// with default attributes on the next boot.
    ///
    /// A `tokio` mutex because it is held across the file I/O of a whole pass.
    pub persist_lock: tokio::sync::Mutex<()>,
    /// The window in which the MCP write tools may write.
    mcp_writes: WriteWindow,
    /// The window in which the UI's destructive buttons work. Separate from the
    /// MCP one on purpose: opening the writes for an agent must not silently
    /// unlock the delete buttons of a page somebody left open.
    pub ui_writes: WriteWindow,
}

impl AppContext {
    pub fn new(settings: Arc<SettingsModel>) -> Self {
        Self {
            namespaces: DbNamespaces::new(),
            json_schemas: JsonSchemasCache::new(),
            reader_sessions: ReaderSessions::new(),
            transactions: Transactions::new(),
            backups: BackupsRepo::new(settings.get_backups_dest()),
            settings,
            states: Arc::new(AppStates::create_un_initialized()),
            created: DateTimeAsMicroseconds::now(),
            http_endpoint: crate::listen_endpoints::http(),
            grpc_endpoint: crate::listen_endpoints::grpc(),
            persist_lock: tokio::sync::Mutex::new(()),
            mcp_writes: WriteWindow::new(),
            ui_writes: WriteWindow::new(),
        }
    }

    /// Opens the MCP write tools for [`WRITE_WINDOW_SECS`], from now.
    ///
    /// Delegated rather than open-coded: the UI has a window of its own with the
    /// same rules, and one decision wants one spelling.
    pub fn open_mcp_writes(&self, now: DateTimeAsMicroseconds) -> i64 {
        self.mcp_writes.open(now)
    }

    pub fn close_mcp_writes(&self) {
        self.mcp_writes.close();
    }

    /// How many seconds the window still has, or `None` when it is shut.
    pub fn mcp_writes_remaining_secs(&self, now: DateTimeAsMicroseconds) -> Option<i64> {
        self.mcp_writes.remaining_secs(now)
    }

    pub fn mcp_writes_are_open(&self, now: DateTimeAsMicroseconds) -> bool {
        self.mcp_writes_remaining_secs(now).is_some()
    }
}
