use dioxus::prelude::*;
use std::collections::HashSet;

use crate::models::TableListItemApiModel;
use crate::utils::format_bytes;

#[component]
pub fn TablesPane(
    tables: Vec<TableListItemApiModel>,
    selected: String,
    /// Tables an open transaction is aimed at. This server has no connected
    /// writer to light the dot with — a write is a unary call that is over by
    /// the moment it could be listed — and an open transaction is the one thing
    /// it does hold while somebody is writing.
    writing_tables: HashSet<String>,
    on_select: EventHandler<String>,
) -> Element {
    let mut filter = use_signal(String::new);
    let filter_ra = filter.read();
    let needle = filter_ra.to_lowercase();
    let needle_empty = needle.is_empty();
    drop(filter_ra);

    let visible: Vec<TableListItemApiModel> = tables
        .into_iter()
        .filter(|t| needle_empty || t.name.to_lowercase().contains(&needle))
        .collect();

    let total = visible.len();

    // Summed over what is on screen, so filtering the list narrows the total
    // too. `/api/Tables/List` carries the metrics itself here, so there is
    // nothing to wait for and no state in which the numbers are missing.
    let total_size: u64 = visible.iter().map(|t| t.data_size).sum();
    let header_count = format!("{} · {}", total, format_bytes(total_size as f64));

    let rows = visible.into_iter().map(|t| {
        let active = t.name == selected;
        let writing = writing_tables.contains(&t.name);
        let cls = if active {
            "tables-pane__item active"
        } else {
            "tables-pane__item"
        };
        let dot_cls = if writing {
            "tables-pane__dot has-writer"
        } else {
            "tables-pane__dot"
        };
        let dot_title = if writing {
            "An open transaction is writing to this table"
        } else {
            "No open transaction"
        };
        let part_str = super::format_compact_count(t.partitions_count);
        let size_str = format_bytes(t.data_size as f64);
        // The pane has room for two numbers; the third lives in the tooltip
        // rather than in a row of its own.
        let count_title = format!("{} partitions · {} rows", t.partitions_count, t.rows_count);
        let name = t.name.clone();
        rsx! {
            div { class: cls, onclick: move |_| on_select.call(name.clone()),
                span { class: dot_cls, title: dot_title }
                span { class: "tables-pane__name", "{t.name}" }
                span { class: "tables-pane__meta",
                    span {
                        class: "tables-pane__count",
                        title: "{count_title}",
                        "{part_str}"
                    }
                    span {
                        class: "tables-pane__size",
                        title: "Data size",
                        "{size_str}"
                    }
                }
            }
        }
    });

    rsx! {
        aside { class: "tables-pane",
            div { class: "pane-header",
                span { class: "pane-header__title", "Tables" }
                span { class: "pane-header__count", "{header_count}" }
            }
            div { class: "pane-filter",
                input {
                    class: "filter-input",
                    placeholder: "filter tables…",
                    value: "{filter.read()}",
                    oninput: move |evt| filter.set(evt.value()),
                }
            }
            div { class: "pane-list", {rows} }
        }
    }
}
