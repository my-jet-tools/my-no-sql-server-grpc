use dioxus::prelude::*;

use super::{PanePagination, format_compact_count};
use crate::models::PartitionMetricApiModel;

#[component]
pub fn PartitionsPane(
    /// The window `/api/Partitions/Details` answered with, in the table's own
    /// partition order, each entry carrying its own records count and size —
    /// so there is no metric here that has to be guessed at.
    partitions: Vec<PartitionMetricApiModel>,
    /// Partitions the whole table holds. This is what the pager counts, and it
    /// is the number the pane titles itself with.
    total: usize,
    page: usize,
    page_size: usize,
    selected: Option<String>,
    on_select: EventHandler<String>,
    on_page_change: EventHandler<usize>,
) -> Element {
    let mut filter = use_signal(String::new);
    let filter_ra = filter.read();
    let needle = filter_ra.to_lowercase();
    let needle_empty = needle.is_empty();
    drop(filter_ra);

    let visible: Vec<PartitionMetricApiModel> = partitions
        .into_iter()
        .filter(|p| needle_empty || p.partition_key.to_lowercase().contains(&needle))
        .collect();

    // The filter narrows the loaded window, not the table — the server has no
    // partition-key search — so while one is typed the heading says which of
    // the two numbers it is showing.
    let heading = if needle_empty {
        format!("Partitions · {}", format_compact_count(total as u64))
    } else {
        format!(
            "Partitions · {} of {}",
            visible.len(),
            format_compact_count(total as u64)
        )
    };

    let rows = visible.into_iter().map(|metric| {
        let active = selected.as_ref() == Some(&metric.partition_key);
        let cls = if active {
            "partitions-pane__item active"
        } else {
            "partitions-pane__item"
        };
        let records_str = format_compact_count(metric.records_count);
        let size_str = crate::utils::format_bytes(metric.data_size as f64);
        let pk = metric.partition_key.clone();
        let pk_for_handler = metric.partition_key.clone();
        rsx! {
            div { class: cls, onclick: move |_| on_select.call(pk_for_handler.clone()),
                span { class: "partitions-pane__name", "{pk}" }
                span { class: "partitions-pane__meta",
                    span {
                        class: "partitions-pane__count",
                        title: "Records",
                        "{records_str}"
                    }
                    span {
                        class: "partitions-pane__size",
                        title: "Size in bytes",
                        "{size_str}"
                    }
                }
            }
        }
    });

    rsx! {
        aside { class: "partitions-pane",
            div { class: "pane-header",
                span { class: "pane-header__title", "{heading}" }
            }
            div { class: "pane-filter",
                input {
                    class: "filter-input",
                    placeholder: "filter this page…",
                    value: "{filter.read()}",
                    oninput: move |evt| filter.set(evt.value()),
                }
            }
            div { class: "pane-list", {rows} }
            PanePagination {
                total,
                page,
                page_size,
                on_page_change: move |p| on_page_change.call(p),
            }
        }
    }
}
