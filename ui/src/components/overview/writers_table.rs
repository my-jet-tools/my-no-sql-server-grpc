use dioxus::prelude::*;

use super::classify_secs_ago;
use crate::components::atoms::{Badge, BadgeTone, StatePill, StateTone};
use crate::models::TransactionApiModel;
use crate::settings::HealthThresholds;
use crate::utils::{format_moment, format_secs_ago};

/// What used to be the writers table. This server has no connected writers to
/// list — a write is a unary gRPC call, over by the time it could be counted —
/// and what it does hold between calls is the open transactions: actions
/// accumulated that no table has seen yet. One that stopped receiving is
/// exactly what an operator is looking for here, hence the last column.
///
/// Server-wide, like the readers table, so the namespace is a column.
#[component]
pub fn TransactionsTable(transactions: Vec<TransactionApiModel>) -> Element {
    let thresholds = *use_context::<Signal<HealthThresholds>>().read();
    if transactions.is_empty() {
        return rsx! {
            div { class: "card",
                div { class: "card__header",
                    span { class: "card__title", "Open transactions" }
                    span { class: "card__subtitle", "0 open" }
                }
                div { class: "card__body",
                    div { style: "color:var(--text-muted); font-size:12px; text-align:center; padding:14px;",
                        "No open transactions"
                    }
                }
            }
        };
    }

    let count = transactions.len();
    let rows = transactions.into_iter().map(|tx| {
        let tone = classify_secs_ago(tx.last_incoming_secs_ago, thresholds);
        let state_label = match tone {
            StateTone::Ok => "live",
            StateTone::Warn => "idle",
            StateTone::Bad => "stalled",
            StateTone::Neutral => "—",
        };

        rsx! {
            tr {
                td { class: "mono muted", "{tx.id}" }
                td { class: "mono muted", "{tx.namespace}" }
                td {
                    span { class: "badge-list",
                        Badge { text: tx.table.clone(), tone: BadgeTone::Writer }
                    }
                }
                td { class: "mono num", "{tx.actions}" }
                td { class: "mono muted", "{format_moment(tx.started_at.as_str())}" }
                td { class: "mono", "{format_secs_ago(tx.last_incoming_secs_ago)}" }
                td {
                    StatePill { label: state_label.to_string(), tone }
                }
            }
        }
    });

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Open transactions" }
                span { class: "card__subtitle", "{count} open" }
            }
            table { class: "dt",
                thead {
                    tr {
                        th { "Id" }
                        th { "Namespace" }
                        th { "Table" }
                        th { class: "num", "Actions" }
                        th { "Started" }
                        th { "Last action" }
                        th { "State" }
                    }
                }
                tbody { {rows} }
            }
        }
    }
}
