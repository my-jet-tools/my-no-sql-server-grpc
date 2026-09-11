use dioxus::prelude::*;

#[component]
pub fn NotFound(segments: Vec<String>) -> Element {
    rsx! {
        section { class: "text-gray-700 p-8",
            h1 { class: "text-2xl font-bold", "404: Not Found" }
            p { class: "mt-4", "It's gone 😞" }
        }
    }
}
