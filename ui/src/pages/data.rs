use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use dioxus::prelude::*;
use serde_json::Value;

use crate::AppContext;
use crate::AppRoute;
use crate::api::{
    bulk_delete_many, bulk_delete_rows, delete_row, get_partition_details, get_rows, get_status,
    get_tables_list,
};
use crate::components::atoms::{Badge, BadgeTone, Icon, IconKind};
use crate::components::data::{
    PARTITION_KEY, PartitionsPane, ROW_KEY, RowDrawer, RowsTable, TIME_STAMP, TableHeader,
    TablePagination, TableToolbar, TablesPane,
};
use crate::models::{
    DEFAULT_NAMESPACE, PagedApiModel, PartitionMetricApiModel, StatusApiModel, TableApiModel,
    TableListItemApiModel,
};

#[derive(Clone)]
enum DialogState {
    DeleteOne {
        partition_key: String,
        row_key: String,
    },
    BulkDelete {
        partition_key: String,
        row_keys: Vec<String>,
    },
    PasteDelete {
        raw: String,
        parsed: Option<BTreeMap<String, Vec<String>>>,
        total_rows: usize,
        partitions_touched: usize,
        error: Option<String>,
    },
}

/// The rows window as the server is asked for it. The page number is part of
/// the identity now: `/api/Row` takes `skip`/`limit`, so turning a page is a
/// fetch and not a slice of something the browser already holds.
#[derive(Clone, PartialEq)]
struct RowsScope {
    table: String,
    partition: String,
    skip: usize,
    limit: usize,
}

struct DataState {
    tables: Vec<TableListItemApiModel>,
    tables_loaded: bool,
    /// Table whose partitions window is currently loaded or loading.
    loaded_for_table: Option<String>,
    /// The partitions window itself — keys with their own records count and
    /// size, straight out of `/api/Partitions/Details`.
    partitions: Option<Vec<PartitionMetricApiModel>>,
    /// Partitions the whole table holds, which is what the pane's pager counts.
    partitions_total: usize,
    /// Which window of them the pane is pointed at.
    partitions_page: usize,
    /// The rows window currently loaded or loading.
    loaded_rows_for: Option<RowsScope>,
    /// True once the rows for `loaded_rows_for` have actually arrived.
    rows_ready: bool,
    /// The (table, partition) `current_page` counts pages of. A page number
    /// belongs to the partition it was turned to, so selecting another one
    /// starts over — while a refresh of the same partition does not.
    rows_pair: Option<(String, String)>,
    headers: Vec<String>,
    rows: Vec<Value>,
    /// Rows the selected partition holds, as the partition-metrics poll last
    /// reported it. Kept on the state rather than looked up per render: the
    /// pane's window can page away from the selected partition, and losing the
    /// count would collapse its hundred pages into one.
    rows_total: Option<u64>,
    dialog: Option<DialogState>,
    /// Why the last write attempt from the open dialog failed — most often
    /// "write access is disabled". Rendered inside the dialog, since a
    /// rejected delete otherwise looks like the button did nothing.
    write_error: Option<String>,
    checked_keys: HashSet<String>,
    page_size: usize,
    current_page: usize,
}

impl Default for DataState {
    fn default() -> Self {
        Self {
            tables: Vec::new(),
            tables_loaded: false,
            loaded_for_table: None,
            partitions: None,
            partitions_total: 0,
            partitions_page: 0,
            loaded_rows_for: None,
            rows_ready: false,
            rows_pair: None,
            headers: Vec::new(),
            rows: Vec::new(),
            rows_total: None,
            dialog: None,
            write_error: None,
            checked_keys: HashSet::new(),
            page_size: DEFAULT_PAGE_SIZE,
            current_page: 0,
        }
    }
}

const DEFAULT_PAGE_SIZE: usize = 100;

/// Partitions per window of the pane. A table can hold a million of them and
/// every one in the window costs the server a measurement, so this is both what
/// the pane can show and what the poll below repeats every three seconds.
const PARTITIONS_PAGE_SIZE: usize = 100;

impl DataState {
    fn set_tables(&mut self, tables: Vec<TableListItemApiModel>) {
        self.tables = tables;
        self.tables_loaded = true;
    }

    fn mark_tables_loaded(&mut self) {
        self.tables_loaded = true;
    }

    /// Start loading the partitions window for `table`; invalidates any rows.
    fn begin_table_load(&mut self, table: &str) {
        self.loaded_for_table = Some(table.to_string());
        self.partitions = None;
        self.partitions_total = 0;
        self.partitions_page = 0;
        self.loaded_rows_for = None;
        self.rows_ready = false;
        self.rows_pair = None;
        self.headers = Vec::new();
        self.rows = Vec::new();
        self.rows_total = None;
        self.checked_keys.clear();
        self.current_page = 0;
    }

    /// Point the pane at another window. The keys of the previous one go with
    /// it: they would otherwise sit under the new page's pager until the answer
    /// arrives, which reads as a page that did not turn.
    fn set_partitions_page(&mut self, page: usize) {
        self.partitions_page = page;
        self.partitions = None;
    }

    /// Apply a fetched partitions window, unless the table or the page moved
    /// meanwhile — both the poll and a page turn fetch, and an answer that
    /// belongs to neither is an answer about something the pane left behind.
    fn set_partition_details(
        &mut self,
        table: &str,
        page: usize,
        answer: PagedApiModel<PartitionMetricApiModel>,
    ) {
        if self.loaded_for_table.as_deref() != Some(table) || self.partitions_page != page {
            return;
        }

        let selected_partition = match self.rows_pair.as_ref() {
            Some((pair_table, partition)) if pair_table == table => Some(partition.clone()),
            _ => None,
        };

        self.partitions_total = answer.amount as usize;
        self.partitions = Some(answer.data);

        // The rows pager's total, whenever the window happens to carry the
        // selected partition. `/api/Row` answers with a bare array — by
        // decision, so a single row is not a different shape — so this is where
        // the count comes from.
        if let Some(partition) = selected_partition {
            let records = self.partitions.as_ref().and_then(|window| {
                window
                    .iter()
                    .find(|metric| metric.partition_key == partition)
                    .map(|metric| metric.records_count)
            });
            if let Some(records) = records {
                self.rows_total = Some(records);
            }
        }
    }

    /// Start loading a rows window; clears the previous rows.
    fn begin_rows_load(&mut self, scope: RowsScope) {
        let pair = (scope.table.clone(), scope.partition.clone());
        if self.rows_pair.as_ref() != Some(&pair) {
            self.rows_pair = Some(pair);
            self.current_page = 0;
            self.rows_total = None;
        }
        self.loaded_rows_for = Some(scope);
        self.rows_ready = false;
        self.headers = Vec::new();
        self.rows = Vec::new();
        // Ticks belong to the window: the bulk bar can only offer what the page
        // shows, and a tick left behind on a page the user has walked off would
        // be deleted without ever being seen again.
        self.checked_keys.clear();
    }

    fn set_page(&mut self, page: usize) {
        self.current_page = page;
    }

    fn set_page_size(&mut self, size: usize) {
        self.page_size = size.max(1);
        self.current_page = 0;
    }

    /// Apply fetched rows, unless the window changed meanwhile.
    fn set_rows(&mut self, scope: &RowsScope, rows: Vec<Value>) {
        if self.loaded_rows_for.as_ref() != Some(scope) {
            return;
        }
        let (headers, rows) = build_rows_state(rows);
        self.headers = headers;
        self.rows = rows;
        self.rows_ready = true;
    }

    /// Force the next render to refetch the current rows window. `rows_pair` is
    /// left alone on purpose — a refresh stays on the page it was pressed on.
    fn clear_rows_scope(&mut self) {
        self.loaded_rows_for = None;
        self.rows_ready = false;
    }

    /// Opens a confirm dialog. Clears any error left over from a previous
    /// attempt, so a reopened dialog never shows a stale rejection.
    fn open_dialog(&mut self, dialog: DialogState) {
        self.dialog = Some(dialog);
        self.write_error = None;
    }

    fn close_dialog(&mut self) {
        self.dialog = None;
        self.write_error = None;
    }

    fn set_write_error(&mut self, err: String) {
        self.write_error = Some(err);
    }
}

/// Extract `(table, partition, row)` from the current data route.
fn parse_data_route(route: &AppRoute) -> (Option<String>, Option<String>, Option<String>) {
    match route {
        AppRoute::DataTable { table } => (Some(table.clone()), None, None),
        AppRoute::DataPartition { table, partition } => {
            (Some(table.clone()), Some(partition.clone()), None)
        }
        AppRoute::DataRow {
            table,
            partition,
            row,
        } => (
            Some(table.clone()),
            Some(partition.clone()),
            Some(row.clone()),
        ),
        _ => (None, None, None),
    }
}

// Route placeholders — the URL patterns for the data section. `DataLayout`
// renders the whole page and reads the params via `use_route`, so these render
// nothing themselves.
#[component]
pub fn Data() -> Element {
    rsx! {}
}

#[component]
pub fn DataTable(table: String) -> Element {
    let _ = table;
    rsx! {}
}

#[component]
pub fn DataPartition(table: String, partition: String) -> Element {
    let _ = (table, partition);
    rsx! {}
}

#[component]
pub fn DataRow(table: String, partition: String, row: String) -> Element {
    let _ = (table, partition, row);
    rsx! {}
}

#[component]
pub fn DataLayout() -> Element {
    let mut cs = use_signal(DataState::default);
    let row_filter = use_signal(String::new);
    let app_ctx = use_context::<Signal<AppContext>>();
    let nav = navigator();

    let route = use_route::<AppRoute>();
    let (url_table, url_partition, url_row) = parse_data_route(&route);

    // ---- one-time tables list load ----
    use_effect(move || {
        spawn(async move {
            if cs.peek().tables_loaded {
                return;
            }
            match get_tables_list().await {
                Ok(list) => {
                    let mut sorted = list;
                    sorted.sort_by(|a, b| a.name.cmp(&b.name));
                    cs.write().set_tables(sorted);
                }
                Err(err) => {
                    dioxus_utils::console_log(format!("Tables error: {}", err));
                    cs.write().mark_tables_loaded();
                }
            }
        });
    });

    // ---- background status refresh: keeps the selected table's header, its
    // readers and its open transactions live while the data section is mounted.
    // Polls every 3s. ----
    let mut status_started = use_signal(|| false);
    use_effect(move || {
        if *status_started.peek() {
            return;
        }
        status_started.set(true);
        let mut ctx = app_ctx;
        spawn(async move {
            loop {
                match get_status().await {
                    Ok(s) => ctx.write().status = Some(s),
                    Err(err) => {
                        dioxus_utils::console_log(format!("Status error: {}", err));
                    }
                }
                dioxus_utils::js::sleep(Duration::from_secs(3)).await;
            }
        });
    });

    // ---- which rows window to ask for. Settled before anything renders,
    // because the window is part of the request now rather than of the render:
    // the page number, the page size and the partition together are what
    // `/api/Row` is called with. ----
    let (page_size, stored_page, rows_total, page_belongs_here) = {
        let state = cs.read();
        let belongs = match (&url_table, &url_partition, &state.rows_pair) {
            (Some(table), Some(partition), Some((pair_table, pair_partition))) => {
                table == pair_table && partition == pair_partition
            }
            _ => false,
        };
        (
            state.page_size,
            state.current_page,
            state.rows_total,
            belongs,
        )
    };

    // A page number counted for another partition means nothing here, so a
    // freshly selected one starts at its first page. Derived rather than reset
    // in place — nothing writes to the state while rendering.
    let stored_page = if page_belongs_here { stored_page } else { 0 };
    let current_page = match rows_total {
        Some(total) => {
            let total_pages = (total as usize).div_ceil(page_size).max(1);
            stored_page.min(total_pages - 1)
        }
        // Nothing has said how many rows there are, so there is nothing to
        // clamp against: the window the server answers with is the only
        // evidence, and the pager reads it.
        None => stored_page,
    };
    let page_start = current_page * page_size;

    // ---- load + live-refresh the partitions window for the URL table. A
    // per-table loop fetches once immediately and then re-polls every 3s so the
    // record counts and byte sizes stay current. It self-terminates when the
    // selected table changes (a fresh render spawns a new loop for the new
    // table), and it reads the page off the state on every pass, so a turned
    // page keeps being refreshed rather than the one it was spawned on. ----
    if let Some(table) = url_table.clone() {
        let already = { cs.read().loaded_for_table.as_deref() == Some(table.as_str()) };
        if !already {
            let url_partition_at_nav = url_partition.clone();
            spawn(async move {
                if cs.peek().loaded_for_table.as_deref() == Some(table.as_str()) {
                    return;
                }
                cs.write().begin_table_load(&table);
                let mut first = true;
                loop {
                    if cs.peek().loaded_for_table.as_deref() != Some(table.as_str()) {
                        break;
                    }
                    let page = cs.peek().partitions_page;
                    match get_partition_details(
                        &table,
                        Some(page * PARTITIONS_PAGE_SIZE),
                        Some(PARTITIONS_PAGE_SIZE),
                    )
                    .await
                    {
                        Ok(answer) => {
                            // Table with a single partition — jump straight into
                            // it on first load. `replace` so Back skips this
                            // step. `amount` counts the whole table, so this is
                            // "one partition", not "one on this page".
                            let only = if answer.amount == 1 {
                                answer.data.first().map(|d| d.partition_key.clone())
                            } else {
                                None
                            };
                            cs.write().set_partition_details(&table, page, answer);
                            if first
                                && url_partition_at_nav.is_none()
                                && let Some(only) = only
                            {
                                nav.replace(AppRoute::DataPartition {
                                    table: table.clone(),
                                    partition: only,
                                });
                            }
                        }
                        Err(err) => {
                            dioxus_utils::console_log(format!("Partitions error: {}", err));
                        }
                    }
                    first = false;
                    dioxus_utils::js::sleep(Duration::from_secs(3)).await;
                }
            });
        }
    }

    // ---- load the rows window whenever the URL (table, partition), the page
    // or the page size changes ----
    if let (Some(table), Some(partition)) = (url_table.clone(), url_partition.clone()) {
        let scope = RowsScope {
            table,
            partition,
            skip: page_start,
            limit: page_size,
        };
        let already = { cs.read().loaded_rows_for.as_ref() == Some(&scope) };
        if !already {
            spawn(async move {
                if cs.peek().loaded_rows_for.as_ref() == Some(&scope) {
                    return;
                }
                cs.write().begin_rows_load(scope.clone());
                match get_rows(
                    &scope.table,
                    &scope.partition,
                    Some(scope.skip),
                    Some(scope.limit),
                )
                .await
                {
                    Ok(rows) => cs.write().set_rows(&scope, rows),
                    Err(err) => {
                        dioxus_utils::console_log(format!("Rows error: {}", err));
                    }
                }
            });
        }
    }

    // ---- read state for rendering ----
    let cs_ra = cs.read();
    let tables = cs_ra.tables.clone();

    let partitions_window: Vec<PartitionMetricApiModel> =
        match (&url_table, &cs_ra.loaded_for_table, &cs_ra.partitions) {
            (Some(t), Some(lt), Some(window)) if t == lt => window.clone(),
            _ => Vec::new(),
        };
    let partitions_total = match (&url_table, &cs_ra.loaded_for_table) {
        (Some(t), Some(lt)) if t == lt => cs_ra.partitions_total,
        _ => 0,
    };
    let partitions_page = cs_ra.partitions_page;

    // The whole window has to match, the page size included: a size change
    // lands on page 1 too, and the rows of the previous size are not the rows of
    // this one.
    let rows_scope_matches = match (&url_table, &url_partition, &cs_ra.loaded_rows_for) {
        (Some(t), Some(p), Some(scope)) => {
            t == &scope.table
                && p == &scope.partition
                && scope.skip == page_start
                && scope.limit == page_size
        }
        _ => false,
    };
    let rows_ready = rows_scope_matches && cs_ra.rows_ready;
    let headers = if rows_scope_matches {
        cs_ra.headers.clone()
    } else {
        Vec::new()
    };
    let all_rows = if rows_scope_matches {
        cs_ra.rows.clone()
    } else {
        Vec::new()
    };
    let checked_keys = cs_ra.checked_keys.clone();
    let dialog_val = cs_ra.dialog.clone();
    let write_error = cs_ra.write_error.clone();
    drop(cs_ra);

    let selected_table = url_table.clone().unwrap_or_default();

    // Derive the selected table's header, its readers and its open transactions
    // from the status poll. Everything here is namespace-scoped: `/api/Status`
    // answers about every namespace at once, and a table name only means
    // something inside one of them.
    let ctx_ra = app_ctx.read();
    let status: Option<StatusApiModel> = ctx_ra.status.clone();
    drop(ctx_ra);

    // Named rather than left to the status answer's own first entry: the api
    // layer sends no `ns` header while nothing is selected, and the server then
    // works in the default namespace — so that is the namespace whose tables,
    // rows and readers this page is actually looking at.
    let ns_name = crate::storage::load_namespace().unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
    let namespace = status
        .as_ref()
        .and_then(|s| s.namespace(Some(ns_name.as_str())));

    let writing_tables = build_writing_tables(&status, &ns_name);
    let transaction_tags = derive_table_transactions(&status, &ns_name, &selected_table);
    let reader_count_for_selected = derive_table_readers(&status, &ns_name, &selected_table);
    let table_stats: Option<TableApiModel> =
        namespace.and_then(|n| n.tables.iter().find(|t| t.name == selected_table).cloned());

    // The filter narrows the window, not the table: the rows arrive a page at a
    // time now, so the pager below keeps counting the partition while this
    // counts what is left of the page.
    let filter_str = row_filter.read().to_lowercase();
    let filtered_rows: Vec<Value> = if filter_str.is_empty() {
        all_rows.clone()
    } else {
        all_rows
            .iter()
            .filter(|row| row.to_string().to_lowercase().contains(&filter_str))
            .cloned()
            .collect()
    };

    let visible_keys: Vec<String> = filtered_rows
        .iter()
        .filter_map(|r| r.get(ROW_KEY).and_then(|v| v.as_str().map(String::from)))
        .collect();

    // The drawer carries only the row key in the URL — resolve the full row out
    // of the loaded window.
    let resolved_row: Option<Value> = url_row.as_ref().and_then(|rk| {
        all_rows
            .iter()
            .find(|r| r.get(ROW_KEY).and_then(|v| v.as_str()) == Some(rk.as_str()))
            .cloned()
    });

    // ---- navigation handlers ----
    let select_table = move |name: String| {
        nav.push(AppRoute::DataTable { table: name });
    };

    let select_partition = {
        let table = url_table.clone();
        move |pk: String| {
            if let Some(table) = table.clone() {
                nav.push(AppRoute::DataPartition {
                    table,
                    partition: pk,
                });
            }
        }
    };

    // A page turn in the pane fetches at once rather than waiting for the poll
    // to come round: the list is dropped the moment the page changes, and a
    // pane that stays empty for three seconds looks broken.
    let turn_partitions_page = {
        let table = url_table.clone();
        move |page: usize| {
            let Some(table) = table.clone() else {
                return;
            };
            cs.write().set_partitions_page(page);
            spawn(async move {
                match get_partition_details(
                    &table,
                    Some(page * PARTITIONS_PAGE_SIZE),
                    Some(PARTITIONS_PAGE_SIZE),
                )
                .await
                {
                    Ok(answer) => cs.write().set_partition_details(&table, page, answer),
                    Err(err) => {
                        dioxus_utils::console_log(format!("Partitions error: {}", err));
                    }
                }
            });
        }
    };

    let on_row_click = {
        let table = url_table.clone();
        let partition = url_partition.clone();
        move |row: Value| {
            let (Some(table), Some(partition)) = (table.clone(), partition.clone()) else {
                return;
            };
            let rk = row
                .get(ROW_KEY)
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            nav.push(AppRoute::DataRow {
                table,
                partition,
                row: rk,
            });
        }
    };

    let close_drawer = {
        let table = url_table.clone();
        let partition = url_partition.clone();
        move |_| {
            if let (Some(table), Some(partition)) = (table.clone(), partition.clone()) {
                nav.push(AppRoute::DataPartition { table, partition });
            }
        }
    };

    let confirm_delete = {
        let table = url_table.clone();
        let partition = url_partition.clone();
        move |_| {
            let dialog_val = cs.read().dialog.clone();
            let Some(table_name) = table.clone() else {
                return;
            };
            match dialog_val {
                Some(DialogState::DeleteOne {
                    partition_key,
                    row_key,
                }) => {
                    let back_partition = partition.clone();
                    spawn(async move {
                        if let Err(err) = delete_row(&table_name, &partition_key, &row_key).await {
                            cs.write().set_write_error(err.to_string());
                            return;
                        }
                        {
                            let mut w = cs.write();
                            w.close_dialog();
                            w.checked_keys.remove(&row_key);
                            w.clear_rows_scope();
                        }
                        // Drop the row segment so the drawer closes.
                        if let Some(partition) = back_partition {
                            nav.push(AppRoute::DataPartition {
                                table: table_name.clone(),
                                partition,
                            });
                        }
                    });
                }
                Some(DialogState::BulkDelete {
                    partition_key,
                    row_keys,
                }) => {
                    spawn(async move {
                        if let Err(err) =
                            bulk_delete_rows(&table_name, &partition_key, &row_keys).await
                        {
                            cs.write().set_write_error(err.to_string());
                            return;
                        }
                        let mut w = cs.write();
                        w.close_dialog();
                        for rk in &row_keys {
                            w.checked_keys.remove(rk);
                        }
                        w.clear_rows_scope();
                    });
                }
                Some(DialogState::PasteDelete {
                    parsed: Some(grouped),
                    ..
                }) => {
                    spawn(async move {
                        if let Err(err) = bulk_delete_many(&table_name, &grouped).await {
                            cs.write().set_write_error(err.to_string());
                            return;
                        }
                        let mut w = cs.write();
                        w.close_dialog();
                        w.checked_keys.clear();
                        w.clear_rows_scope();
                    });
                }
                Some(DialogState::PasteDelete { parsed: None, .. }) => {}
                None => {}
            }
        }
    };

    let toggle_row_check = move |rk: String| {
        let mut w = cs.write();
        if w.checked_keys.contains(&rk) {
            w.checked_keys.remove(&rk);
        } else {
            w.checked_keys.insert(rk);
        }
    };

    let toggle_all_check = {
        let visible_keys = visible_keys.clone();
        move |check_all: bool| {
            let mut w = cs.write();
            if check_all {
                for k in &visible_keys {
                    w.checked_keys.insert(k.clone());
                }
            } else {
                for k in &visible_keys {
                    w.checked_keys.remove(k);
                }
            }
        }
    };

    let center_content = if url_table.is_none() {
        render_empty_state(tables.clone(), select_table)
    } else {
        let on_refresh_table = move |_| {
            cs.write().clear_rows_scope();
        };
        let on_export_click = {
            let table = url_table.clone();
            let pk_opt = url_partition.clone();
            move |_| {
                let (Some(table), Some(pk)) = (table.clone(), pk_opt.clone()) else {
                    return;
                };
                let url = crate::api::download_rows_url(&table, &pk);
                let script = format!(
                    "window.location.href = {};",
                    serde_json::to_string(&url).unwrap_or_else(|_| "\"\"".to_string())
                );
                let _ = dioxus::document::eval(&script);
            }
        };
        let export_enabled = url_partition.is_some();
        let checked_in_partition: Vec<String> = visible_keys
            .iter()
            .filter(|k| checked_keys.contains(*k))
            .cloned()
            .collect();
        let checked_count = checked_in_partition.len();

        let bulk_bar = if checked_count > 0 {
            let pk_opt = url_partition.clone();
            let keys_for_delete = checked_in_partition.clone();
            rsx! {
                div { class: "bulk-bar",
                    span { class: "bulk-bar__count", "{checked_count} selected" }
                    div { class: "bulk-bar__spacer" }
                    button {
                        class: "btn btn--ghost btn--sm",
                        onclick: move |_| { cs.write().checked_keys.clear(); },
                        "Clear"
                    }
                    button {
                        class: "btn btn--danger btn--sm",
                        onclick: move |_| {
                            let Some(pk) = pk_opt.clone() else { return };
                            cs.write().open_dialog(DialogState::BulkDelete {
                                partition_key: pk,
                                row_keys: keys_for_delete.clone(),
                            });
                        },
                        "Delete selected"
                    }
                }
            }
        } else {
            rsx! {}
        };

        rsx! {
            div { class: "rows-col",
                TableHeader {
                    name: selected_table.clone(),
                    stats: table_stats.clone(),
                    on_refresh: on_refresh_table,
                }
                TableToolbar {
                    filter_value: row_filter,
                    transaction_tags: transaction_tags.clone(),
                    reader_count: reader_count_for_selected,
                    on_export: on_export_click,
                    export_enabled,
                    on_paste_delete: move |_| {
                        cs.write().open_dialog(DialogState::PasteDelete {
                            raw: String::new(),
                            parsed: None,
                            total_rows: 0,
                            partitions_touched: 0,
                            error: None,
                        });
                    },
                    paste_enabled: true,
                }
                {bulk_bar}
                RowsTable {
                    headers: headers.clone(),
                    rows: filtered_rows.clone(),
                    selected_row_key: url_row.clone(),
                    on_row_click,
                    selectable: true,
                    checked_keys: checked_keys.clone(),
                    on_toggle_row: toggle_row_check,
                    on_toggle_all: toggle_all_check,
                }
                TablePagination {
                    total: rows_total.map(|total| total as usize),
                    loaded: all_rows.len(),
                    page_size,
                    current_page,
                    on_page_change: move |p: usize| { cs.write().set_page(p); },
                    on_page_size_change: move |sz: usize| { cs.write().set_page_size(sz); },
                }
            }
        }
    };

    let partitions_content = if url_table.is_none() {
        rsx! { aside { class: "partitions-pane" } }
    } else {
        rsx! {
            PartitionsPane {
                partitions: partitions_window,
                total: partitions_total,
                page: partitions_page,
                page_size: PARTITIONS_PAGE_SIZE,
                selected: url_partition.clone(),
                on_select: select_partition,
                on_page_change: turn_partitions_page,
            }
        }
    };

    let drawer_content = match url_row.as_ref() {
        None => rsx! {},
        Some(rk) => {
            if !rows_ready {
                rsx! {
                    DrawerMessage {
                        title: "Loading row…".to_string(),
                        message: "Fetching partition rows…".to_string(),
                        on_close: close_drawer,
                    }
                }
            } else if let Some(row) = resolved_row.clone() {
                let pk_val = row
                    .get(PARTITION_KEY)
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                let rk_val = row
                    .get(ROW_KEY)
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                rsx! {
                    RowDrawer {
                        row,
                        on_close: close_drawer,
                        on_delete: move |_| {
                            cs.write().open_dialog(DialogState::DeleteOne {
                                partition_key: pk_val.clone(),
                                row_key: rk_val.clone(),
                            });
                        },
                    }
                }
            } else {
                // The rows come a page at a time, so a row key the URL names may
                // be a real row sitting on another one. Saying so beats
                // "deleted".
                rsx! {
                    DrawerMessage {
                        title: "Row not on this page".to_string(),
                        message: format!(
                            "No row with key \"{}\" in the loaded page of this partition.",
                            rk,
                        ),
                        on_close: close_drawer,
                    }
                }
            }
        }
    };

    // A rejected write (most often "write access is disabled") is shown inside
    // the open dialog — the dialog deliberately stays open so the user can
    // enable write access and retry without re-selecting the rows.
    let write_error_line = match write_error.as_ref() {
        Some(msg) => rsx! { div { class: "dialog__error", "{msg}" } },
        None => rsx! {},
    };

    let dialog_render = match dialog_val {
        Some(DialogState::DeleteOne {
            partition_key,
            row_key,
        }) => rsx! {
            div { class: "dialog-overlay",
                div { class: "dialog",
                    div { class: "dialog__header", "Confirm delete" }
                    div { class: "dialog__body",
                        "Delete row "
                        b { "{row_key}" }
                        " from partition "
                        b { "{partition_key}" }
                        "?"
                        {write_error_line.clone()}
                    }
                    div { class: "dialog__footer",
                        button {
                            class: "btn btn--ghost btn--sm",
                            onclick: move |_| { cs.write().close_dialog(); },
                            "Cancel"
                        }
                        button { class: "btn btn--danger btn--sm", onclick: confirm_delete.clone(),
                            "Delete"
                        }
                    }
                }
            }
        },
        Some(DialogState::BulkDelete {
            partition_key,
            row_keys,
        }) => {
            let total = row_keys.len();
            const PREVIEW_LIMIT: usize = 50;
            let preview: Vec<String> = row_keys.iter().take(PREVIEW_LIMIT).cloned().collect();
            let extra = total.saturating_sub(preview.len());
            let items = preview.into_iter().map(|k| {
                rsx! {
                    div { class: "dialog__list-item", "{k}" }
                }
            });
            let extra_line = if extra > 0 {
                rsx! { div { class: "dialog__list-extra", "+ {extra} more" } }
            } else {
                rsx! {}
            };
            rsx! {
                div { class: "dialog-overlay",
                    div { class: "dialog",
                        div { class: "dialog__header", "Confirm bulk delete" }
                        div { class: "dialog__body",
                            "Delete "
                            b { "{total}" }
                            " row(s) from partition "
                            b { "{partition_key}" }
                            "?"
                            div { class: "dialog__list",
                                {items}
                                {extra_line}
                            }
                            {write_error_line.clone()}
                        }
                        div { class: "dialog__footer",
                            button {
                                class: "btn btn--ghost btn--sm",
                                onclick: move |_| { cs.write().close_dialog(); },
                                "Cancel"
                            }
                            button { class: "btn btn--danger btn--sm", onclick: confirm_delete.clone(),
                                "Delete {total} row(s)"
                            }
                        }
                    }
                }
            }
        }
        Some(DialogState::PasteDelete {
            raw,
            parsed,
            total_rows,
            partitions_touched,
            error,
        }) => {
            let parse_ready = parsed.is_some();
            let total = total_rows;
            let partitions_n = partitions_touched;
            let error_text = error.clone();
            let target_table = selected_table.clone();

            let on_textarea_input = move |evt: dioxus::events::FormEvent| {
                let new_raw = evt.value();
                let mut w = cs.write();
                if let Some(DialogState::PasteDelete {
                    raw: r,
                    parsed: p,
                    total_rows: t,
                    partitions_touched: pt,
                    error: e,
                }) = w.dialog.as_mut()
                {
                    *r = new_raw;
                    *p = None;
                    *t = 0;
                    *pt = 0;
                    *e = None;
                }
            };

            let on_parse_click = move |_| {
                let raw_snapshot = {
                    let cs_ra = cs.read();
                    match cs_ra.dialog.as_ref() {
                        Some(DialogState::PasteDelete { raw, .. }) => raw.clone(),
                        _ => return,
                    }
                };
                match parse_paste_delete_input(&raw_snapshot) {
                    Ok((grouped, total, partitions_touched)) => {
                        let mut w = cs.write();
                        if let Some(DialogState::PasteDelete {
                            parsed,
                            total_rows,
                            partitions_touched: pt_field,
                            error,
                            ..
                        }) = w.dialog.as_mut()
                        {
                            *parsed = Some(grouped);
                            *total_rows = total;
                            *pt_field = partitions_touched;
                            *error = None;
                        }
                    }
                    Err(msg) => {
                        let mut w = cs.write();
                        if let Some(DialogState::PasteDelete {
                            parsed,
                            total_rows,
                            partitions_touched: pt_field,
                            error,
                            ..
                        }) = w.dialog.as_mut()
                        {
                            *parsed = None;
                            *total_rows = 0;
                            *pt_field = 0;
                            *error = Some(msg);
                        }
                    }
                }
            };

            let preview_items: Vec<(String, String)> = parsed
                .as_ref()
                .map(|g| {
                    const PREVIEW_LIMIT: usize = 50;
                    let mut out = Vec::new();
                    'outer: for (pk, rks) in g.iter() {
                        for rk in rks {
                            out.push((pk.clone(), rk.clone()));
                            if out.len() >= PREVIEW_LIMIT {
                                break 'outer;
                            }
                        }
                    }
                    out
                })
                .unwrap_or_default();
            let preview_shown = preview_items.len();
            let preview_extra = total.saturating_sub(preview_shown);
            let preview_items_render = preview_items.into_iter().map(|(pk, rk)| {
                rsx! {
                    div { class: "dialog__list-item", "pk={pk} / rk={rk}" }
                }
            });
            let preview_extra_line = if preview_extra > 0 {
                rsx! { div { class: "dialog__list-extra", "+ {preview_extra} more" } }
            } else {
                rsx! {}
            };

            let placeholder_text: &'static str = "[\n  { \"PartitionKey\": \"a\", \"RowKey\": \"1\" },\n  { \"PartitionKey\": \"b\", \"RowKey\": \"5\" }\n]";

            let error_render = if let Some(msg) = error_text {
                rsx! {
                    div { class: "dialog__error", "{msg}" }
                }
            } else {
                rsx! {}
            };

            let summary_render = if parse_ready {
                rsx! {
                    div { style: "margin: 6px 0;",
                        "Will delete "
                        b { "{total}" }
                        " row(s) across "
                        b { "{partitions_n}" }
                        " partition(s)"
                    }
                }
            } else {
                rsx! {}
            };

            rsx! {
                div { class: "dialog-overlay",
                    div { class: "dialog",
                        div { class: "dialog__header", "Paste & delete" }
                        div { class: "dialog__body",
                            div { style: "margin-bottom: 6px;",
                                "Target table: "
                                b { "{target_table}" }
                            }
                            div { style: "margin-bottom: 6px; font-size: 12px; color: gray;",
                                "Paste a JSON array of objects with "
                                code { "PartitionKey" }
                                " and "
                                code { "RowKey" }
                                " fields."
                            }
                            textarea {
                                value: "{raw}",
                                oninput: on_textarea_input,
                                rows: "10",
                                style: "width: 100%; font-family: monospace; font-size: 12px;",
                                placeholder: placeholder_text,
                            }
                            {error_render}
                            {write_error_line.clone()}
                            {summary_render}
                            div { class: "dialog__list",
                                {preview_items_render}
                                {preview_extra_line}
                            }
                        }
                        div { class: "dialog__footer",
                            button {
                                class: "btn btn--ghost btn--sm",
                                onclick: move |_| { cs.write().close_dialog(); },
                                "Cancel"
                            }
                            button {
                                class: "btn btn--sm",
                                onclick: on_parse_click,
                                "Parse"
                            }
                            button {
                                class: "btn btn--danger btn--sm",
                                disabled: !parse_ready,
                                onclick: confirm_delete.clone(),
                                "Delete {total} row(s)"
                            }
                        }
                    }
                }
            }
        }
        None => rsx! {},
    };

    let data_cls = if url_row.is_some() {
        "data"
    } else {
        "data data--no-drawer"
    };

    rsx! {
        section { class: "page page--flush",
            div { class: data_cls,
                TablesPane {
                    tables: tables.clone(),
                    selected: selected_table.clone(),
                    writing_tables,
                    on_select: select_table,
                }
                {partitions_content}
                {center_content}
                {drawer_content}
            }
            {dialog_render}
            Outlet::<AppRoute> {}
        }
    }
}

/// A minimal row drawer used while rows are still loading or when the URL
/// points at a row key that the loaded page does not carry.
#[component]
fn DrawerMessage(title: String, message: String, on_close: EventHandler<()>) -> Element {
    rsx! {
        aside { class: "row-drawer",
            div { class: "row-drawer__header",
                span { class: "row-drawer__title", "Row Detail" }
                button {
                    class: "topbar__icon-btn",
                    onclick: move |_| on_close.call(()),
                    Icon { kind: IconKind::X }
                }
            }
            div { class: "row-drawer__body",
                div { class: "empty-state",
                    div { class: "empty-state__title", "{title}" }
                    div { class: "empty-state__sub", "{message}" }
                }
            }
        }
    }
}

/// What a parsed paste-and-delete input amounts to: the row keys grouped by
/// partition, how many there are of them, and how many partitions they touch —
/// the last two being what the dialog states before anything is deleted.
type PasteDeletePlan = (BTreeMap<String, Vec<String>>, usize, usize);

fn parse_paste_delete_input(raw: &str) -> Result<PasteDeletePlan, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Input is empty.".to_string());
    }

    let parsed: Value =
        serde_json::from_str(trimmed).map_err(|err| format!("Invalid JSON: {}", err))?;

    let arr = parsed
        .as_array()
        .ok_or_else(|| "Top-level JSON must be an array.".to_string())?;

    if arr.is_empty() {
        return Err("Array is empty.".to_string());
    }

    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total: usize = 0;

    for (idx, item) in arr.iter().enumerate() {
        let obj = item.as_object().ok_or_else(|| {
            format!(
                "Item #{} is not an object (expected {{ PartitionKey, RowKey }}).",
                idx
            )
        })?;

        let pk = obj
            .get(PARTITION_KEY)
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("Item #{} is missing a string \"PartitionKey\" field.", idx))?;
        let rk = obj
            .get(ROW_KEY)
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("Item #{} is missing a string \"RowKey\" field.", idx))?;

        if pk.is_empty() {
            return Err(format!("Item #{}: PartitionKey is empty.", idx));
        }
        if rk.is_empty() {
            return Err(format!("Item #{}: RowKey is empty.", idx));
        }

        grouped
            .entry(pk.to_string())
            .or_default()
            .push(rk.to_string());
        total += 1;
    }

    let partitions_touched = grouped.len();
    Ok((grouped, total, partitions_touched))
}

fn build_rows_state(rows: Vec<Value>) -> (Vec<String>, Vec<Value>) {
    let mut headers: Vec<String> = vec![
        PARTITION_KEY.to_string(),
        ROW_KEY.to_string(),
        TIME_STAMP.to_string(),
    ];

    for row in rows.iter() {
        if let Value::Object(map) = row {
            for key in map.keys() {
                if key == PARTITION_KEY || key == ROW_KEY || key == TIME_STAMP {
                    continue;
                }
                if !headers.iter().any(|h| h == key) {
                    headers.push(key.clone());
                }
            }
        }
    }

    (headers, rows)
}

/// Tables an open transaction is aimed at, in the namespace the UI is pointed
/// at. This is what lights the dot in the tables pane: there are no connected
/// writers to light it with here — a write is a unary call that is over by the
/// moment it could be listed — and a transaction is the one piece of writing
/// the server holds open long enough to name.
fn build_writing_tables(status: &Option<StatusApiModel>, namespace: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Some(s) = status {
        for transaction in s.transactions.iter() {
            if transaction.namespace == namespace {
                set.insert(transaction.table.clone());
            }
        }
    }
    set
}

/// One pill per open transaction on this table: who is writing to it right now,
/// as closely as this server can answer that.
fn derive_table_transactions(
    status: &Option<StatusApiModel>,
    namespace: &str,
    table: &str,
) -> Vec<String> {
    let mut tags = Vec::new();
    if let Some(s) = status {
        for transaction in s.transactions.iter() {
            if transaction.namespace != namespace || transaction.table != table {
                continue;
            }
            // The id is a token, not something to read: the first bytes are
            // enough to tell two transactions apart, and the actions count is
            // what says how much is waiting in this one.
            let short_id: String = transaction.id.chars().take(8).collect();
            tags.push(format!("tx {} · {} actions", short_id, transaction.actions));
        }
    }
    tags
}

fn derive_table_readers(status: &Option<StatusApiModel>, namespace: &str, table: &str) -> usize {
    match status {
        Some(s) => s
            .readers
            .iter()
            .filter(|reader| reader.namespace == namespace)
            .filter(|reader| reader.tables.iter().any(|t| t == table))
            .count(),
        None => 0,
    }
}

fn render_empty_state(
    tables: Vec<TableListItemApiModel>,
    on_pick: impl FnMut(String) + Clone + 'static,
) -> Element {
    let chips = tables.into_iter().take(8).map(|t| {
        let name = t.name.clone();
        let mut on_pick = on_pick.clone();
        rsx! {
            button {
                class: "btn btn--sm",
                onclick: move |_| on_pick(name.clone()),
                Badge { text: t.name.clone(), tone: BadgeTone::Neutral }
            }
        }
    });

    rsx! {
        div { class: "rows-col",
            div { class: "empty-state",
                div { class: "empty-state__icon",
                    Icon { kind: IconKind::Layers }
                }
                div { class: "empty-state__title", "Select a table to begin" }
                div { class: "empty-state__sub", "Choose a table from the left, or pick one of the recently active tables below." }
                div { class: "empty-state__chips", {chips} }
            }
        }
    }
}
