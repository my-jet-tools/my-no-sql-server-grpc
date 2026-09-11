use dioxus::prelude::*;

use super::classify_secs_ago;
use crate::components::atoms::{Badge, BadgeTone, StateTone, StatusDot};
use crate::models::ReaderApiModel;
use crate::settings::HealthThresholds;
use crate::utils::format_secs_ago;

#[derive(Clone, Copy, PartialEq)]
enum ReaderFilter {
    All,
    Healthy,
    Issues,
}

#[derive(Default)]
struct ReadersState {
    filter: Option<ReaderFilter>,
    show_all: bool,
}

impl ReadersState {
    fn current_filter(&self) -> ReaderFilter {
        self.filter.unwrap_or(ReaderFilter::All)
    }
}

/// Every reader session the server holds, whatever namespace it reads — the
/// grid above and this table are about sessions, not about the namespace the
/// page is scoped to, which is why the namespace is a column here.
#[component]
pub fn ReadersTable(readers: Vec<ReaderApiModel>) -> Element {
    let thresholds = *use_context::<Signal<HealthThresholds>>().read();
    let mut cs = use_signal(ReadersState::default);
    let cs_ra = cs.read();
    let active_filter = cs_ra.current_filter();
    let show_all = cs_ra.show_all;
    drop(cs_ra);

    let total = readers.len();

    let filtered: Vec<ReaderApiModel> = readers
        .into_iter()
        .filter(|r| {
            let tone = classify_secs_ago(r.last_incoming_secs_ago, thresholds);
            match active_filter {
                ReaderFilter::All => true,
                ReaderFilter::Healthy => matches!(tone, StateTone::Ok),
                ReaderFilter::Issues => !matches!(tone, StateTone::Ok),
            }
        })
        .collect();

    let filtered_total = filtered.len();
    let display_limit = if show_all { filtered_total } else { 14 };
    let display: Vec<ReaderApiModel> = filtered.into_iter().take(display_limit).collect();

    let rows = display.into_iter().map(|r| {
        let tone = classify_secs_ago(r.last_incoming_secs_ago, thresholds);
        let tone_for_time = tone;
        let visible_tables: Vec<String> = r.tables.iter().take(3).cloned().collect();
        let overflow = if r.tables.len() > 3 {
            Some(r.tables.len() - 3)
        } else {
            None
        };

        let table_badges = visible_tables.into_iter().map(|t| {
            rsx! {
                Badge { text: t, tone: BadgeTone::Reader }
            }
        });

        let overflow_badge = if let Some(n) = overflow {
            rsx! {
                Badge { text: format!("+{n}"), tone: BadgeTone::Neutral }
            }
        } else {
            rsx! {}
        };

        let last_class = match tone_for_time {
            StateTone::Bad => "mono",
            StateTone::Warn => "mono",
            _ => "mono muted",
        };
        let last_style = match tone_for_time {
            StateTone::Bad => "color: var(--danger);",
            StateTone::Warn => "color: var(--warn);",
            _ => "",
        };

        // Chunks the server is still holding for this session. Zero is the
        // resting state, so only a backlog is written in full colour.
        let pending_class = if r.pending_chunks > 0 {
            "mono num"
        } else {
            "mono num muted"
        };

        rsx! {
            tr {
                td { class: "mono muted", "{r.id}" }
                td {
                    div { style: "display:flex; align-items:center; gap:8px;",
                        StatusDot { tone }
                        span { "{r.name}" }
                    }
                }
                td { class: "mono muted", "{r.version}" }
                td { class: "mono muted", "{r.namespace}" }
                td { class: "mono muted",
                    span { class: "dt-ellipsis", "{r.ip}" }
                }
                td {
                    span { class: "badge-list",
                        {table_badges}
                        {overflow_badge}
                    }
                }
                td { class: "{pending_class}", "{r.pending_chunks}" }
                td { class: "{last_class}", style: "{last_style}",
                    "{format_secs_ago(r.last_incoming_secs_ago)}"
                }
            }
        }
    });

    let mut on_filter = move |f: ReaderFilter| {
        let mut w = cs.write();
        w.filter = Some(f);
        w.show_all = false;
    };

    let footer = if filtered_total > display_limit {
        let remaining = filtered_total - display_limit;
        rsx! {
            div { class: "card__footer",
                a { onclick: move |_| { cs.write().show_all = true; },
                    "Show all ({remaining} more)"
                }
            }
        }
    } else if show_all && filtered_total > 14 {
        rsx! {
            div { class: "card__footer",
                a { onclick: move |_| { cs.write().show_all = false; },
                    "Collapse"
                }
            }
        }
    } else {
        rsx! {}
    };

    let chip_cls = move |f: ReaderFilter| -> &'static str {
        if f == active_filter {
            "chip active"
        } else {
            "chip"
        }
    };

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Readers" }
                div { style: "display:flex; align-items:center; gap:10px;",
                    span { class: "card__subtitle", "{total} connected" }
                    div { class: "chip-group",
                        button {
                            class: chip_cls(ReaderFilter::All),
                            onclick: move |_| on_filter(ReaderFilter::All),
                            "All"
                        }
                        button {
                            class: chip_cls(ReaderFilter::Healthy),
                            onclick: move |_| on_filter(ReaderFilter::Healthy),
                            "Healthy"
                        }
                        button {
                            class: chip_cls(ReaderFilter::Issues),
                            onclick: move |_| on_filter(ReaderFilter::Issues),
                            "Issues"
                        }
                    }
                }
            }
            table { class: "dt",
                thead {
                    tr {
                        th { "Id" }
                        th { "Client" }
                        th { "Version" }
                        th { "Namespace" }
                        th { "Address" }
                        th { "Tables" }
                        th { class: "num", "Pending" }
                        th { "Last incoming" }
                    }
                }
                tbody { {rows} }
            }
            {footer}
        }
    }
}
