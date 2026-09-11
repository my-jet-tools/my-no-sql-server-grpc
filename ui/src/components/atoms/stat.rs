use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq)]
pub enum StatTone {
    Ok,
    Warn,
    Bad,
    Info,
}

impl StatTone {
    fn class(&self) -> &'static str {
        match self {
            StatTone::Ok => "stat--ok",
            StatTone::Warn => "stat--warn",
            StatTone::Bad => "stat--bad",
            StatTone::Info => "stat--info",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum DeltaTone {
    Neutral,
    Ok,
    Warn,
    Bad,
}

impl DeltaTone {
    fn class(&self) -> Option<&'static str> {
        match self {
            DeltaTone::Neutral => None,
            DeltaTone::Ok => Some("stat__delta--ok"),
            DeltaTone::Warn => Some("stat__delta--warn"),
            DeltaTone::Bad => Some("stat__delta--bad"),
        }
    }
}

#[component]
pub fn Stat(
    label: String,
    value: String,
    #[props(default = String::new())] unit: String,
    #[props(default = String::new())] delta: String,
    #[props(default = DeltaTone::Neutral)] delta_tone: DeltaTone,
    tone: StatTone,
) -> Element {
    let unit_el = if unit.is_empty() {
        rsx! {}
    } else {
        rsx! {
            span { class: "stat__unit", "{unit}" }
        }
    };

    let delta_el = if delta.is_empty() {
        rsx! {}
    } else {
        let cls = match delta_tone.class() {
            Some(c) => format!("stat__delta {}", c),
            None => "stat__delta".to_string(),
        };
        rsx! {
            div { class: "{cls}", "{delta}" }
        }
    };

    rsx! {
        div { class: "stat {tone.class()}",
            div { class: "stat__label", "{label}" }
            div { class: "stat__value",
                "{value}"
                {unit_el}
            }
            {delta_el}
        }
    }
}
