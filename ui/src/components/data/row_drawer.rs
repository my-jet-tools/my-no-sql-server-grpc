use dioxus::prelude::*;
use serde_json::Value;

use super::cell::{cell_class, cell_string, pretty_json_html};
use crate::components::atoms::{Icon, IconKind};

#[derive(Clone, Copy, PartialEq, Default)]
pub enum DrawerView {
    #[default]
    Form,
    Json,
}

#[component]
pub fn RowDrawer(row: Value, on_close: EventHandler<()>, on_delete: EventHandler<()>) -> Element {
    let mut view = use_signal(|| DrawerView::Form);
    let view_val = *view.read();

    let body = match view_val {
        DrawerView::Form => render_form(&row),
        DrawerView::Json => render_json(&row),
    };

    let json_string = serde_json::to_string_pretty(&row).unwrap_or_default();
    let copy_handler = move |_| {
        copy_to_clipboard(&json_string);
    };

    let form_cls = if matches!(view_val, DrawerView::Form) {
        "btn-group__item active"
    } else {
        "btn-group__item"
    };
    let json_cls = if matches!(view_val, DrawerView::Json) {
        "btn-group__item active"
    } else {
        "btn-group__item"
    };

    rsx! {
        aside { class: "row-drawer",
            div { class: "row-drawer__header",
                span { class: "row-drawer__title", "Row Detail" }
                div { class: "btn-group",
                    button {
                        class: form_cls,
                        onclick: move |_| view.set(DrawerView::Form),
                        "Form"
                    }
                    button {
                        class: json_cls,
                        onclick: move |_| view.set(DrawerView::Json),
                        "JSON"
                    }
                }
                button {
                    class: "topbar__icon-btn",
                    onclick: move |_| on_close.call(()),
                    Icon { kind: IconKind::X }
                }
            }
            div { class: "row-drawer__body", {body} }
            // No Edit here. Editing a row means writing an entity, and an
            // entity needs its schema — which travels with the write, on gRPC.
            // The HTTP surface this page talks to takes keys and attributes,
            // never an entity, so a button for it could never be wired up.
            div { class: "row-drawer__footer",
                button { class: "btn btn--ghost btn--sm", onclick: copy_handler,
                    Icon { kind: IconKind::Copy }
                    "Copy JSON"
                }
                button {
                    class: "btn btn--danger btn--sm",
                    onclick: move |_| on_delete.call(()),
                    "Delete"
                }
            }
        }
    }
}

fn render_form(row: &Value) -> Element {
    let Value::Object(map) = row else {
        return rsx! {
            pre { class: "row-drawer__json", "{row}" }
        };
    };

    let fields = map.iter().map(|(k, v)| {
        let s = cell_string(v);
        let mut cls = format!("row-drawer__value {}", cell_class(k, v));
        if k == "PartitionKey" {
            cls = "row-drawer__value pk".to_string();
        } else if k == "RowKey" {
            cls = "row-drawer__value rk".to_string();
        }
        rsx! {
            div { class: "row-drawer__field",
                div { class: "row-drawer__label", "{k}" }
                div { class: cls, "{s}" }
            }
        }
    });

    rsx! { div { {fields} } }
}

fn render_json(row: &Value) -> Element {
    let html = pretty_json_html(row);
    rsx! {
        pre { class: "row-drawer__json", dangerous_inner_html: "{html}" }
    }
}

fn copy_to_clipboard(text: &str) {
    if let Some(window) = web_sys::window() {
        let clipboard = window.navigator().clipboard();
        let _ = clipboard.write_text(text);
    }
}
