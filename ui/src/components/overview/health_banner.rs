use dioxus::prelude::*;

use crate::components::atoms::{Icon, IconKind};

#[derive(Clone, Copy, PartialEq)]
pub enum HealthTone {
    Ok,
    Warn,
    Bad,
}

#[component]
pub fn HealthBanner(tone: HealthTone, headline: String, sub: String, uptime: String) -> Element {
    let (cls, icon) = match tone {
        HealthTone::Ok => ("health", IconKind::ShieldCheck),
        HealthTone::Warn => ("health health--warn", IconKind::AlertTriangle),
        HealthTone::Bad => ("health health--bad", IconKind::AlertTriangle),
    };

    rsx! {
        section { class: cls,
            div { class: "health__icon",
                Icon { kind: icon }
            }
            div { class: "health__text",
                div { class: "health__headline", "{headline}" }
                div { class: "health__sub", "{sub}" }
            }
            div { class: "health__uptime",
                span { class: "health__uptime-label", "Uptime" }
                "{uptime}"
            }
        }
    }
}
