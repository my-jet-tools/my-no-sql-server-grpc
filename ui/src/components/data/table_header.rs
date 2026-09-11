use dioxus::prelude::*;

use crate::components::atoms::{Badge, BadgeTone, Icon, IconKind};
use crate::models::TableApiModel;
use crate::utils::format_bytes;

#[component]
pub fn TableHeader(
    name: String,
    stats: Option<TableApiModel>,
    on_refresh: EventHandler<()>,
) -> Element {
    // Whether the table reaches the disk at all — the one property of a table
    // worth saying beside its name, and the only one whose wrong value costs
    // data. It is a statement and not a control: changing an attribute is not
    // something this page offers.
    let persist = match stats.as_ref() {
        Some(t) => {
            let (text, tone) = if t.persist {
                ("persist", BadgeTone::Neutral)
            } else {
                ("in-memory", BadgeTone::Warn)
            };
            rsx! {
                Badge { text: text.to_string(), tone }
            }
        }
        None => rsx! {},
    };

    let meta = if let Some(t) = stats {
        let size = format_bytes(t.data_size as f64);
        let created = short_moment(&t.created);
        // Absent means nothing has written to this table since the server
        // started — the moment is not kept on disk, so a restart forgets it
        // rather than reporting the previous run's.
        let last_write = t
            .last_write_at
            .as_deref()
            .map(short_moment)
            .unwrap_or_else(|| "—".to_string());
        // Absent or zero is "no limit", and a limit nobody set is not a fact
        // about the table.
        let max_partitions = match t.max_partitions_amount.filter(|v| *v > 0) {
            Some(max) => rsx! {
                div { class: "table-header__meta-item",
                    "max partitions: "
                    b { "{max}" }
                }
            },
            None => rsx! {},
        };
        let max_rows = match t.max_rows_per_partition_amount.filter(|v| *v > 0) {
            Some(max) => rsx! {
                div { class: "table-header__meta-item",
                    "max rows/partition: "
                    b { "{max}" }
                }
            },
            None => rsx! {},
        };
        rsx! {
            div { class: "table-header__meta-item",
                "rows: "
                b { "{t.rows_count}" }
            }
            div { class: "table-header__meta-item",
                "partitions: "
                b { "{t.partitions_count}" }
            }
            div { class: "table-header__meta-item",
                "size: "
                b { "{size}" }
            }
            // One entity per table is the normal case, so anything but 1 here is
            // worth seeing: either a deploy is going through or two different
            // entities are aimed at one table.
            div { class: "table-header__meta-item",
                "schemas: "
                b { "{t.schemas_count}" }
            }
            {max_partitions}
            {max_rows}
            div { class: "table-header__meta-item",
                "created: "
                b { "{created}" }
            }
            div { class: "table-header__meta-item",
                "last write: "
                b { "{last_write}" }
            }
        }
    } else {
        rsx! {
            div { class: "table-header__meta-item muted", "loading…" }
        }
    };

    rsx! {
        div { class: "table-header",
            div { class: "table-header__name",
                span { class: "table-header__title", "{name}" }
                {persist}
            }
            div { class: "table-header__meta", {meta} }
            div { class: "table-header__actions",
                button {
                    class: "topbar__icon-btn",
                    title: "Refresh",
                    onclick: move |_| on_refresh.call(()),
                    Icon { kind: IconKind::RefreshCw }
                }
                button { class: "topbar__icon-btn",
                    Icon { kind: IconKind::MoreHorizontal }
                }
            }
        }
    }
}

/// `2026-02-03T14:05:06.123456+00:00` → `2026-02-03 14:05:06`. The moments
/// arrive as RFC3339 from this server rather than as unix microseconds, and
/// seconds are as much of them as a header row can use.
fn short_moment(value: &str) -> String {
    let head: String = value.chars().take(19).collect();

    // Anything shorter than `YYYY-MM-DDTHH:MM:SS` is not a moment we recognise,
    // and showing it whole beats showing a slice of it.
    if head.chars().count() < 19 {
        return value.to_string();
    }

    head.replace('T', " ")
}
