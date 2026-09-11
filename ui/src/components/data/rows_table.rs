use std::collections::HashSet;

use dioxus::prelude::*;
use serde_json::Value;

use super::cell::{cell_class, cell_string};

pub const PARTITION_KEY: &str = "PartitionKey";
pub const ROW_KEY: &str = "RowKey";
pub const TIME_STAMP: &str = "TimeStamp";

/// Publishes the PartitionKey column's laid-out width as `--rt-pk-w` on the
/// table, because the RowKey column's sticky `left` is offset by it and CSS
/// cannot read a content-sized column's width.
///
/// Run on mount, then kept current by a `ResizeObserver`: the column resizes
/// whenever the rows change, a longer key appears, the webfont finishes
/// loading, or the window is resized — and every one of those would otherwise
/// leave RowKey frozen at a stale offset. The observer is re-attached on each
/// run (the previous one may be watching a detached node) and stored on the
/// table element so it never accumulates.
const FROZEN_WIDTH_SYNC_JS: &str = r#"(function () {
    const table = document.querySelector('table.rt');
    if (!table) return;
    const sync = () => {
        const pk = table.querySelector('thead th.pk');
        const w = pk ? pk.getBoundingClientRect().width : 0;
        table.style.setProperty('--rt-pk-w', w + 'px');
    };
    sync();
    if (window.ResizeObserver) {
        if (table.__rtFrozenObs) table.__rtFrozenObs.disconnect();
        const obs = new ResizeObserver(sync);
        const pk = table.querySelector('thead th.pk');
        if (pk) obs.observe(pk);
        table.__rtFrozenObs = obs;
    }
})();"#;

#[component]
pub fn RowsTable(
    headers: Vec<String>,
    rows: Vec<Value>,
    selected_row_key: Option<String>,
    on_row_click: EventHandler<Value>,
    #[props(default)] selectable: bool,
    #[props(default)] checked_keys: HashSet<String>,
    #[props(default)] on_toggle_row: EventHandler<String>,
    #[props(default)] on_toggle_all: EventHandler<bool>,
) -> Element {
    if rows.is_empty() {
        return rsx! {
            div { class: "rows-wrap",
                div { class: "empty-state",
                    div { class: "empty-state__title", "No rows" }
                    div { class: "empty-state__sub", "This partition is empty." }
                }
            }
        };
    }

    let total = rows.len();
    let visible_count = rows
        .iter()
        .filter(|r| {
            r.get(ROW_KEY)
                .and_then(|v| v.as_str())
                .map(|s| checked_keys.contains(s))
                .unwrap_or(false)
        })
        .count();
    let all_checked = visible_count == total && total > 0;

    // The PartitionKey / RowKey headers carry the same `pk` / `rk` classes as
    // their body cells, so the frozen-column rules line up across thead+tbody.
    let header_cells = headers.iter().map(|h| {
        let cls = match h.as_str() {
            PARTITION_KEY => "pk",
            ROW_KEY => "rk",
            TIME_STAMP => "num",
            _ => "",
        };
        rsx! { th { class: cls, "{h}" } }
    });

    let body_rows = rows.iter().enumerate().map(|(i, row)| {
        let rk = row
            .get(ROW_KEY)
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();

        let is_selected = selected_row_key.as_ref() == Some(&rk);
        let is_checked = checked_keys.contains(&rk);

        let cells = headers.iter().map(|h| {
            let v = row.get(h.as_str()).cloned().unwrap_or(Value::Null);
            let mut cls = cell_class(h, &v).to_string();
            let mut is_num = false;
            if matches!(&v, Value::Number(_)) {
                is_num = true;
            }
            if h == PARTITION_KEY {
                cls = "pk".to_string();
            } else if h == ROW_KEY {
                cls = "rk".to_string();
            }
            if is_num {
                cls.push_str(" num");
            }
            let s = cell_string(&v);
            rsx! {
                td { class: "{cls}", title: "{s}", "{s}" }
            }
        });

        let row_val = row.clone();
        let row_cls = if is_selected { "selected" } else { "" };
        let rk_for_toggle = rk.clone();

        let check_cell = if selectable {
            rsx! {
                td {
                    class: "rt-check",
                    onclick: move |evt| {
                        evt.stop_propagation();
                        on_toggle_row.call(rk_for_toggle.clone());
                    },
                    input {
                        r#type: "checkbox",
                        checked: is_checked,
                    }
                }
            }
        } else {
            rsx! {}
        };

        rsx! {
            tr {
                key: "{i}",
                class: row_cls,
                onclick: move |_| on_row_click.call(row_val.clone()),
                {check_cell}
                {cells}
            }
        }
    });

    let head_check = if selectable {
        rsx! {
            th {
                class: "rt-check",
                onclick: move |_| on_toggle_all.call(!all_checked),
                input {
                    r#type: "checkbox",
                    checked: all_checked,
                }
            }
        }
    } else {
        rsx! {}
    };

    // Tells the CSS whether a checkbox column exists, since it shifts the
    // `left` offset of every frozen column to its right.
    let table_cls = if selectable {
        "rt rt--selectable"
    } else {
        "rt"
    };

    rsx! {
        div { class: "rows-wrap",
            table {
                class: "{table_cls}",
                onmounted: move |_| {
                    let _ = dioxus::document::eval(FROZEN_WIDTH_SYNC_JS);
                },
                thead {
                    tr {
                        {head_check}
                        {header_cells}
                    }
                }
                tbody { {body_rows} }
            }
        }
    }
}
