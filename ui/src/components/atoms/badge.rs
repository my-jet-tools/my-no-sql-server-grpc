use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum BadgeTone {
    Writer,
    Reader,
    Ok,
    Warn,
    Bad,
    Neutral,
}

impl BadgeTone {
    fn class(&self) -> &'static str {
        match self {
            BadgeTone::Writer => "badge--writer",
            BadgeTone::Reader => "badge--reader",
            BadgeTone::Ok => "badge--ok",
            BadgeTone::Warn => "badge--warn",
            BadgeTone::Bad => "badge--bad",
            BadgeTone::Neutral => "badge--neutral",
        }
    }
}

#[component]
pub fn Badge(text: String, tone: BadgeTone) -> Element {
    rsx! {
        span { class: "badge {tone.class()}", "{text}" }
    }
}
