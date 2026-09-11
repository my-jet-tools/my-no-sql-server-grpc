use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum StateTone {
    Ok,
    Warn,
    Bad,
    Neutral,
}

impl StateTone {
    fn class(&self) -> &'static str {
        match self {
            StateTone::Ok => "state--ok",
            StateTone::Warn => "state--warn",
            StateTone::Bad => "state--bad",
            StateTone::Neutral => "state--neutral",
        }
    }
}

#[component]
pub fn StatePill(label: String, tone: StateTone) -> Element {
    rsx! {
        span { class: "state {tone.class()}",
            span { class: "state__dot" }
            span { "{label}" }
        }
    }
}

#[component]
pub fn StatusDot(tone: StateTone) -> Element {
    rsx! {
        span { class: "state {tone.class()}",
            span { class: "state__dot" }
        }
    }
}
