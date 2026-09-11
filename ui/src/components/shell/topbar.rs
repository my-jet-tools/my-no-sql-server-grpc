use std::time::Duration;

use dioxus::prelude::*;

use crate::api::{get_status, get_ui_settings, set_ui_writes};
use crate::components::atoms::{Icon, IconKind};
use crate::models::{DEFAULT_NAMESPACE, ServerApiModel};
use crate::settings::UiServerSettings;
use crate::storage;

/// How often the shell re-reads what it says about the server.
///
/// Slower than a page's own poll on purpose: a name, a version and a location
/// never move, an uptime is shown in hours, and both write windows are named in
/// minutes here - the second-by-second countdown belongs to the Settings page,
/// which is the page that owns the switch. The shell is on every page, so this
/// interval is paid everywhere.
const SHELL_POLL_SECS: u64 = 5;

#[derive(Clone, PartialEq)]
pub struct Crumb {
    pub label: String,
    pub active: bool,
}

/// What the shell knows about the server itself.
///
/// It is polled here rather than taken as a prop: the topbar is drawn on every
/// page, and `/api/Status` is otherwise read by the page that needs the whole of
/// it. The shell needs the head of that answer - `server` - and nothing else.
#[derive(Default)]
struct ShellState {
    started: bool,
    server: Option<ServerApiModel>,
    /// Both write windows come off `/api/Settings`: the UI window is reported
    /// nowhere else, and reading one route for both keeps the two badges from
    /// describing two different moments.
    windows: Option<UiServerSettings>,
}

#[component]
pub fn Topbar(
    crumbs: Vec<Crumb>,
    on_refresh: EventHandler<()>,
    namespaces: Vec<crate::models::NamespaceApiModel>,
    /// Empty string means the default namespace — the UI then sends no `ns`
    /// header at all, which is what a pre-namespace client does.
    current_ns: String,
    on_namespace_change: EventHandler<String>,
) -> Element {
    let mut theme = use_signal(|| storage::load_theme().unwrap_or_else(|| "light".to_string()));
    let theme_val = theme.read().clone();
    let is_dark = theme_val == "dark";

    let mut shell = use_signal(ShellState::default);
    let started_val = shell.read().started;
    use_effect(move || {
        if started_val {
            return;
        }
        shell.write().started = true;
        spawn(async move {
            loop {
                match get_status().await {
                    // Only the head of the answer is kept; the namespaces, the
                    // readers and the transactions in it belong to the pages.
                    Ok(status) => shell.write().server = Some(status.server),
                    Err(err) => {
                        // The last good answer stays on screen: a name blinking
                        // out of the chrome on one missed poll says "broken"
                        // about the wrong thing.
                        dioxus_utils::console_log(format!("Shell status error: {}", err));
                    }
                }

                match get_ui_settings().await {
                    Ok(settings) => shell.write().windows = Some(settings),
                    Err(err) => {
                        dioxus_utils::console_log(format!("Shell settings error: {}", err));
                    }
                }

                dioxus_utils::js::sleep(Duration::from_secs(SHELL_POLL_SECS)).await;
            }
        });
    });

    let toggle_theme = move |_| {
        let next = if theme.read().as_str() == "dark" {
            "light"
        } else {
            "dark"
        };
        storage::save_theme(next);
        storage::apply_theme(next);
        theme.set(next.to_string());
    };

    // The window shuts itself after ten minutes, so the badge is also the way
    // to keep it open: an operator halfway through a clean-up should not have to
    // walk back to the Settings page to buy another ten.
    let extend_ui_writes = move |_| {
        spawn(async move {
            if let Err(err) = set_ui_writes(true).await {
                dioxus_utils::console_log(format!("Write window error: {}", err));
                return;
            }
            if let Ok(settings) = get_ui_settings().await {
                shell.write().windows = Some(settings);
            }
        });
    };

    let crumbs_iter = crumbs.into_iter().enumerate().map(|(i, c)| {
        let cls = if c.active {
            "topbar__crumb active"
        } else {
            "topbar__crumb"
        };
        let sep = if i > 0 {
            rsx! {
                span { class: "topbar__crumb-sep", "/" }
            }
        } else {
            rsx! {}
        };
        rsx! {
            {sep}
            span { class: cls, "{c.label}" }
        }
    });

    let theme_icon = if is_dark {
        IconKind::Sun
    } else {
        IconKind::Moon
    };

    let shell_ra = shell.read();
    let server = shell_ra.server.clone();
    let windows = shell_ra.windows;
    drop(shell_ra);

    // Which server this browser tab is actually looking at. Two of these tabs
    // side by side are the normal case, and the namespace picker below is not
    // enough to tell them apart.
    let server_badges = match server {
        Some(server) => {
            let identity = if server.version.is_empty() {
                server.name.clone()
            } else {
                format!("{} · v{}", server.name, server.version)
            };
            let ports = format!("gRPC {} · HTTP {}", server.grpc_port, server.http_port);
            let uptime = format_uptime(server.up_time_secs);
            let started_at = server.started_at.clone();

            let location = if server.location.is_empty() {
                rsx! {}
            } else {
                let location = server.location.clone();
                rsx! {
                    span {
                        class: "badge badge--neutral",
                        title: "Where this server says it runs",
                        "{location}"
                    }
                }
            };

            rsx! {
                span { class: "badge badge--neutral", title: "{ports}", "{identity}" }
                {location}
                span {
                    class: "badge badge--neutral",
                    title: "Started at {started_at}",
                    "up {uptime}"
                }
            }
        }
        None => rsx! {},
    };

    // Both windows are held in memory on the server and shut themselves, so the
    // only honest way to show one is while it is open.
    let write_windows = match windows {
        Some(windows) => {
            let ui = if windows.ui_writes_enabled {
                let left = format_window(windows.ui_writes_remaining_secs);
                rsx! {
                    button {
                        class: "badge badge--warn",
                        style: "cursor: pointer;",
                        title: "The destructive UI operations are open. Click to give them another 10 minutes.",
                        onclick: extend_ui_writes,
                        "ui writes · {left}"
                    }
                }
            } else {
                rsx! {}
            };

            // Shown, never thrown: the MCP window is opened by a human calling
            // `POST /api/Mcp/Writes`, and the server withholds a tool for it on
            // purpose. A button here would be that tool with a mouse on it.
            let mcp = if windows.mcp_writes_enabled {
                let left = format_window(windows.mcp_writes_remaining_secs);
                rsx! {
                    span {
                        class: "badge badge--warn",
                        title: "The MCP write tools are open. Only POST /api/Mcp/Writes opens or shuts this one.",
                        "mcp writes · {left}"
                    }
                }
            } else {
                rsx! {}
            };

            rsx! {
                {ui}
                {mcp}
            }
        }
        None => rsx! {},
    };

    // The default namespace is offered with an empty value: the UI then stores
    // nothing and sends no `ns` header, which is exactly how it behaved before
    // namespaces existed.
    let ns_options = namespaces.into_iter().map(|ns| {
        let value = if ns.name == DEFAULT_NAMESPACE {
            String::new()
        } else {
            ns.name.clone()
        };
        let is_current = value == current_ns;
        let label = format!("{} · {}", ns.name, ns.tables_amount);

        rsx! {
            option { key: "{ns.name}", value: "{value}", selected: is_current, "{label}" }
        }
    });

    rsx! {
        header { class: "topbar",
            div { class: "topbar__breadcrumbs", {crumbs_iter} }
            span { class: "badge-list",
                {server_badges}
                {write_windows}
            }
            div { class: "topbar__ns",
                Icon { kind: IconKind::Layers, class: "topbar__ns-icon".to_string() }
                select {
                    id: "topbar-namespace",
                    // Everything below this bar is read per namespace now, so
                    // this select is what the whole UI is pointed with.
                    title: "Namespace the UI works in",
                    value: "{current_ns}",
                    onchange: move |evt| on_namespace_change.call(evt.value()),
                    {ns_options}
                }
            }
            div { class: "topbar__search",
                Icon { kind: IconKind::Search, class: "topbar__search-icon".to_string() }
                input { id: "topbar-search", placeholder: "Search tables, partitions, rows…" }
                span { class: "kbd", "⌘K" }
            }
            div { class: "topbar__actions",
                button {
                    class: "topbar__icon-btn",
                    title: "Refresh",
                    onclick: move |_| on_refresh.call(()),
                    Icon { kind: IconKind::RefreshCw }
                }
                button {
                    class: "topbar__icon-btn",
                    title: "Toggle theme",
                    onclick: toggle_theme,
                    Icon { kind: theme_icon }
                }
            }
        }
    }
}

/// Coarse on purpose: a server that has been up for three days is not read to
/// the second, and the exact moment it started is on the badge as a title.
fn format_uptime(up_time_secs: f64) -> String {
    if !up_time_secs.is_finite() || up_time_secs <= 0.0 {
        return "0s".to_string();
    }

    let total = up_time_secs as u64;
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;

    if days > 0 {
        format!("{}d {}h", days, hours)
    } else if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else if minutes > 0 {
        format!("{}m", minutes)
    } else {
        format!("{}s", total)
    }
}

/// Minutes, not a ticking clock: the shell re-reads the window every few
/// seconds, and a countdown that jumps five at a time reads as broken.
fn format_window(remaining_secs: Option<u64>) -> String {
    match remaining_secs {
        Some(secs) if secs >= 60 => format!("{}m left", secs / 60),
        Some(_) => "<1m left".to_string(),
        // Open with no time named - an older server, or a window that expired
        // between the answer and this render.
        None => "open".to_string(),
    }
}
