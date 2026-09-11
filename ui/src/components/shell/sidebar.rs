use dioxus::prelude::*;

use crate::AppRoute;
use crate::components::atoms::{Icon, IconKind};

#[component]
pub fn Sidebar(
    active: SidebarSection,
    tables_count: usize,
    /// Reader sessions attached to the server. There is no writer count beside
    /// it and no total called "clients": a write here is a unary gRPC call, so
    /// nothing holds a session between two of them.
    readers_count: usize,
    /// Readers per namespace, biggest first. Empty while the status has not
    /// arrived yet.
    readers_by_namespace: Vec<(String, usize)>,
    /// Transactions open right now - the one write that does hold a session,
    /// which is why the foot names them next to the readers.
    transactions_count: usize,
    online: bool,
) -> Element {
    let dot_class = if online {
        "sidebar__live-dot"
    } else {
        "sidebar__live-dot offline"
    };
    let live_text = if online {
        let readers = if readers_count == 1 {
            "1 reader".to_string()
        } else {
            format!("{} readers", readers_count)
        };

        if transactions_count > 0 {
            format!("Live · {} · {} tx", readers, transactions_count)
        } else {
            format!("Live · {}", readers)
        }
    } else {
        "Offline".to_string()
    };

    // Only worth the line when there is something to disambiguate: a server
    // running a single namespace says nothing new by naming it.
    let ns_breakdown = if online && readers_by_namespace.len() > 1 {
        let items = readers_by_namespace.into_iter().map(|(namespace, amount)| {
            rsx! {
                span { class: "sidebar__live-ns", key: "{namespace}",
                    span { class: "sidebar__live-ns-name", "{namespace}" }
                    span { class: "sidebar__live-ns-count", "{amount}" }
                }
            }
        });

        rsx! {
            div { class: "sidebar__live-by-ns", {items} }
        }
    } else {
        rsx! {}
    };

    rsx! {
        aside { class: "sidebar",
            div { class: "sidebar__brand",
                div { class: "sidebar__logo",
                    img {
                        class: "sidebar__logo-img",
                        src: asset!("/public/favicon.svg"),
                        alt: "MyNoSql",
                    }
                }
                div {
                    div { class: "sidebar__brand-name", "MyNoSql" }
                    // Which MyNoSql this is, rather than a version: the real
                    // name, version and location of the server are polled and
                    // shown in the topbar, and a hardcoded "v0.7.3 · prod"
                    // under the logo was a lie the moment it was typed.
                    div { class: "sidebar__brand-sub", "protobuf entities" }
                }
            }
            nav { class: "sidebar__nav",
                Link {
                    to: AppRoute::Home {},
                    class: nav_class(active == SidebarSection::Overview),
                    Icon { kind: IconKind::Activity, class: "sidebar__nav-icon".to_string() }
                    span { class: "sidebar__nav-label", "Overview" }
                }
                Link {
                    to: AppRoute::Data {},
                    class: nav_class(active == SidebarSection::Tables),
                    Icon { kind: IconKind::Database, class: "sidebar__nav-icon".to_string() }
                    span { class: "sidebar__nav-label", "Tables" }
                    span { class: "sidebar__nav-count", "{tables_count}" }
                }
                Link {
                    to: AppRoute::Connections {},
                    class: nav_class(active == SidebarSection::Connections),
                    Icon { kind: IconKind::Plug, class: "sidebar__nav-icon".to_string() }
                    span { class: "sidebar__nav-label", "Connections" }
                    // The whole registry, like the page behind it: readers are
                    // listed there with a namespace column instead of being
                    // filtered down to the selected one.
                    span { class: "sidebar__nav-count", "{readers_count}" }
                }
                Link {
                    to: AppRoute::Snapshots {},
                    class: nav_class(active == SidebarSection::Snapshots),
                    Icon { kind: IconKind::HardDrive, class: "sidebar__nav-icon".to_string() }
                    span { class: "sidebar__nav-label", "Snapshots" }
                }
                Link {
                    to: AppRoute::Settings {},
                    class: nav_class(active == SidebarSection::Settings),
                    Icon { kind: IconKind::Settings, class: "sidebar__nav-icon".to_string() }
                    span { class: "sidebar__nav-label", "Settings" }
                }
            }
            div { class: "sidebar__foot",
                div { class: "sidebar__live",
                    div { class: "sidebar__live-line",
                        span { class: dot_class }
                        span { "{live_text}" }
                    }
                    // Which namespaces those readers are in. Without it the
                    // total is ambiguous the moment a second namespace exists.
                    {ns_breakdown}
                }
            }
        }
    }
}

fn nav_class(active: bool) -> &'static str {
    if active {
        "sidebar__nav-item active"
    } else {
        "sidebar__nav-item"
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum SidebarSection {
    Overview,
    Tables,
    Connections,
    Snapshots,
    Settings,
}
