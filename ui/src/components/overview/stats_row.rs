use dioxus::prelude::*;

use super::classify_secs_ago;
use crate::components::atoms::{DeltaTone, Stat, StatTone, StateTone};
use crate::models::{
    NamespaceStatusApiModel, ReaderApiModel, TransactionApiModel, WriteWindowApiModel,
};
use crate::settings::HealthThresholds;
use crate::utils::{format_bytes, format_duration_secs, format_moment};

/// The tiles are not the JSON version's. There is no status bar on this server
/// and no traffic counters behind one: a write is a unary gRPC call that is
/// over by the time it could be counted. What is shown instead is what this
/// server does report — who it is, what the selected namespace holds, what is
/// still owed to disk, who is reading, what is mid-transaction, and whether the
/// MCP write window is open.
#[component]
pub fn StatsRow(
    version: String,
    location: String,
    up_time_secs: f64,
    namespaces_count: usize,
    namespace: NamespaceStatusApiModel,
    readers: Vec<ReaderApiModel>,
    transactions: Vec<TransactionApiModel>,
    mcp_writes: WriteWindowApiModel,
) -> Element {
    let thresholds = *use_context::<Signal<HealthThresholds>>().read();

    let version = if version.is_empty() {
        "—".to_string()
    } else {
        version
    };
    let location = if location.is_empty() {
        "no location set".to_string()
    } else {
        location
    };
    // Up-time rides along with the location instead of taking a tile of its
    // own: the banner shows it too, and one tile per number would push the
    // namespace off the row.
    let server_delta = format!("{location} · up {}", format_duration_secs(up_time_secs));

    let namespace_delta = if namespaces_count > 1 {
        format!("{namespaces_count} namespaces on server")
    } else {
        "the only namespace".to_string()
    };

    // One entity per table is the normal case, so this is 0 almost always —
    // which is why it is only shown when it is not: either a deploy is going
    // through or two different entities are aimed at one table.
    let multi_schema = namespace
        .tables
        .iter()
        .filter(|table| table.schemas_count > 1)
        .count();
    let tables_delta = if multi_schema > 0 {
        format!(
            "{} partitions · {} multi-schema",
            namespace.partitions_count, multi_schema
        )
    } else {
        format!("{} partitions", namespace.partitions_count)
    };

    let queue = &namespace.persist_queue;
    let persisted = match queue.last_persisted_at.as_deref() {
        Some(moment) => format!("saved {}", clock_of(moment)),
        // Nothing has reached the disk in this run: either nothing changed, or
        // nothing in this namespace persists at all.
        None => "never saved".to_string(),
    };
    let persist_tone = if queue.partitions + queue.tables_metadata > 0 {
        StatTone::Warn
    } else {
        StatTone::Ok
    };

    let mut ok = 0;
    let mut warn = 0;
    let mut bad = 0;
    for reader in readers.iter() {
        match classify_secs_ago(reader.last_incoming_secs_ago, thresholds) {
            StateTone::Ok => ok += 1,
            StateTone::Warn => warn += 1,
            StateTone::Bad => bad += 1,
            StateTone::Neutral => {}
        }
    }

    let reader_count = readers.len();
    let reader_tone = if bad > 0 {
        StatTone::Bad
    } else if warn > 0 {
        StatTone::Warn
    } else {
        StatTone::Ok
    };

    let transactions_count = transactions.len();
    let buffered_actions: u64 = transactions.iter().map(|itm| itm.actions).sum();
    let idle_transactions = transactions
        .iter()
        .filter(|itm| {
            matches!(
                classify_secs_ago(itm.last_incoming_secs_ago, thresholds),
                StateTone::Bad
            )
        })
        .count();

    let (transactions_delta, transactions_tone) = if transactions_count == 0 {
        ("nothing in flight".to_string(), StatTone::Info)
    } else if idle_transactions > 0 {
        (
            format!("{buffered_actions} actions · {idle_transactions} idle"),
            StatTone::Warn,
        )
    } else {
        (
            format!("{buffered_actions} actions buffered"),
            StatTone::Info,
        )
    };

    // An open window is the state worth noticing, so it is the one tinted:
    // closed means the MCP tools can only read, which is the resting state.
    let (mcp_value, mcp_delta, mcp_tone) = if mcp_writes.open {
        let remaining = match mcp_writes.remaining_secs {
            Some(secs) => format!("{} left", format_duration_secs(secs as f64)),
            None => "closing".to_string(),
        };
        ("open".to_string(), remaining, StatTone::Warn)
    } else {
        (
            "closed".to_string(),
            "read-only tools".to_string(),
            StatTone::Ok,
        )
    };

    rsx! {
        div { class: "stats-row",
            Stat {
                label: "Server".to_string(),
                value: version,
                delta: server_delta,
                tone: StatTone::Info,
            }
            Stat {
                label: "Namespace".to_string(),
                value: namespace.name.clone(),
                delta: namespace_delta,
                tone: StatTone::Info,
            }
            Stat {
                label: "Tables".to_string(),
                value: format!("{}", namespace.tables_count),
                delta: tables_delta,
                delta_tone: if multi_schema > 0 { DeltaTone::Warn } else { DeltaTone::Neutral },
                tone: StatTone::Info,
            }
            Stat {
                label: "Rows in memory".to_string(),
                value: format_compact(namespace.rows_count),
                unit: "rows".to_string(),
                delta: format_bytes(namespace.data_size as f64),
                tone: StatTone::Ok,
            }
            Stat {
                label: "Persist queue".to_string(),
                value: format!("{}", queue.partitions),
                unit: "partitions".to_string(),
                delta: format!("{} table meta · {}", queue.tables_metadata, persisted),
                delta_tone: if matches!(persist_tone, StatTone::Warn) { DeltaTone::Warn } else { DeltaTone::Neutral },
                tone: persist_tone,
            }
            Stat {
                label: "Readers".to_string(),
                value: format!("{reader_count}"),
                unit: "connected".to_string(),
                delta: format!("{ok} ok · {warn} slow · {bad} stalled"),
                delta_tone: tone_to_delta(reader_tone),
                tone: reader_tone,
            }
            Stat {
                label: "Transactions".to_string(),
                value: format!("{transactions_count}"),
                unit: "open".to_string(),
                delta: transactions_delta,
                delta_tone: tone_to_delta(transactions_tone),
                tone: transactions_tone,
            }
            Stat {
                label: "MCP writes".to_string(),
                value: mcp_value,
                delta: mcp_delta,
                tone: mcp_tone,
            }
        }
    }
}

/// The tile's delta line has room for a clock, not for a date: the date only
/// differs from today's when the queue number next to it is already the
/// alarming part. Rendered through `format_moment` so there is one place that
/// knows what shape the server sends.
fn clock_of(moment: &str) -> String {
    let rendered = format_moment(moment);

    match rendered.split_once(' ') {
        Some((_, clock)) => clock.to_string(),
        None => rendered,
    }
}

fn tone_to_delta(tone: StatTone) -> DeltaTone {
    match tone {
        StatTone::Ok => DeltaTone::Ok,
        StatTone::Warn => DeltaTone::Warn,
        StatTone::Bad => DeltaTone::Bad,
        StatTone::Info => DeltaTone::Neutral,
    }
}

pub fn format_compact(n: u64) -> String {
    let v = n as f64;
    if v >= 1_000_000_000.0 {
        format!("{:.1}B", v / 1_000_000_000.0)
    } else if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1}K", v / 1_000.0)
    } else {
        format!("{}", n)
    }
}
