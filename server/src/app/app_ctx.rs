use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use rust_extensions::AppStates;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::settings_reader::SettingsModel;

use crate::json_view::JsonSchemasCache;
use crate::persist::backup::BackupsRepo;
use crate::reader::ReaderSessions;
use crate::transactions::Transactions;

use super::DbNamespaces;

pub const APP_NAME: &str = env!("CARGO_PKG_NAME");
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long the MCP write tools stay open once somebody opens them.
///
/// Ten minutes is long enough for a person to say what they want done and
/// short enough that a window left open is a window that closes itself. It is
/// the same figure the JSON version uses, and for the same reason.
pub const MCP_WRITES_WINDOW_SECS: i64 = 600;

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
    /// One persist pass at a time. The timer, the flush call and the shutdown
    /// drain all write through the same page-files, and a partition landing
    /// before the `tables.meta` entry that names its table is a table restored
    /// with default attributes on the next boot.
    ///
    /// A `tokio` mutex because it is held across the file I/O of a whole pass.
    pub persist_lock: tokio::sync::Mutex<()>,
    /// When the MCP write window closes, in unix microseconds. `0` - closed.
    ///
    /// Held in memory and never persisted: a restart leaves the writes shut,
    /// which is the state anybody would assume of a server they have just
    /// brought up.
    mcp_writes_open_until: AtomicI64,
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
            persist_lock: tokio::sync::Mutex::new(()),
            mcp_writes_open_until: AtomicI64::new(0),
        }
    }

    /// Opens the MCP write tools for [`MCP_WRITES_WINDOW_SECS`], from now.
    ///
    /// Calling it again while a window is open moves the end further out rather
    /// than adding to it: the question being answered is "how long from now",
    /// and two clicks a minute apart must not add up to twenty minutes.
    pub fn open_mcp_writes(&self, now: DateTimeAsMicroseconds) -> i64 {
        let mut until = now;
        until.add_seconds(MCP_WRITES_WINDOW_SECS);

        self.mcp_writes_open_until
            .store(until.unix_microseconds, Ordering::SeqCst);

        MCP_WRITES_WINDOW_SECS
    }

    pub fn close_mcp_writes(&self) {
        self.mcp_writes_open_until.store(0, Ordering::SeqCst);
    }

    /// How many seconds the window still has, or `None` when it is shut.
    pub fn mcp_writes_remaining_secs(&self, now: DateTimeAsMicroseconds) -> Option<i64> {
        let until = self.mcp_writes_open_until.load(Ordering::SeqCst);

        if until <= now.unix_microseconds {
            return None;
        }

        // Rounded up: with 1.4 seconds left the honest answer is "under two",
        // and `0` would read as shut while the window is still open.
        let left = until - now.unix_microseconds;
        let mut secs = left / 1_000_000;

        if left % 1_000_000 != 0 {
            secs += 1;
        }

        Some(secs)
    }

    pub fn mcp_writes_are_open(&self, now: DateTimeAsMicroseconds) -> bool {
        self.mcp_writes_remaining_secs(now).is_some()
    }
}
