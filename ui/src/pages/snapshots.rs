use dioxus::prelude::*;
use serde_json::Value;

use crate::AppRoute;
use crate::api;
use crate::components::data::{PARTITION_KEY, ROW_KEY, RowsTable, TIME_STAMP, TablePagination};
use crate::models::{BackupsApiModel, SnapshotFileApiModel, SnapshotTableApiModel};

/// Rows of one archived partition shown at a time.
///
/// The window is applied here rather than on the wire: `/api/Backup/Rows` does
/// take `skip`/`limit`, but the api layer asks it for the whole partition, so
/// the whole partition is what this page has to keep out of the DOM.
const DEFAULT_PAGE_SIZE: usize = 100;

/// Client-side window over the rows of one archived partition. Its own struct so
/// `SnapshotsState` keeps a derived `Default` while the page size still starts
/// at something other than zero.
struct RowsPaging {
    page_size: usize,
    current_page: usize,
}

impl Default for RowsPaging {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_PAGE_SIZE,
            current_page: 0,
        }
    }
}

#[derive(Default)]
struct SnapshotsState {
    // Whether this server keeps backups at all, and the policy its timer
    // follows. Asked for before the list: with no `BackupsDest` there is no
    // backups folder, and every backup route refuses instead of answering an
    // empty one — so a page that only looked at the list would read a refusal
    // as "nothing backed up yet".
    backups: Option<BackupsApiModel>,
    // The namespace these snapshots are of. A backup here is a zip of ONE
    // namespace, so this is what a snapshot and a restore are about — not a
    // detail to leave off the screen.
    namespace: String,

    // Snapshot files list (loaded once, refreshed on demand).
    files: Vec<SnapshotFileApiModel>,
    files_started: bool,
    files_ready: bool,

    // Tables for the selected file.
    loaded_for_file: Option<String>,
    tables: Vec<SnapshotTableApiModel>,
    tables_ready: bool,

    // Partitions for the selected (file, table).
    loaded_for_table: Option<(String, String)>,
    partitions: Vec<String>,
    partitions_ready: bool,

    // Rows for the selected (file, table, partition).
    loaded_rows_for: Option<(String, String, String)>,
    row_headers: Vec<String>,
    rows: Vec<Value>,
    rows_ready: bool,
    paging: RowsPaging,

    // Active "restore from backup" dialog, if any.
    restore: Option<RestoreDialog>,

    // True while a forced "make snapshot" request is in flight.
    making_snapshot: bool,

    error: Option<String>,
}

/// State of the confirmation dialog shown when restoring from a snapshot.
///
/// It carries its own `file`, because a restore can be started from the files
/// list — where the file is not the one in the URL.
struct RestoreDialog {
    file: String,
    target: RestoreTarget,
    in_progress: bool,
    error: Option<String>,
    done: bool,
}

/// What a restore puts back.
///
/// Two shapes and not three: this server restores either the whole archive or
/// one named partition of one table. There is no per-table restore and no
/// "clean the table first" — a backup is a zip of one namespace, and what comes
/// back replaces partition by partition.
#[derive(Clone)]
enum RestoreTarget {
    /// Every table and every partition the archive holds. `tables` is what the
    /// archive was listed as holding, shown for confirmation — empty when the
    /// restore was started from the files list and nothing was listed yet.
    WholeSnapshot {
        tables: Vec<String>,
    },
    Partition {
        table: String,
        partition: String,
    },
}

impl SnapshotsState {
    fn begin_files_load(&mut self) {
        self.files_started = true;
        self.files_ready = false;
        self.error = None;
    }

    /// What the server said about backups. `files_ready` is set here for the
    /// unconfigured case too: there is no list to wait for, and leaving the
    /// page "loading" would keep Refresh disabled forever.
    fn set_backups(&mut self, backups: BackupsApiModel, namespace: String) {
        self.files_ready = !backups.configured;
        if !backups.configured {
            self.files = Vec::new();
        }
        self.backups = Some(backups);
        self.namespace = namespace;
    }

    fn backups_configured(&self) -> bool {
        self.backups.as_ref().is_some_and(|itm| itm.configured)
    }

    /// The status call itself failed, so nothing is known about backups. The
    /// root view stops loading — the error line says why — and Refresh retries.
    fn status_error(&mut self, err: String) {
        self.files_ready = true;
        self.error = Some(err);
    }

    fn set_files(&mut self, files: Vec<SnapshotFileApiModel>) {
        self.files = files;
        self.files_ready = true;
    }

    fn begin_make_snapshot(&mut self) {
        self.making_snapshot = true;
        self.error = None;
    }

    fn make_snapshot_done(&mut self) {
        self.making_snapshot = false;
        // Force the files list to reload so the freshly created snapshot shows up.
        self.files_started = false;
    }

    fn make_snapshot_error(&mut self, err: String) {
        self.making_snapshot = false;
        self.error = Some(err);
    }

    fn begin_tables_load(&mut self, file: &str) {
        self.loaded_for_file = Some(file.to_string());
        self.tables = Vec::new();
        self.tables_ready = false;
        self.error = None;
    }

    fn set_tables(&mut self, file: &str, tables: Vec<SnapshotTableApiModel>) {
        if self.loaded_for_file.as_deref() == Some(file) {
            self.tables = tables;
            self.tables_ready = true;
        }
    }

    /// The table names of the file currently listed, or nothing when the listed
    /// file is not the one being restored.
    fn listed_tables_of(&self, file: &str) -> Vec<String> {
        if self.loaded_for_file.as_deref() != Some(file) || !self.tables_ready {
            return Vec::new();
        }

        self.tables.iter().map(|itm| itm.name.clone()).collect()
    }

    fn begin_partitions_load(&mut self, key: &(String, String)) {
        self.loaded_for_table = Some(key.clone());
        self.partitions = Vec::new();
        self.partitions_ready = false;
        self.error = None;
    }

    fn set_partitions(&mut self, key: &(String, String), partitions: Vec<String>) {
        if self.loaded_for_table.as_ref() == Some(key) {
            self.partitions = partitions;
            self.partitions_ready = true;
        }
    }

    fn begin_rows_load(&mut self, key: &(String, String, String)) {
        self.loaded_rows_for = Some(key.clone());
        self.row_headers = Vec::new();
        self.rows = Vec::new();
        self.rows_ready = false;
        self.paging.current_page = 0;
        self.error = None;
    }

    fn set_rows(&mut self, key: &(String, String, String), headers: Vec<String>, rows: Vec<Value>) {
        if self.loaded_rows_for.as_ref() == Some(key) {
            self.row_headers = headers;
            self.rows = rows;
            self.rows_ready = true;
        }
    }

    fn set_page(&mut self, page: usize) {
        self.paging.current_page = page;
    }

    fn set_page_size(&mut self, size: usize) {
        self.paging.page_size = size.max(1);
        self.paging.current_page = 0;
    }

    fn open_restore_snapshot(&mut self, file: String, tables: Vec<String>) {
        self.restore = Some(RestoreDialog {
            file,
            target: RestoreTarget::WholeSnapshot { tables },
            in_progress: false,
            error: None,
            done: false,
        });
    }

    fn open_restore_partition(&mut self, file: String, table: String, partition: String) {
        self.restore = Some(RestoreDialog {
            file,
            target: RestoreTarget::Partition { table, partition },
            in_progress: false,
            error: None,
            done: false,
        });
    }

    fn restore_begin(&mut self) {
        if let Some(dialog) = self.restore.as_mut() {
            dialog.in_progress = true;
            dialog.error = None;
        }
    }

    fn restore_done(&mut self) {
        if let Some(dialog) = self.restore.as_mut() {
            dialog.in_progress = false;
            dialog.done = true;
        }
    }

    fn restore_error(&mut self, err: String) {
        if let Some(dialog) = self.restore.as_mut() {
            dialog.in_progress = false;
            dialog.error = Some(err);
        }
    }

    fn close_restore(&mut self) {
        self.restore = None;
    }
}

/// Extract `(file, table, partition)` from the current snapshot route.
fn parse_snapshot_route(route: &AppRoute) -> (Option<String>, Option<String>, Option<String>) {
    match route {
        AppRoute::SnapshotFile { file } => (Some(file.clone()), None, None),
        AppRoute::SnapshotTable { file, table } => (Some(file.clone()), Some(table.clone()), None),
        AppRoute::SnapshotPartition {
            file,
            table,
            partition,
        } => (
            Some(file.clone()),
            Some(table.clone()),
            Some(partition.clone()),
        ),
        _ => (None, None, None),
    }
}

// Route placeholders — the URL patterns for the snapshots section. `SnapshotsLayout`
// renders the whole page and reads the params via `use_route`, so these render
// nothing themselves.
#[component]
pub fn Snapshots() -> Element {
    rsx! {}
}

#[component]
pub fn SnapshotFile(file: String) -> Element {
    let _ = file;
    rsx! {}
}

#[component]
pub fn SnapshotTable(file: String, table: String) -> Element {
    let _ = (file, table);
    rsx! {}
}

#[component]
pub fn SnapshotPartition(file: String, table: String, partition: String) -> Element {
    let _ = (file, table, partition);
    rsx! {}
}

#[component]
pub fn SnapshotsLayout() -> Element {
    let mut cs = use_signal(SnapshotsState::default);
    let nav = navigator();

    let route = use_route::<AppRoute>();
    let (url_file, url_table, url_partition) = parse_snapshot_route(&route);

    // ---- what the server keeps in the way of backups, then the files list ----
    if !cs.read().files_started {
        spawn(async move {
            if cs.peek().files_started {
                return;
            }
            cs.write().begin_files_load();

            // The status answers two things nothing else here can: whether
            // backups have a folder at all, and the name of the namespace every
            // request on this page carries. Both come before the list, because
            // an unconfigured server refuses the list rather than answering it.
            match api::get_status().await {
                Ok(status) => {
                    // The api layer sends no `ns` header when nothing is
                    // selected, and the server then works in the default
                    // namespace — so that, and not "the first namespace the
                    // server reported", is what this page is pointed at.
                    let selected = crate::storage::load_namespace()
                        .unwrap_or_else(|| crate::models::DEFAULT_NAMESPACE.to_string());
                    let namespace = status
                        .namespace(Some(selected.as_str()))
                        .map(|itm| itm.name.clone())
                        // A namespace can hold snapshots and no live tables at
                        // all — an archive outlives what it was taken of — so
                        // the name the requests carry is still the name to show.
                        .unwrap_or(selected);

                    cs.write()
                        .set_backups(status.server.backups.clone(), namespace);
                }
                Err(err) => {
                    cs.write()
                        .status_error(format!("Failed to load server status: {}", err));
                    return;
                }
            }

            if !cs.peek().backups_configured() {
                return;
            }

            match api::get_snapshots_list().await {
                Ok(mut files) => {
                    // Newest first. The name IS the moment it was taken, and the
                    // `_02` suffix of a second snapshot inside one second sorts
                    // right after its bare name, so sorting by name is sorting
                    // by when they happened.
                    files.sort_by(|a, b| b.name.cmp(&a.name));
                    cs.write().set_files(files);
                }
                Err(err) => {
                    cs.write().error = Some(format!("Failed to load snapshots: {}", err));
                }
            }
        });
    }

    // Nothing below the files list is asked for until backups are known to be
    // configured: with no backups folder every one of those routes refuses, and
    // a hand-typed URL should not answer with a raw 412.
    let backups = cs.read().backups.clone();
    let backups_configured = backups.as_ref().is_some_and(|itm| itm.configured);
    let backups_unconfigured = backups.as_ref().is_some_and(|itm| !itm.configured);

    // ---- load tables whenever the URL file changes ----
    if let Some(file) = url_file.clone()
        && backups_configured
        && cs.read().loaded_for_file.as_deref() != Some(file.as_str())
    {
        spawn(async move {
            if cs.peek().loaded_for_file.as_deref() == Some(file.as_str()) {
                return;
            }
            cs.write().begin_tables_load(&file);
            match api::get_snapshot_tables(&file).await {
                Ok(tables) => cs.write().set_tables(&file, tables),
                Err(err) => {
                    cs.write().error = Some(format!("Failed to load tables: {}", err));
                }
            }
        });
    }

    // ---- load partitions whenever the URL (file, table) changes ----
    if let (Some(file), Some(table)) = (url_file.clone(), url_table.clone()) {
        let key = (file, table);
        if backups_configured && cs.read().loaded_for_table.as_ref() != Some(&key) {
            spawn(async move {
                if cs.peek().loaded_for_table.as_ref() == Some(&key) {
                    return;
                }
                cs.write().begin_partitions_load(&key);
                match api::get_snapshot_partitions(&key.0, &key.1).await {
                    Ok(partitions) => cs.write().set_partitions(&key, partitions),
                    Err(err) => {
                        cs.write().error = Some(format!("Failed to load partitions: {}", err));
                    }
                }
            });
        }
    }

    // ---- load rows whenever the URL (file, table, partition) changes ----
    if let (Some(file), Some(table), Some(pk)) =
        (url_file.clone(), url_table.clone(), url_partition.clone())
    {
        let key = (file, table, pk);
        if backups_configured && cs.read().loaded_rows_for.as_ref() != Some(&key) {
            spawn(async move {
                if cs.peek().loaded_rows_for.as_ref() == Some(&key) {
                    return;
                }
                cs.write().begin_rows_load(&key);
                match api::get_snapshot_rows(&key.0, &key.1, &key.2).await {
                    Ok(rows) => {
                        let (headers, rows) = build_rows_state(rows);
                        cs.write().set_rows(&key, headers, rows);
                    }
                    Err(err) => {
                        cs.write().error = Some(format!("Failed to load rows: {}", err));
                    }
                }
            });
        }
    }

    // ---- snapshot of state for rendering ----
    let cs_ra = cs.read();
    let error = cs_ra.error.clone();
    let namespace = cs_ra.namespace.clone();
    let files = cs_ra.files.clone();
    let files_ready = cs_ra.files_ready;
    let making_snapshot = cs_ra.making_snapshot;

    let tables_scope =
        url_file.is_some() && cs_ra.loaded_for_file.as_deref() == url_file.as_deref();
    let tables = if tables_scope {
        cs_ra.tables.clone()
    } else {
        Vec::new()
    };
    let tables_ready = tables_scope && cs_ra.tables_ready;

    let partitions_key = match (&url_file, &url_table) {
        (Some(f), Some(t)) => Some((f.clone(), t.clone())),
        _ => None,
    };
    let partitions_scope =
        partitions_key.is_some() && cs_ra.loaded_for_table.as_ref() == partitions_key.as_ref();
    let partitions = if partitions_scope {
        cs_ra.partitions.clone()
    } else {
        Vec::new()
    };
    let partitions_ready = partitions_scope && cs_ra.partitions_ready;

    let rows_key = match (&url_file, &url_table, &url_partition) {
        (Some(f), Some(t), Some(p)) => Some((f.clone(), t.clone(), p.clone())),
        _ => None,
    };
    let rows_scope = rows_key.is_some() && cs_ra.loaded_rows_for.as_ref() == rows_key.as_ref();
    let row_headers = if rows_scope {
        cs_ra.row_headers.clone()
    } else {
        Vec::new()
    };
    let rows = if rows_scope {
        cs_ra.rows.clone()
    } else {
        Vec::new()
    };
    let rows_ready = rows_scope && cs_ra.rows_ready;
    let page_size = cs_ra.paging.page_size;
    let stored_page = cs_ra.paging.current_page;
    drop(cs_ra);

    // Refresh re-fetches the deepest level matching the current URL by clearing
    // its load marker — the inline loaders above pick it up on the next render.
    let refresh_file = url_file.clone();
    let refresh_table = url_table.clone();
    let refresh_partition = url_partition.clone();
    let on_refresh = move |_| {
        let mut w = cs.write();
        match (
            refresh_file.is_some(),
            refresh_table.is_some(),
            refresh_partition.is_some(),
        ) {
            (false, _, _) => w.files_started = false,
            (true, false, _) => w.loaded_for_file = None,
            (true, true, false) => w.loaded_for_table = None,
            (true, true, true) => w.loaded_rows_for = None,
        }
    };

    // Force-create a snapshot of this namespace on the server, then reload the
    // files list. It is one namespace and not the server: the timer takes every
    // namespace because that is its job, while a button on a page showing one
    // must not spend another namespace's `MaxBackups` slot.
    let on_make_snapshot = move |_| {
        if cs.peek().making_snapshot {
            return;
        }
        cs.write().begin_make_snapshot();
        spawn(async move {
            match api::make_snapshot().await {
                Ok(_) => cs.write().make_snapshot_done(),
                Err(err) => cs
                    .write()
                    .make_snapshot_error(format!("Failed to make snapshot: {}", err)),
            }
        });
    };

    let loading = match (&url_file, &url_table, &url_partition) {
        (None, _, _) => !files_ready,
        (Some(_), None, _) => !tables_ready,
        (Some(_), Some(_), None) => !partitions_ready,
        (Some(_), Some(_), Some(_)) => !rows_ready,
    };

    // ---- navigation handlers ----
    let open_file = move |file: String| {
        nav.push(AppRoute::SnapshotFile { file });
    };
    let open_table = {
        let file = url_file.clone();
        move |table: String| {
            if let Some(file) = file.clone() {
                nav.push(AppRoute::SnapshotTable { file, table });
            }
        }
    };
    let open_partition = {
        let file = url_file.clone();
        let table = url_table.clone();
        move |partition: String| {
            if let (Some(file), Some(table)) = (file.clone(), table.clone()) {
                nav.push(AppRoute::SnapshotPartition {
                    file,
                    table,
                    partition,
                });
            }
        }
    };

    // ---- restore dialog handlers (the whole archive, or one partition) ----
    let open_restore_snapshot = move |file: String| {
        // What the archive holds is only known if it is the file being listed.
        // Restoring one straight off the files list is allowed anyway — the
        // dialog then names the file and the namespace, which is what the
        // request is about.
        let tables = {
            let ra = cs.read();
            ra.listed_tables_of(file.as_str())
        };
        cs.write().open_restore_snapshot(file, tables);
    };
    let open_restore_partition = {
        let file = url_file.clone();
        let table = url_table.clone();
        move |partition: String| {
            if let (Some(file), Some(table)) = (file.clone(), table.clone()) {
                cs.write().open_restore_partition(file, table, partition);
            }
        }
    };
    let confirm_restore = move |_| {
        let (file, target) = {
            let ra = cs.read();
            match ra.restore.as_ref() {
                Some(dialog) if !dialog.in_progress && !dialog.done => {
                    (dialog.file.clone(), dialog.target.clone())
                }
                _ => return,
            }
        };
        cs.write().restore_begin();
        spawn(async move {
            let result = match &target {
                // One request for the whole archive, which here is the whole
                // namespace: the restore route takes nothing but the file name.
                // `restore_table_from_backup` still carries the JSON version's
                // `tableName` and `cleanTable`, which this server's route does
                // not read — "*" is that server's spelling for "everything in
                // the file", and a server that never cleans a whole table can
                // only mean `false`.
                RestoreTarget::WholeSnapshot { .. } => {
                    api::restore_table_from_backup(&file, "*", false).await
                }
                RestoreTarget::Partition { table, partition } => {
                    api::restore_partition_from_backup(&file, table, partition).await
                }
            };

            match result {
                Ok(_) => cs.write().restore_done(),
                Err(err) => cs.write().restore_error(err.to_string()),
            }
        });
    };

    let go_to_files = move |_| {
        nav.push(AppRoute::Snapshots {});
    };
    let go_to_tables = {
        let file = url_file.clone();
        move |_| {
            if let Some(file) = file.clone() {
                nav.push(AppRoute::SnapshotFile { file });
            }
        }
    };
    let go_to_partitions = {
        let file = url_file.clone();
        let table = url_table.clone();
        move |_| {
            if let (Some(file), Some(table)) = (file.clone(), table.clone()) {
                nav.push(AppRoute::SnapshotTable { file, table });
            }
        }
    };

    let error_view = if let Some(err) = error {
        rsx! {
            div { style: "color: var(--danger); font-size: 12.5px; padding: 8px 0;", "{err}" }
        }
    } else {
        rsx! {}
    };

    let body = if backups_unconfigured {
        // Every level of this page reads the same backups folder, so when there
        // is none there is nothing to show at any of them.
        render_not_configured()
    } else {
        match (url_file.clone(), url_table.clone(), url_partition.clone()) {
            (None, _, _) => render_files(files, !files_ready, open_file, open_restore_snapshot),
            (Some(file), None, _) => render_tables(file, tables, !tables_ready, open_table),
            (Some(_), Some(_), None) => render_partitions(
                partitions,
                !partitions_ready,
                open_partition,
                open_restore_partition,
            ),
            (Some(_), Some(_), Some(_)) => {
                render_rows(cs, row_headers, rows, !rows_ready, page_size, stored_page)
            }
        }
    };

    let crumbs = render_crumbs(
        url_file.clone(),
        url_table.clone(),
        url_partition.clone(),
        go_to_files,
        go_to_tables,
        go_to_partitions,
    );

    let policy = render_policy(namespace.as_str(), backups.as_ref());
    let restore_render = render_restore_dialog(cs, confirm_restore);

    // Restoring a whole snapshot is offered wherever a file is open, not only in
    // the files list: the point of walking into an archive's tables and rows is
    // deciding whether to put it back.
    let restore_snapshot_button = match (&url_file, backups_configured) {
        (Some(file), true) => {
            let file = file.clone();
            let mut open_restore_snapshot = open_restore_snapshot;
            rsx! {
                button {
                    class: "btn btn--ghost btn--sm",
                    onclick: move |_| open_restore_snapshot(file.clone()),
                    "Restore snapshot"
                }
            }
        }
        _ => rsx! {},
    };

    rsx! {
        section { class: "page page--padded",
            div { style: "display: flex; flex-direction: column; gap: 14px; max-width: 960px;",
                div { style: "display: flex; align-items: center; justify-content: space-between; gap: 12px;",
                    {crumbs}
                    div { style: "display: flex; align-items: center; gap: 8px;",
                        {restore_snapshot_button}
                        button {
                            class: "btn btn--primary btn--sm",
                            disabled: making_snapshot || backups_unconfigured,
                            onclick: on_make_snapshot,
                            if making_snapshot { "Making snapshot…" } else { "Make snapshot" }
                        }
                        button {
                            class: "btn btn--ghost btn--sm",
                            disabled: loading,
                            onclick: on_refresh,
                            if loading { "Refreshing…" } else { "Refresh" }
                        }
                    }
                }
                {policy}
                {error_view}
                {body}
            }
        }
        {restore_render}
        Outlet::<AppRoute> {}
    }
}

/// The line under the header: which namespace these snapshots are of, and how
/// often the server takes one.
///
/// The namespace is not decoration. A backup is a zip of one namespace, so the
/// list, a new snapshot and a restore all mean the namespace the UI is pointed
/// at — and switching namespaces switches the whole page.
fn render_policy(namespace: &str, backups: Option<&BackupsApiModel>) -> Element {
    // Nothing known yet, or nowhere to keep backups: the body says so on its
    // own, and a half-sentence above it would only compete with it.
    let Some(backups) = backups.filter(|itm| itm.configured) else {
        return rsx! {};
    };

    // The timer runs only when both the folder and the interval are set, so an
    // interval that is not there is not "every 0 seconds" — it is no timer.
    let policy = match (backups.interval_secs, backups.max_backups) {
        (Some(interval), Some(max)) => format!(
            "taken every {}, last {} kept",
            format_interval(interval),
            max
        ),
        (Some(interval), None) => format!("taken every {}, all kept", format_interval(interval)),
        (None, _) => "no timer — taken by hand only".to_string(),
    };

    rsx! {
        div { style: "font-size: 12px; color: var(--text-muted);",
            "Snapshots of namespace "
            span { style: "font-family: var(--font-mono); color: var(--text);", "{namespace}" }
            " · {policy}"
        }
    }
}

/// A backup interval as an operator wrote it down. Seconds are the wire form,
/// but `BackupIntervalSecs` is set in hours as often as not.
fn format_interval(secs: u64) -> String {
    if secs >= 3_600 && secs.is_multiple_of(3_600) {
        return format!("{}h", secs / 3_600);
    }
    if secs >= 60 && secs.is_multiple_of(60) {
        return format!("{}m", secs / 60);
    }
    format!("{}s", secs)
}

/// Backups have nowhere to go, which is not the same thing as having none yet.
/// The server tells the two apart — it refuses the list rather than answering an
/// empty one — and so must the page: an operator who never set `BackupsDest`
/// would otherwise go on believing the timer is running.
fn render_not_configured() -> Element {
    rsx! {
        div { class: "empty-state",
            div { class: "empty-state__title", "Backups are not configured" }
            div { class: "empty-state__sub",
                "This server keeps no backups folder. Set "
                span { style: "font-family: var(--font-mono);", "BackupsDest" }
                " in its settings to the folder snapshots belong in, and "
                span { style: "font-family: var(--font-mono);", "BackupIntervalSecs" }
                " for the timer to start taking them."
            }
        }
    }
}

/// Renders the "restore from backup" confirmation dialog (or nothing when no
/// restore is in progress). `confirm` triggers the actual restore request.
fn render_restore_dialog(
    mut cs: Signal<SnapshotsState>,
    confirm: impl FnMut(()) + Clone + 'static,
) -> Element {
    let ra = cs.read();
    let Some(dialog) = ra.restore.as_ref() else {
        return rsx! {};
    };

    let namespace = ra.namespace.clone();
    let file = dialog.file.clone();
    let target = dialog.target.clone();
    let in_progress = dialog.in_progress;
    let done = dialog.done;
    let error = dialog.error.clone();
    drop(ra);

    let mut confirm = confirm;

    let error_view = if let Some(err) = error {
        rsx! {
            div { class: "dialog__error", "{err}" }
        }
    } else {
        rsx! {}
    };

    // What the archive was listed as holding. Shown for a whole-snapshot
    // restore, because that is exactly the set of tables about to be written
    // into — and the operator has usually just been looking at it.
    let tables_list = match &target {
        RestoreTarget::WholeSnapshot { tables } if !tables.is_empty() => {
            let items = tables.iter().map(|t| {
                rsx! {
                    div { key: "{t}", class: "dialog__list-item", "{t}" }
                }
            });
            rsx! {
                div { class: "dialog__list", {items} }
            }
        }
        _ => rsx! {},
    };

    let body = if done {
        let what = match &target {
            RestoreTarget::WholeSnapshot { .. } => rsx! {
                "Namespace "
                b { "{namespace}" }
                " has been restored from "
                b { "{file}" }
                "."
            },
            RestoreTarget::Partition { table, partition } => rsx! {
                "Partition "
                b { "{partition}" }
                " of table "
                b { "{table}" }
                " has been restored from "
                b { "{file}" }
                "."
            },
        };
        rsx! {
            div { class: "dialog__body",
                {what}
                {tables_list}
            }
            div { class: "dialog__footer",
                button {
                    class: "btn btn--primary btn--sm",
                    onclick: move |_| { cs.write().close_restore(); },
                    "Close"
                }
            }
        }
    } else {
        let (what, note) = match &target {
            RestoreTarget::WholeSnapshot { .. } => (
                rsx! {
                    "Restore the whole of namespace "
                    b { "{namespace}" }
                    " from snapshot "
                    b { "{file}" }
                    "?"
                },
                // Not "everything is wiped and rewritten": the archive replaces
                // the partitions it carries and leaves the rest alone, and the
                // schemas it carries are added to the tables rather than
                // replacing what they have learned since.
                "Every partition the snapshot holds replaces the one in its table. \
                 Partitions the snapshot does not carry are left as they are, and a table \
                 the snapshot has but the server does not is created.",
            ),
            RestoreTarget::Partition { table, partition } => (
                rsx! {
                    "Restore partition "
                    b { "{partition}" }
                    " of table "
                    b { "{table}" }
                    " from snapshot "
                    b { "{file}" }
                    "?"
                },
                "The partition replaces the one in the table — it ends up holding the \
                 snapshot's rows and nothing else.",
            ),
        };
        let restore_label = if in_progress {
            "Restoring…"
        } else {
            "Restore"
        };
        rsx! {
            div { class: "dialog__body",
                {what}
                div { style: "margin-top: 8px;", "{note}" }
                {tables_list}
                {error_view}
            }
            div { class: "dialog__footer",
                button {
                    class: "btn btn--ghost btn--sm",
                    disabled: in_progress,
                    onclick: move |_| { cs.write().close_restore(); },
                    "Cancel"
                }
                button {
                    class: "btn btn--primary btn--sm",
                    disabled: in_progress,
                    onclick: move |_| confirm(()),
                    "{restore_label}"
                }
            }
        }
    };

    rsx! {
        div { class: "dialog-overlay",
            div { class: "dialog",
                div { class: "dialog__header", "Restore from backup" }
                {body}
            }
        }
    }
}

fn render_crumbs(
    file: Option<String>,
    table: Option<String>,
    partition: Option<String>,
    on_root: impl FnMut(()) + Clone + 'static,
    on_file: impl FnMut(()) + Clone + 'static,
    on_table: impl FnMut(()) + Clone + 'static,
) -> Element {
    let mut on_root = on_root;
    let mut on_file = on_file;
    let mut on_table = on_table;

    let file_segment = file.clone();
    let table_segment = table.clone();
    let partition_segment = partition.clone();

    let file_active = file.is_some();
    let table_active = table.is_some();
    let partition_active = partition.is_some();

    rsx! {
        div { style: "display: flex; flex-wrap: wrap; align-items: center; gap: 6px; font-size: 13px; color: var(--text-muted);",
            button {
                class: "btn btn--ghost btn--sm",
                disabled: !file_active,
                onclick: move |_| on_root(()),
                "Snapshots"
            }
            if let Some(name) = file_segment {
                span { "›" }
                button {
                    class: "btn btn--ghost btn--sm",
                    style: "font-family: var(--font-mono);",
                    disabled: !table_active,
                    onclick: move |_| on_file(()),
                    "{name}"
                }
            }
            if let Some(name) = table_segment {
                span { "›" }
                button {
                    class: "btn btn--ghost btn--sm",
                    style: "font-family: var(--font-mono);",
                    disabled: !partition_active,
                    onclick: move |_| on_table(()),
                    "{name}"
                }
            }
            if let Some(name) = partition_segment {
                span { "›" }
                span { style: "font-family: var(--font-mono); padding: 0 6px;", "{name}" }
            }
        }
    }
}

fn render_files(
    files: Vec<SnapshotFileApiModel>,
    loading: bool,
    on_pick: impl FnMut(String) + Clone + 'static,
    on_restore: impl FnMut(String) + Clone + 'static,
) -> Element {
    if loading && files.is_empty() {
        return rsx! {
            div { class: "empty-state",
                div { class: "empty-state__title", "Loading snapshots…" }
            }
        };
    }
    if files.is_empty() {
        return rsx! {
            div { class: "empty-state",
                div { class: "empty-state__title", "No snapshots of this namespace yet" }
                div { class: "empty-state__sub",
                    "A snapshot is a zip of one namespace, written into the backup folder configured on the server. Take one now with \"Make snapshot\", or wait for the backup timer."
                }
            }
        };
    }
    let count = files.len();
    let rows = files.into_iter().map(move |file| {
        let n = file.name.clone();
        let restore_name = file.name.clone();
        let taken = format_taken_at(file.name.as_str());
        let size = crate::utils::format_bytes(file.size as f64);
        let mut on_pick = on_pick.clone();
        let mut on_restore = on_restore.clone();
        rsx! {
            tr {
                key: "{file.name}",
                style: "cursor: pointer;",
                onclick: move |_| on_pick(n.clone()),
                td { style: "font-family: var(--font-mono);", "{file.name}" }
                td { "{taken}" }
                td { class: "num", "{size}" }
                td {
                    class: "num",
                    onclick: move |evt| { evt.stop_propagation(); },
                    button {
                        class: "btn btn--ghost btn--sm",
                        onclick: move |evt| {
                            evt.stop_propagation();
                            on_restore(restore_name.clone());
                        },
                        "Restore"
                    }
                }
            }
        }
    });
    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Snapshot files" }
                span { class: "card__subtitle", "{count} file(s) · newest first" }
            }
            div { class: "card__body",
                table { class: "rt",
                    thead {
                        tr {
                            th { "File name" }
                            th { "Taken (UTC)" }
                            th { class: "num", "Size" }
                            th { class: "num", "" }
                        }
                    }
                    tbody { {rows} }
                }
            }
        }
    }
}

/// The moment a snapshot was taken, read out of its own name.
///
/// The server names one `YYYYMMDDTHHMMSS.zip` in UTC, plus `_02`, `_03` for the
/// second and later of one second — so the name is the moment, and it is the
/// only place that moment is kept. A name that does not parse is shown as a dash
/// rather than guessed at: an operator can drop an uploaded archive into the
/// folder under any name they like.
fn format_taken_at(name: &str) -> String {
    let bytes = name.as_bytes();

    if bytes.len() < 15 || bytes[8] != b'T' {
        return "—".to_string();
    }

    if !bytes[..15]
        .iter()
        .enumerate()
        .all(|(idx, byte)| idx == 8 || byte.is_ascii_digit())
    {
        return "—".to_string();
    }

    format!(
        "{}-{}-{} {}:{}:{}",
        &name[0..4],
        &name[4..6],
        &name[6..8],
        &name[9..11],
        &name[11..13],
        &name[13..15],
    )
}

/// The tables one archive holds.
///
/// No per-table restore button and no checkboxes: this server restores the whole
/// archive or one named partition, so a control offering a table on its own
/// would be a control with no request behind it. Whole-archive restore lives in
/// the page header, where the file it belongs to is the one in the URL.
fn render_tables(
    file: String,
    tables: Vec<SnapshotTableApiModel>,
    loading: bool,
    on_pick: impl FnMut(String) + Clone + 'static,
) -> Element {
    if loading && tables.is_empty() {
        return rsx! {
            div { class: "empty-state",
                div { class: "empty-state__title", "Loading tables…" }
            }
        };
    }
    if tables.is_empty() {
        return rsx! {
            div { class: "empty-state",
                div { class: "empty-state__title", "No tables in this snapshot" }
                div { class: "empty-state__sub",
                    "An archive of a namespace whose tables were all empty carries none."
                }
            }
        };
    }
    let count = tables.len();
    // What the archive carries, not what the live table has now: the two drift
    // apart the moment anything is written after the backup was taken.
    let partitions: u64 = tables.iter().map(|itm| itm.partitions_count).sum();

    let rows = tables.into_iter().map(move |t| {
        let n = t.name.clone();
        let mut on_pick = on_pick.clone();
        rsx! {
            tr {
                key: "{t.name}",
                style: "cursor: pointer;",
                onclick: move |_| on_pick(n.clone()),
                td { style: "font-family: var(--font-mono);", "{t.name}" }
                td { class: "num", "{t.partitions_count}" }
            }
        }
    });

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Tables" }
                span { class: "card__subtitle", "{count} table(s) · {partitions} partition(s) in {file}" }
            }
            div { class: "card__body",
                table { class: "rt",
                    thead {
                        tr {
                            th { "Table name" }
                            th { class: "num", "Partitions" }
                        }
                    }
                    tbody { {rows} }
                }
            }
        }
    }
}

/// The partition keys of one archived table.
///
/// The route answers `{amount, data}`, the same shape the live partition list
/// uses, but there is no second page to ask for: a zip is read whole to be read
/// at all, so `amount` is the length of `data` and the count below is the same
/// number either way.
fn render_partitions(
    partitions: Vec<String>,
    loading: bool,
    on_pick: impl FnMut(String) + Clone + 'static,
    on_restore: impl FnMut(String) + Clone + 'static,
) -> Element {
    if loading && partitions.is_empty() {
        return rsx! {
            div { class: "empty-state",
                div { class: "empty-state__title", "Loading partitions…" }
            }
        };
    }
    if partitions.is_empty() {
        return rsx! {
            div { class: "empty-state",
                div { class: "empty-state__title", "No partitions in this table" }
            }
        };
    }
    let count = partitions.len();
    let rows = partitions.into_iter().map(move |pk| {
        let p = pk.clone();
        let restore_pk = pk.clone();
        let mut on_pick = on_pick.clone();
        let mut on_restore = on_restore.clone();
        rsx! {
            tr {
                key: "{pk}",
                style: "cursor: pointer;",
                onclick: move |_| on_pick(p.clone()),
                td { style: "font-family: var(--font-mono);", "{pk}" }
                td {
                    class: "num",
                    onclick: move |evt| { evt.stop_propagation(); },
                    button {
                        class: "btn btn--ghost btn--sm",
                        onclick: move |evt| {
                            evt.stop_propagation();
                            on_restore(restore_pk.clone());
                        },
                        "Restore"
                    }
                }
            }
        }
    });
    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Partitions" }
                span { class: "card__subtitle", "{count} partition(s)" }
            }
            div { class: "card__body",
                table { class: "rt",
                    thead {
                        tr {
                            th { "Partition key" }
                            th { class: "num", "" }
                        }
                    }
                    tbody { {rows} }
                }
            }
        }
    }
}

fn render_rows(
    mut cs: Signal<SnapshotsState>,
    headers: Vec<String>,
    rows: Vec<Value>,
    loading: bool,
    page_size: usize,
    stored_page: usize,
) -> Element {
    if loading && rows.is_empty() {
        return rsx! {
            div { class: "empty-state",
                div { class: "empty-state__title", "Loading rows…" }
            }
        };
    }
    let count = rows.len();
    // An archived partition is as big as a live one, so only a page of it goes
    // into the DOM. The page is clamped rather than stored back: a partition
    // that came back shorter than the page being shown must not leave an empty
    // table with no way out of it.
    let total_pages = count.div_ceil(page_size).max(1);
    let current_page = stored_page.min(total_pages - 1);
    let page_start = current_page * page_size;
    let page_end = (page_start + page_size).min(count);
    let visible_rows: Vec<Value> = rows[page_start..page_end].to_vec();
    let visible = visible_rows.len();

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Rows" }
                span { class: "card__subtitle", "{count} row(s)" }
            }
            div { class: "card__body", style: "padding: 0;",
                RowsTable {
                    headers,
                    rows: visible_rows,
                    selected_row_key: None::<String>,
                    on_row_click: |_| {},
                }
                TablePagination {
                    // The total is known exactly here, unlike on the data page:
                    // the whole partition came back in one answer, and the
                    // window is this page's own.
                    total: Some(count),
                    loaded: visible,
                    page_size,
                    current_page,
                    on_page_change: move |p: usize| { cs.write().set_page(p); },
                    on_page_size_change: move |sz: usize| { cs.write().set_page_size(sz); },
                }
            }
        }
    }
}

/// The columns of one archived partition, taken from the rows themselves.
///
/// A backup row is rendered through the schemas the ARCHIVE carries, so its
/// columns are whatever that entity version declared — and a table the archive
/// carried no attributes for comes back keyed by protobuf field number instead,
/// with no `PartitionKey` or `RowKey` among the keys at all. That is why nothing
/// is invented here: a fixed column set would show a column of dashes for
/// exactly the archives that need reading most.
///
/// `PartitionKey`, `RowKey` and `TimeStamp` are hoisted to the front when they
/// are there. The first two are ordinary declared fields on this server, and
/// `RowsTable` freezes exactly those two columns in that order; `TimeStamp` is
/// named by the contract even without a schema, and the wire order puts it after
/// the user's fields because the server appends it when handing the row out.
fn build_rows_state(rows: Vec<Value>) -> (Vec<String>, Vec<Value>) {
    let mut headers: Vec<String> = Vec::new();

    for row in rows.iter() {
        if let Value::Object(map) = row {
            for key in map.keys() {
                if !headers.iter().any(|h| h == key) {
                    headers.push(key.clone());
                }
            }
        }
    }

    let mut front = 0usize;
    for key in [PARTITION_KEY, ROW_KEY, TIME_STAMP] {
        if let Some(pos) = headers.iter().position(|h| h == key) {
            let header = headers.remove(pos);
            headers.insert(front, header);
            front += 1;
        }
    }

    (headers, rows)
}
