use dioxus::prelude::*;

use crate::components::atoms::{Badge, BadgeTone, Icon, IconKind};

#[component]
pub fn TableToolbar(
    filter_value: Signal<String>,
    /// Open transactions aimed at this table, one pill each. They stand where
    /// the writer pills used to: a write here is a unary call that is over
    /// before it could be listed, and a transaction is the only writing this
    /// server holds open long enough to name.
    transaction_tags: Vec<String>,
    reader_count: usize,
    on_export: EventHandler<()>,
    export_enabled: bool,
    on_paste_delete: EventHandler<()>,
    paste_enabled: bool,
) -> Element {
    let transaction_pills = transaction_tags.into_iter().map(|tag| {
        rsx! {
            Badge { text: tag, tone: BadgeTone::Writer }
        }
    });

    rsx! {
        div { class: "table-toolbar-new",
            // The filter narrows the loaded page, not the table: the rows come
            // a window at a time now, and the server has no row search to hand
            // a needle to.
            input {
                class: "filter-input",
                placeholder: "filter this page… e.g. Status=\"ACTIVE\"",
                value: "{filter_value.read()}",
                oninput: move |evt| filter_value.set(evt.value()),
            }
            div { class: "table-toolbar-new__spacer" }
            div { class: "table-toolbar-new__group",
                {transaction_pills}
            }
            div { class: "table-toolbar-new__group",
                Badge { text: format!("{reader_count} readers"), tone: BadgeTone::Reader }
            }
            button {
                class: "btn btn--sm",
                disabled: !paste_enabled,
                onclick: move |_| on_paste_delete.call(()),
                Icon { kind: IconKind::Layers }
                "Paste & delete"
            }
            button {
                class: "btn btn--sm",
                disabled: !export_enabled,
                onclick: move |_| on_export.call(()),
                Icon { kind: IconKind::Download }
                "Export"
            }
        }
    }
}
