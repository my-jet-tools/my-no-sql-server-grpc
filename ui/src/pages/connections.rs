use std::time::Duration;

use dioxus::prelude::*;

use crate::api::get_connections;
use crate::components::atoms::{
    Badge, BadgeTone, Icon, IconKind, MiniChart, MiniChartSeries, StatePill, StateTone,
};
use crate::components::overview::classify_secs_ago;
use crate::models::{ConnectionsApiModel, ReaderApiModel};
use crate::settings::HealthThresholds;

/// Two minutes of history at one sample a second.
const MAX_POINTS: usize = 120;

/// Tables named on a row before the rest is folded into a "+n" badge.
const MAX_TABLE_BADGES: usize = 4;

/// One poll, rolled up. `/api/Connections` reports a gauge per reader and no
/// history at all, so the series is sampled here - this page is the only place
/// that sees two polls in a row.
#[derive(Clone, Copy)]
struct Sample {
    worst_secs_ago: f64,
    average_secs_ago: f64,
}

#[derive(Default)]
struct ConnectionsState {
    started: bool,
    snapshot: Option<ConnectionsApiModel>,
    history: Vec<Sample>,
}

impl ConnectionsState {
    fn push(&mut self, snapshot: ConnectionsApiModel) {
        if snapshot.readers.is_empty() {
            // Nothing to sample: a zero here would draw a line saying everybody
            // polled just now, which is the opposite of what it would mean.
            self.history.clear();
        } else {
            let worst = snapshot
                .readers
                .iter()
                .map(|reader| reader.last_incoming_secs_ago)
                .fold(0.0_f64, f64::max);

            let total: f64 = snapshot
                .readers
                .iter()
                .map(|reader| reader.last_incoming_secs_ago)
                .sum();

            self.history.push(Sample {
                worst_secs_ago: worst,
                average_secs_ago: total / snapshot.readers.len() as f64,
            });

            if self.history.len() > MAX_POINTS {
                let overflow = self.history.len() - MAX_POINTS;
                self.history.drain(0..overflow);
            }
        }

        self.snapshot = Some(snapshot);
    }
}

#[component]
pub fn Connections() -> Element {
    let mut cs = use_signal(ConnectionsState::default);
    let thresholds = *use_context::<Signal<HealthThresholds>>().read();

    let started_val = cs.read().started;
    let on_mount = move |_| {
        if started_val {
            return;
        }
        cs.write().started = true;
        spawn(async move {
            loop {
                match get_connections().await {
                    Ok(result) => cs.write().push(result),
                    Err(err) => {
                        dioxus_utils::console_log(format!("Connections error: {}", err));
                    }
                }
                dioxus_utils::js::sleep(Duration::from_secs(1)).await;
            }
        });
    };

    let cs_ra = cs.read();
    let history = cs_ra.history.clone();
    let snapshot = cs_ra.snapshot.clone();
    drop(cs_ra);

    let content = match snapshot {
        Some(snapshot) => render_connections(&history, &snapshot, thresholds),
        None => rsx! {
            div { class: "empty-state",
                div { class: "empty-state__title", "Connecting to server…" }
            }
        },
    };

    rsx! {
        section { class: "page page--padded", onmounted: on_mount,
            div { class: "connections", {content} }
        }
    }
}

fn render_connections(
    history: &[Sample],
    snapshot: &ConnectionsApiModel,
    thresholds: HealthThresholds,
) -> Element {
    // Every reader, whatever namespace it works in - the registry is
    // server-wide, and a reader missing from the list is exactly what somebody
    // opens this page to find out about. Which namespace each one is in is a
    // column instead.
    let readers = snapshot.readers.as_slice();

    rsx! {
        {render_poll_age_card(history, readers)}
        {render_readers_card(readers, thresholds)}
    }
}

/// How long the readers have been going without asking for changes, over time.
///
/// This is where the four traffic counters of the JSON version used to be
/// charted; this server counts no bytes per reader, and the age of a poll is
/// what it does report about one - which is also the number this page is read
/// for.
fn render_poll_age_card(history: &[Sample], readers: &[ReaderApiModel]) -> Element {
    if readers.is_empty() {
        return rsx! {};
    }

    let worst = readers
        .iter()
        .map(|reader| reader.last_incoming_secs_ago)
        .fold(0.0_f64, f64::max);
    let average = readers
        .iter()
        .map(|reader| reader.last_incoming_secs_ago)
        .sum::<f64>()
        / readers.len() as f64;
    let pending: u64 = readers.iter().map(|reader| reader.pending_chunks).sum();

    let series = vec![
        MiniChartSeries::new(
            history.iter().map(|s| s.worst_secs_ago).collect(),
            "mini-chart__line--out",
        ),
        MiniChartSeries::new(
            history.iter().map(|s| s.average_secs_ago).collect(),
            "mini-chart__line--in",
        ),
    ];
    let max = history
        .iter()
        .map(|s| s.worst_secs_ago)
        .fold(0.0_f64, f64::max)
        .max(1.0);

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Reader poll age · all readers" }
                div { class: "conn-legend",
                    span { class: "conn-legend__item",
                        span { class: "conn-legend__dot conn-legend__dot--out" }
                        "Worst "
                        b { "{format_secs_ago(worst)}" }
                    }
                    span { class: "conn-legend__item",
                        span { class: "conn-legend__dot conn-legend__dot--in" }
                        "Average "
                        b { "{format_secs_ago(average)}" }
                    }
                    span { class: "conn-legend__item",
                        span { class: "conn-legend__dot conn-legend__dot--write" }
                        "Queued "
                        b { "{pending}" }
                        span { class: "conn-legend__sub", " chunks" }
                    }
                }
            }
            div { class: "card__body",
                MiniChart {
                    series,
                    max,
                    label: format_secs_ago(max),
                }
            }
        }
    }
}

fn render_readers_card(readers: &[ReaderApiModel], thresholds: HealthThresholds) -> Element {
    let namespaces = readers
        .iter()
        .map(|reader| reader.namespace.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();

    let stalled = readers
        .iter()
        .filter(|reader| {
            matches!(
                classify_secs_ago(reader.last_incoming_secs_ago, thresholds),
                StateTone::Bad
            )
        })
        .count();

    let subtitle = match namespaces {
        0 | 1 => format!("{} attached", readers.len()),
        amount => format!("{} attached · {} namespaces", readers.len(), amount),
    };

    let stalled_pill = if stalled > 0 {
        rsx! {
            StatePill { label: format!("{stalled} stalled"), tone: StateTone::Bad }
        }
    } else {
        rsx! {}
    };

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Readers" }
                div { style: "display:flex; align-items:center; gap:10px;",
                    span { class: "card__subtitle", "{subtitle}" }
                    {stalled_pill}
                }
            }
            div { class: "card__body", {render_readers_body(readers, thresholds)} }
            // Said out loud rather than left as an empty table: the JSON
            // version listed writer sessions here, and a reader of this page
            // is entitled to know why this one cannot.
            div { class: "card__footer",
                "Writers are not listed: a write here is a unary gRPC call, so nothing holds a session between two of them. The one write that does hold one is an open transaction, and those are reported with the server status."
            }
        }
    }
}

fn render_readers_body(readers: &[ReaderApiModel], thresholds: HealthThresholds) -> Element {
    if readers.is_empty() {
        return rsx! {
            div { class: "empty-state",
                div { class: "empty-state__icon",
                    Icon { kind: IconKind::Plug }
                }
                div { class: "empty-state__title", "No readers attached" }
                div { class: "empty-state__sub",
                    "A reader shows up here as soon as it subscribes to a table."
                }
            }
        };
    }

    // Left in the order the server sent it: it sorts by namespace, app name and
    // id precisely so two samples a second apart can be compared by eye.
    let rows = readers.iter().map(|reader| {
        let tone = classify_secs_ago(reader.last_incoming_secs_ago, thresholds);

        let table_badges = reader
            .tables
            .iter()
            .take(MAX_TABLE_BADGES)
            .cloned()
            .map(|table| {
                rsx! {
                    Badge { text: table, tone: BadgeTone::Reader }
                }
            });

        let overflow_badge = if reader.tables.len() > MAX_TABLE_BADGES {
            let rest = reader.tables.len() - MAX_TABLE_BADGES;
            rsx! {
                Badge { text: format!("+{rest}"), tone: BadgeTone::Neutral }
            }
        } else {
            rsx! {}
        };

        // A queue that keeps growing is a reader that stopped reading, so the
        // number is only coloured when there is something in it.
        let pending_style = if reader.pending_chunks > 0 {
            "color: var(--warn);"
        } else {
            ""
        };

        rsx! {
            tr {
                td { class: "conn-table__id", "{reader.id}" }
                td { "{reader.name}" }
                td { class: "conn-table__id", "{reader.version}" }
                td { class: "conn-table__ns", "{reader.namespace}" }
                td { class: "conn-table__id", "{reader.ip}" }
                td { class: "conn-table__id", "{format_moment(&reader.connected_at)}" }
                td { class: "conn-table__num",
                    StatePill {
                        label: format_secs_ago(reader.last_incoming_secs_ago),
                        tone,
                    }
                }
                td { class: "conn-table__num", style: "{pending_style}", "{reader.pending_chunks}" }
                td {
                    span { class: "badge-list",
                        {table_badges}
                        {overflow_badge}
                    }
                }
            }
        }
    });

    rsx! {
        table { class: "conn-table",
            thead {
                tr {
                    th { "ID" }
                    th { "App" }
                    th { "Version" }
                    th { "Namespace" }
                    th { "IP" }
                    th { "Connected" }
                    th { class: "conn-table__num", "Last poll" }
                    th { class: "conn-table__num", "Queued" }
                    th { "Subscribed tables" }
                }
            }
            tbody { {rows} }
        }
    }
}

fn format_secs_ago(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "0.0s".to_string();
    }
    if secs < 10.0 {
        return format!("{:.1}s", secs);
    }

    let secs = secs as u64;
    if secs < 60 {
        return format!("{}s", secs);
    }
    if secs < 3_600 {
        return format!("{}m {:02}s", secs / 60, secs % 60);
    }
    format!("{}h {:02}m", secs / 3_600, (secs % 3_600) / 60)
}

/// `connectedAt` arrives as RFC3339 with microseconds and an offset - wider
/// than the cell, and the fraction of a second a session was accepted in is not
/// what the column is read for.
fn format_moment(value: &str) -> String {
    if value.is_empty() {
        return "—".to_string();
    }

    value.chars().take(19).collect::<String>().replace('T', " ")
}
