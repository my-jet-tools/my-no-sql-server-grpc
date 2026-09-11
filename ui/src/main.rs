use dioxus::prelude::*;

mod api;
mod components;
mod models;
mod pages;
mod settings;
mod storage;
mod utils;

use components::shell::{Crumb, Sidebar, SidebarSection, Topbar};
use models::StatusApiModel;
use pages::*;
use settings::HealthThresholds;

#[derive(Routable, PartialEq, Clone)]
pub enum AppRoute {
    #[layout(Shell)]
    #[route("/")]
    Home {},
    #[layout(DataLayout)]
    #[route("/data")]
    Data {},
    #[route("/data/:table")]
    DataTable { table: String },
    #[route("/data/:table/:partition")]
    DataPartition { table: String, partition: String },
    #[route("/data/:table/:partition/:row")]
    DataRow {
        table: String,
        partition: String,
        row: String,
    },
    #[end_layout]
    #[route("/connections")]
    Connections {},
    #[layout(SnapshotsLayout)]
    #[route("/snapshots")]
    Snapshots {},
    #[route("/snapshots/:file")]
    SnapshotFile { file: String },
    #[route("/snapshots/:file/:table")]
    SnapshotTable { file: String, table: String },
    #[route("/snapshots/:file/:table/:partition")]
    SnapshotPartition {
        file: String,
        table: String,
        partition: String,
    },
    #[end_layout]
    #[route("/settings")]
    Settings {},
    #[route("/:..segments")]
    NotFound { segments: Vec<String> },
}

#[derive(Clone, Default)]
pub struct AppContext {
    pub status: Option<StatusApiModel>,
    pub refresh_token: u64,
}

fn main() {
    dioxus::LaunchBuilder::new().launch(|| {
        let theme = storage::load_theme().unwrap_or_else(|| "light".to_string());
        storage::apply_theme(&theme);

        rsx! {
            // The SVG is the real icon; the .ico stays as the fallback for the
            // browsers that ignore an SVG favicon. A browser that understands
            // both picks the SVG.
            document::Link {
                rel: "icon",
                r#type: "image/svg+xml",
                href: asset!("/public/favicon.svg"),
            }
            document::Link {
                rel: "alternate icon",
                r#type: "image/x-icon",
                href: asset!("/public/favicon.ico"),
            }
            Router::<AppRoute> {}
        }
    });
}

#[component]
fn Shell() -> Element {
    let ctx_signal = use_context_provider(|| Signal::new(AppContext::default()));
    let mut ctx = ctx_signal;

    // Health thresholds (Green/Yellow/Red) — loaded from the server once,
    // edited via the Settings page, persisted server-side.
    let thresholds_signal: Signal<HealthThresholds> =
        use_context_provider(|| Signal::new(HealthThresholds::default()));
    let mut thresholds = thresholds_signal;
    let mut thresholds_loaded = use_signal(|| false);
    let loaded_val = *thresholds_loaded.read();
    use_effect(move || {
        if loaded_val {
            return;
        }
        *thresholds_loaded.write() = true;
        spawn(async move {
            if let Ok(t) = api::get_health_thresholds().await {
                thresholds.set(t);
            }
        });
    });

    // Namespaces the server knows about. Loaded once — the list only changes
    // when somebody creates or deletes a namespace, which is not something the
    // UI has to poll for.
    let mut namespaces = use_signal(Vec::<models::NamespaceApiModel>::new);
    let mut namespaces_loaded = use_signal(|| false);
    let namespaces_loaded_val = *namespaces_loaded.read();
    use_effect(move || {
        if namespaces_loaded_val {
            return;
        }
        *namespaces_loaded.write() = true;
        spawn(async move {
            if let Ok(list) = api::get_namespaces_list().await {
                // A namespace stored from a previous session may have been
                // deleted since. Dropping it here matters because every request
                // carries the stored name in the `ns` header, and a name the
                // server no longer knows answers 404 - on every page at once,
                // with the picker among them.
                if let Some(selected) = storage::load_namespace()
                    && !list.iter().any(|itm| itm.name == selected)
                {
                    storage::save_namespace("");
                    reload_into_root();
                    return;
                }

                namespaces.set(list);
            }
        });
    });

    let current_ns = storage::load_namespace().unwrap_or_default();

    let on_namespace_change = move |namespace: String| {
        if namespace == storage::load_namespace().unwrap_or_default() {
            return;
        }

        storage::save_namespace(namespace.as_str());

        // A hard navigation to the root, not a re-render: the table list, the
        // loaded rows, the ticked row keys and the table/partition names sitting
        // in the URL all belong to the previous namespace, and several of them
        // are loaded once and never invalidated. Reloading is the only way to be
        // sure nothing of the old namespace survives the switch — in particular
        // ticked row keys, which would otherwise be fed to a bulk delete in the
        // newly selected namespace.
        reload_into_root();
    };

    let route = use_route::<AppRoute>();
    let section = match &route {
        AppRoute::Home {} => SidebarSection::Overview,
        AppRoute::Data {}
        | AppRoute::DataTable { .. }
        | AppRoute::DataPartition { .. }
        | AppRoute::DataRow { .. } => SidebarSection::Tables,
        AppRoute::Connections {} => SidebarSection::Connections,
        AppRoute::Snapshots {}
        | AppRoute::SnapshotFile { .. }
        | AppRoute::SnapshotTable { .. }
        | AppRoute::SnapshotPartition { .. } => SidebarSection::Snapshots,
        AppRoute::Settings {} => SidebarSection::Settings,
        _ => SidebarSection::Overview,
    };

    let crumbs = match &route {
        AppRoute::Connections {} => vec![
            Crumb {
                label: "MyNoSql".to_string(),
                active: false,
            },
            Crumb {
                label: "Connections".to_string(),
                active: true,
            },
        ],
        AppRoute::Snapshots {}
        | AppRoute::SnapshotFile { .. }
        | AppRoute::SnapshotTable { .. }
        | AppRoute::SnapshotPartition { .. } => vec![
            Crumb {
                label: "MyNoSql".to_string(),
                active: false,
            },
            Crumb {
                label: "Snapshots".to_string(),
                active: true,
            },
        ],
        AppRoute::Settings {} => vec![
            Crumb {
                label: "MyNoSql".to_string(),
                active: false,
            },
            Crumb {
                label: "Settings".to_string(),
                active: true,
            },
        ],
        AppRoute::Data {}
        | AppRoute::DataTable { .. }
        | AppRoute::DataPartition { .. }
        | AppRoute::DataRow { .. } => build_data_crumbs(&route),
        AppRoute::Home {} | AppRoute::NotFound { .. } => vec![
            Crumb {
                label: "MyNoSql".to_string(),
                active: false,
            },
            Crumb {
                label: "Overview".to_string(),
                active: true,
            },
        ],
    };

    let ctx_ra = ctx.read();
    let status = ctx_ra.status.clone();
    drop(ctx_ra);

    let online = status.is_some();

    // NOTE: `current_ns` above is the SELECT value, where an empty string means
    // the default namespace. Here we need the real name — and the real name is
    // the one the api layer is about to send: no `ns` header is what the server
    // answers as `default`, so that, and not "the first namespace reported", is
    // what the pages are looking at.
    let selected_ns_name =
        storage::load_namespace().unwrap_or_else(|| models::DEFAULT_NAMESPACE.to_string());

    // Tables of the selected namespace rather than of the whole server: the
    // Tables page lists exactly that set, so the badge beside it has to count
    // the same one. /api/Status is keyed by namespace, which is why this is a
    // lookup and not a sum.
    let tables_count = status
        .as_ref()
        .and_then(|s| s.namespace(Some(selected_ns_name.as_str())))
        .map(|namespace| namespace.tables_count as usize)
        .unwrap_or(0);

    // Readers are the whole of "connected" here. A write on this server is a
    // unary gRPC call that is over by the time anything could count it, so there
    // is no writer session to count beside them — what there is instead is the
    // open transactions, and those are counted separately because a transaction
    // is not a client.
    let readers_count = status.as_ref().map(|s| s.readers.len()).unwrap_or(0);
    let transactions_count = status.as_ref().map(|s| s.transactions.len()).unwrap_or(0);

    // Readers grouped by the namespace they work in, biggest first. /api/Status
    // is server-wide, so this covers every session, not just the ones of the
    // namespace the UI is currently pointed at.
    let readers_by_namespace: Vec<(String, usize)> = match status.as_ref() {
        Some(status) => {
            let mut by_namespace: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();

            for reader in status.readers.iter() {
                *by_namespace.entry(reader.namespace.clone()).or_default() += 1;
            }

            let mut result: Vec<(String, usize)> = by_namespace.into_iter().collect();
            result.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            result
        }
        None => Vec::new(),
    };

    let on_refresh = move |_| {
        let next = ctx.read().refresh_token.wrapping_add(1);
        ctx.write().refresh_token = next;
    };

    rsx! {
        div { class: "shell",
            Sidebar {
                active: section,
                tables_count,
                readers_count,
                readers_by_namespace,
                transactions_count,
                online,
            }
            div { class: "main",
                Topbar {
                    crumbs,
                    on_refresh: on_refresh,
                    namespaces: namespaces.read().clone(),
                    current_ns,
                    on_namespace_change: on_namespace_change,
                }
                Outlet::<AppRoute> {}
            }
        }
    }
}

/// Sends the browser to the app root and reloads it from scratch. Used when the
/// selected namespace changes: every loader in this app is one-shot and keyed on
/// table/partition names that mean something different in another namespace.
fn reload_into_root() {
    let _ = dioxus::document::eval("window.location.href = '/';");
}

/// Breadcrumbs for the data routes — reflects the `/data/<table>/<partition>/<row>`
/// path, with the deepest selected segment marked active.
fn build_data_crumbs(route: &AppRoute) -> Vec<Crumb> {
    let (table, partition, row) = match route {
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
    };

    let mut crumbs = vec![Crumb {
        label: "MyNoSql".to_string(),
        active: false,
    }];
    crumbs.push(Crumb {
        label: "Tables".to_string(),
        active: table.is_none(),
    });
    if let Some(t) = table {
        crumbs.push(Crumb {
            label: t,
            active: partition.is_none(),
        });
    }
    if let Some(p) = partition {
        crumbs.push(Crumb {
            label: p,
            active: row.is_none(),
        });
    }
    if let Some(r) = row {
        crumbs.push(Crumb {
            label: r,
            active: true,
        });
    }
    crumbs
}
