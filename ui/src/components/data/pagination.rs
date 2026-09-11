use dioxus::prelude::*;

use super::format_compact_count;

pub const PAGE_SIZE_OPTIONS: &[usize] = &[50, 100, 200, 500];

/// The rows footer. It drives the server now: the page it reports is the window
/// `/api/Row` was asked for, not a slice of a list the browser holds.
#[component]
pub fn TablePagination(
    /// Rows the partition holds, as the per-partition metrics report it. `None`
    /// while nothing has said yet — which happens when the selected partition
    /// is not inside the loaded partitions window — and then the pager works
    /// off `loaded` alone: a full window means there is at least one more page.
    total: Option<usize>,
    /// Rows the current window actually came back with.
    loaded: usize,
    page_size: usize,
    current_page: usize,
    on_page_change: EventHandler<usize>,
    on_page_size_change: EventHandler<usize>,
) -> Element {
    // An empty first page is an empty partition — nothing to page through. A
    // later page that came back empty still gets its pager, because that is
    // what the reader needs in order to get back.
    if loaded == 0 && current_page == 0 && total.unwrap_or(0) == 0 {
        return rsx! {};
    }

    let (info, page_label, page, is_first, is_last, last_page) = match total {
        Some(total) => {
            let total_pages = total.div_ceil(page_size).max(1);
            let page = current_page.min(total_pages - 1);
            let start = page * page_size + 1;
            let end = ((page + 1) * page_size).min(total);
            (
                format!(
                    "Showing {}–{} of {}",
                    start,
                    end,
                    format_compact_count(total as u64)
                ),
                format!("Page {} of {}", page + 1, total_pages),
                page,
                page == 0,
                page + 1 >= total_pages,
                total_pages - 1,
            )
        }
        // No total: the window itself is all there is to go on, so there is no
        // page count to show and no last page to jump to.
        None => {
            let start = current_page * page_size + 1;
            let end = current_page * page_size + loaded;
            (
                format!("Showing {}–{}", start, end),
                format!("Page {}", current_page + 1),
                current_page,
                current_page == 0,
                loaded < page_size,
                current_page,
            )
        }
    };

    let prev_page = if is_first { 0 } else { page - 1 };
    let next_page = if is_last { page } else { page + 1 };

    let size_options = PAGE_SIZE_OPTIONS.iter().map(|&sz| {
        let selected = sz == page_size;
        rsx! {
            option {
                value: "{sz}",
                selected: selected,
                "{sz} / page"
            }
        }
    });

    rsx! {
        div { class: "table-pagination",
            div { class: "table-pagination__info", "{info}" }
            div { class: "table-pagination__spacer" }
            select {
                class: "table-pagination__page-size",
                value: "{page_size}",
                onchange: move |evt| {
                    if let Ok(sz) = evt.value().parse::<usize>() {
                        on_page_size_change.call(sz);
                    }
                },
                {size_options}
            }
            div { class: "table-pagination__controls",
                button {
                    class: "btn btn--sm table-pagination__btn",
                    disabled: is_first,
                    onclick: move |_| on_page_change.call(0),
                    "«"
                }
                button {
                    class: "btn btn--sm table-pagination__btn",
                    disabled: is_first,
                    onclick: move |_| on_page_change.call(prev_page),
                    "‹"
                }
                span { class: "table-pagination__label", "{page_label}" }
                button {
                    class: "btn btn--sm table-pagination__btn",
                    disabled: is_last,
                    onclick: move |_| on_page_change.call(next_page),
                    "›"
                }
                button {
                    class: "btn btn--sm table-pagination__btn",
                    disabled: is_last || last_page == page,
                    onclick: move |_| on_page_change.call(last_page),
                    "»"
                }
            }
        }
    }
}

/// The same bar, cut down to what fits a 240px side pane: a range and the two
/// steps. `/api/Partitions/Details` counts the whole table for us, so the pane
/// can say how far through it the reader is without ever holding the key set.
#[component]
pub fn PanePagination(
    total: usize,
    page: usize,
    page_size: usize,
    on_page_change: EventHandler<usize>,
) -> Element {
    if total <= page_size {
        return rsx! {};
    }

    let total_pages = total.div_ceil(page_size).max(1);
    let page = page.min(total_pages - 1);
    let start = page * page_size + 1;
    let end = ((page + 1) * page_size).min(total);
    let is_first = page == 0;
    let is_last = page + 1 >= total_pages;

    let info = format!("{}–{} / {}", start, end, format_compact_count(total as u64));
    let prev_page = if is_first { 0 } else { page - 1 };
    let next_page = if is_last { page } else { page + 1 };

    rsx! {
        div { class: "table-pagination",
            div { class: "table-pagination__info", "{info}" }
            div { class: "table-pagination__spacer" }
            div { class: "table-pagination__controls",
                button {
                    class: "btn btn--sm table-pagination__btn",
                    disabled: is_first,
                    onclick: move |_| on_page_change.call(prev_page),
                    "‹"
                }
                button {
                    class: "btn btn--sm table-pagination__btn",
                    disabled: is_last,
                    onclick: move |_| on_page_change.call(next_page),
                    "›"
                }
            }
        }
    }
}
