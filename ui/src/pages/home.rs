use std::time::Duration;

use dioxus::prelude::*;

use crate::AppContext;
use crate::api::get_status;
use crate::components::atoms::StateTone;
use crate::components::overview::{
    HealthBanner, HealthTone, ReaderHealthGrid, ReadersTable, StatsRow, TableCoverage,
    TransactionsTable, classify_secs_ago,
};
use crate::models::{
    DEFAULT_NAMESPACE, NamespaceStatusApiModel, ReaderApiModel, StatusApiModel, TransactionApiModel,
};
use crate::settings::HealthThresholds;
use crate::utils::format_duration_secs;

#[component]
pub fn Home() -> Element {
    let mut data = use_signal(|| None::<StatusApiModel>);
    let mut started = use_signal(|| false);
    let app_ctx = use_context::<Signal<AppContext>>();

    let started_val = *started.read();
    let on_mount = move |_| {
        if started_val {
            return;
        }
        *started.write() = true;
        let mut ctx = app_ctx;
        spawn(async move {
            loop {
                match get_status().await {
                    Ok(result) => {
                        ctx.write().status = Some(result.clone());
                        data.set(Some(result));
                    }
                    Err(err) => {
                        dioxus_utils::console_log(format!("Status error: {}", err));
                        ctx.write().status = None;
                        data.set(None);
                    }
                }
                dioxus_utils::js::sleep(Duration::from_secs(1)).await;
            }
        });
    };

    let snapshot = data.read().clone();
    let thresholds = *use_context::<Signal<HealthThresholds>>().read();

    // There is no "initializing" answer on this server: /api/Status is complete
    // whenever it answers at all, so either we have it or we are not connected.
    let content = match snapshot {
        Some(status) => render_overview(status, thresholds),
        None => render_loading_msg("Connecting to server…"),
    };

    rsx! {
        section { class: "page page--padded", onmounted: on_mount,
            div { class: "overview", {content} }
        }
    }
}

fn render_loading_msg(msg: &str) -> Element {
    rsx! {
        div { class: "empty-state",
            div { class: "empty-state__title", "{msg}" }
        }
    }
}

fn render_overview(status: StatusApiModel, thresholds: HealthThresholds) -> Element {
    // Status is reported per namespace, so the page has to say which one it is
    // showing. An empty selection means the default namespace — that is what
    // the api layer works in when it sends no `ns` header. A namespace the
    // server has never had to create is simply not in the list, and it is shown
    // as itself and empty rather than silently swapped for somebody else's.
    let selected =
        crate::storage::load_namespace().unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
    let namespace = match status.namespace(Some(selected.as_str())) {
        Some(found) => found.clone(),
        None => NamespaceStatusApiModel {
            name: selected,
            ..Default::default()
        },
    };

    let readers = status.readers.clone();
    let transactions = status.transactions.clone();
    let reader_count = readers.len();

    // A table name only means something inside its namespace, so coverage
    // counts the sessions of the namespace on screen. The grid and the two
    // tables below are about the server's sessions and stay server-wide.
    let namespace_readers: Vec<ReaderApiModel> = readers
        .iter()
        .filter(|reader| reader.namespace == namespace.name)
        .cloned()
        .collect();

    let (tone, headline, sub) = compute_health(&readers, &transactions, thresholds);
    let uptime = format_duration_secs(status.server.up_time_secs);
    let namespace_name = namespace.name.clone();

    rsx! {
        HealthBanner { tone, headline, sub, uptime }
        StatsRow {
            version: status.server.version.clone(),
            location: status.server.location.clone(),
            up_time_secs: status.server.up_time_secs,
            namespaces_count: status.namespaces.len(),
            namespace: namespace.clone(),
            readers: readers.clone(),
            transactions: transactions.clone(),
            mcp_writes: status.server.mcp_writes.clone(),
        }
        div { class: "two-col",
            div { class: "card",
                div { class: "card__header",
                    span { class: "card__title", "Reader health" }
                    span { class: "card__subtitle", "live · {reader_count} clients" }
                }
                div { class: "card__body",
                    ReaderHealthGrid { readers: readers.clone() }
                }
            }
            div { class: "card",
                div { class: "card__header",
                    span { class: "card__title", "Table coverage" }
                    span { class: "card__subtitle", "{namespace_name} · written since restart" }
                }
                div { class: "card__body",
                    TableCoverage { tables: namespace.tables.clone(), readers: namespace_readers }
                }
            }
        }
        TransactionsTable { transactions }
        ReadersTable { readers }
    }
}

fn compute_health(
    readers: &[ReaderApiModel],
    transactions: &[TransactionApiModel],
    thresholds: HealthThresholds,
) -> (HealthTone, String, String) {
    let mut bad = 0usize;
    let mut warn = 0usize;
    for r in readers {
        match classify_secs_ago(r.last_incoming_secs_ago, thresholds) {
            StateTone::Bad => bad += 1,
            StateTone::Warn => warn += 1,
            _ => {}
        }
    }

    // An open transaction that stopped receiving actions belongs in the verdict
    // as much as a stalled reader: it is holding rows that no table has yet.
    let idle = transactions
        .iter()
        .filter(|tx| {
            matches!(
                classify_secs_ago(tx.last_incoming_secs_ago, thresholds),
                StateTone::Bad
            )
        })
        .count();

    // The wording quotes the configured threshold rather than a hardcoded ten
    // seconds — the Settings page can move it.
    let bad_secs = thresholds.bad_ms as f64 / 1_000.0;

    if bad > 0 {
        (
            HealthTone::Bad,
            format!("{} reader{} stalled", bad, if bad == 1 { "" } else { "s" }),
            format!(
                "A reader stream has not asked for changes for over {:.0}s.",
                bad_secs
            ),
        )
    } else if idle > 0 {
        (
            HealthTone::Warn,
            format!(
                "{} transaction{} idle",
                idle,
                if idle == 1 { "" } else { "s" }
            ),
            format!(
                "An open transaction has taken no action for over {:.0}s — its rows are in no table yet.",
                bad_secs
            ),
        )
    } else if warn > 0 {
        (
            HealthTone::Warn,
            format!("{} reader{} slow", warn, if warn == 1 { "" } else { "s" }),
            "Some reader streams are lagging behind the live window.".to_string(),
        )
    } else {
        (
            HealthTone::Ok,
            "All systems nominal".to_string(),
            "Every reader is inside its window and no transaction is waiting.".to_string(),
        )
    }
}
