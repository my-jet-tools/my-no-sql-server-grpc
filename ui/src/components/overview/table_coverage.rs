use dioxus::prelude::*;
use std::collections::HashMap;

use crate::models::{ReaderApiModel, TableApiModel};

/// The donut used to be "tables somebody is writing to", counted from the
/// connected writers. There are none to count here, so it is counted from
/// `lastWriteAt` instead — the nearest thing this server reports, and the same
/// question: a table nothing has written to since the process started.
///
/// The list underneath is unchanged: the tables the most reader sessions are
/// subscribed to.
#[component]
pub fn TableCoverage(tables: Vec<TableApiModel>, readers: Vec<ReaderApiModel>) -> Element {
    let total_tables = tables.len();
    let covered = tables
        .iter()
        .filter(|table| table.last_write_at.is_some())
        .count();

    let pct = if total_tables == 0 {
        0.0
    } else {
        (covered as f64 / total_tables as f64) * 100.0
    };

    let mut reads: HashMap<String, usize> = HashMap::new();
    for r in readers.iter() {
        for t in &r.tables {
            *reads.entry(t.clone()).or_insert(0) += 1;
        }
    }

    let mut top: Vec<(String, usize)> = reads.into_iter().collect();
    top.sort_by_key(|(_, reads)| std::cmp::Reverse(*reads));
    top.truncate(6);

    let max_reads = top.iter().map(|(_, n)| *n).max().unwrap_or(1) as f64;

    let radius: f64 = 55.0;
    let circumference = 2.0 * std::f64::consts::PI * radius;
    let dash = circumference * (pct / 100.0);
    let offset = circumference - dash;

    let empty_hint = if top.is_empty() {
        rsx! {
            div { style: "color:var(--text-muted); font-size:12px; padding:14px;",
                "No reader is subscribed to a table"
            }
        }
    } else {
        rsx! {}
    };

    let list = top.into_iter().map(|(name, n)| {
        let width = format!("{:.1}%", (n as f64 / max_reads) * 100.0);
        rsx! {
            div { class: "coverage__row",
                span { class: "coverage__row-name", "{name}" }
                div { class: "coverage__row-bar",
                    div { class: "coverage__row-bar-fill", style: "width: {width}" }
                }
                span { class: "coverage__row-val", "{n}" }
            }
        }
    });

    rsx! {
        div { class: "coverage",
            div { class: "coverage__donut",
                svg { view_box: "0 0 130 130",
                    circle {
                        cx: "65",
                        cy: "65",
                        r: "55",
                        fill: "none",
                        stroke: "var(--bg-sunken)",
                        stroke_width: "12",
                    }
                    circle {
                        cx: "65",
                        cy: "65",
                        r: "55",
                        fill: "none",
                        stroke: "var(--accent)",
                        stroke_width: "12",
                        stroke_linecap: "round",
                        stroke_dasharray: "{circumference:.2}",
                        stroke_dashoffset: "{offset:.2}",
                    }
                }
                div { class: "coverage__donut-center",
                    div { class: "coverage__donut-value", "{covered}/{total_tables}" }
                    div { class: "coverage__donut-label", "Written" }
                }
            }
            div { class: "coverage__list",
                {list}
                {empty_hint}
            }
        }
    }
}
