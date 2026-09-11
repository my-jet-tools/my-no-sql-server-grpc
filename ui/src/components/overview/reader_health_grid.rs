use dioxus::prelude::*;

use super::classify_secs_ago;
use crate::components::atoms::StateTone;
use crate::models::ReaderApiModel;
use crate::settings::HealthThresholds;
use crate::utils::format_secs_ago;

#[component]
pub fn ReaderHealthGrid(readers: Vec<ReaderApiModel>) -> Element {
    let thresholds = *use_context::<Signal<HealthThresholds>>().read();
    let min_slots: usize = 100;
    let slots = readers.len().max(min_slots);

    let cells = (0..slots).map(|i| {
        if let Some(r) = readers.get(i) {
            let tone = classify_secs_ago(r.last_incoming_secs_ago, thresholds);
            let cls = match tone {
                StateTone::Ok => "rh-cell rh-cell--ok",
                StateTone::Warn => "rh-cell rh-cell--warn",
                StateTone::Bad => "rh-cell rh-cell--bad",
                StateTone::Neutral => "rh-cell",
            };
            // The cell is one square: the tip is the only place the session can
            // say which namespace it reads and whether chunks are piling up for
            // it, and both are what a coloured square makes somebody ask.
            let tip = format!(
                "{} · {} @ {}  ({} · {} pending)",
                r.id,
                r.name,
                r.namespace,
                format_secs_ago(r.last_incoming_secs_ago),
                r.pending_chunks,
            );
            rsx! {
                div { class: "{cls} has-tip",
                    span { class: "has-tip__tip", "{tip}" }
                }
            }
        } else {
            rsx! {
                div { class: "rh-cell" }
            }
        }
    });

    rsx! {
        div { class: "rh-grid", {cells} }
        div { class: "rh-legend",
            div { class: "rh-legend__item",
                span { class: "rh-legend__swatch", style: "background: var(--ok)" }
                span { "Healthy" }
            }
            div { class: "rh-legend__item",
                span { class: "rh-legend__swatch", style: "background: var(--warn)" }
                span { "Slow" }
            }
            div { class: "rh-legend__item",
                span { class: "rh-legend__swatch", style: "background: var(--danger)" }
                span { "Stalled" }
            }
            div { class: "rh-legend__item",
                span { class: "rh-legend__swatch", style: "background: var(--bg-sunken); border:1px solid var(--border)" }
                span { "Empty slot" }
            }
        }
    }
}
