use std::time::Duration;

use dioxus::prelude::*;

use crate::api;
use crate::models::ServerApiModel;
use crate::settings::{DEFAULT_BAD_MS, DEFAULT_WARN_MS, HealthThresholds};
use crate::utils::{format_duration_secs, format_moment};

#[derive(Default)]
struct SettingsState {
    warn_ms: String,
    bad_ms: String,
    saving: bool,
    message: Option<String>,
    error: Option<String>,
}

/// What this server is, as `GET /api/Status` describes itself in `server`.
///
/// Read-only on purpose: every field of it comes out of the server's settings
/// file, and there is no route that writes one.
#[derive(Default)]
struct ServerState {
    /// `None` until the first answer arrives, and again if the server stops
    /// answering — the card then says so rather than showing a stale server.
    info: Option<ServerApiModel>,
    loaded: bool,
}

#[derive(Default)]
struct McpWritesState {
    /// True while the server-side enable window is open.
    enabled: bool,
    /// Seconds left in the window (None when disabled).
    remaining_secs: Option<u64>,
    /// Set once, so we start the status poller only on first render.
    loaded: bool,
    saving: bool,
    message: Option<String>,
    error: Option<String>,
}

/// Mirrors `McpWritesState` for the UI's own destructive-write window.
/// Kept separate from the MCP one on purpose — enabling writes for an agent
/// must not silently unlock the delete buttons in the UI.
#[derive(Default)]
struct UiWritesState {
    enabled: bool,
    remaining_secs: Option<u64>,
    saving: bool,
    message: Option<String>,
    error: Option<String>,
}

/// Enables/disables UI writes on the server, then re-reads the authoritative
/// state so the countdown reflects the real window.
fn toggle_ui_writes(mut uiw: Signal<UiWritesState>, enabled: bool) {
    {
        let mut w = uiw.write();
        w.saving = true;
        w.error = None;
        w.message = None;
    }
    spawn(async move {
        match api::set_ui_writes(enabled).await {
            Ok(()) => {
                let server = api::get_ui_settings().await.ok();
                let mut w = uiw.write();
                w.saving = false;
                if let Some(s) = server {
                    w.enabled = s.ui_writes_enabled;
                    w.remaining_secs = s.ui_writes_remaining_secs;
                } else {
                    w.enabled = enabled;
                }
                // The server ADDS the window to whatever was left, so a fixed
                // "for 10 minutes" would be wrong on every extension. Report
                // what the server actually says is left.
                w.message = Some(if enabled {
                    match w.remaining_secs {
                        Some(secs) => format!(
                            "Write access enabled. {} left.",
                            format_duration_secs(secs as f64)
                        ),
                        None => "Write access enabled.".to_string(),
                    }
                } else {
                    "Write access disabled.".to_string()
                });
            }
            Err(err) => {
                let mut w = uiw.write();
                w.saving = false;
                w.error = Some(format!("Failed: {}", err));
            }
        }
    });
}

/// Enables/disables MCP writes on the server, then re-reads the
/// authoritative state so the countdown reflects the real window.
fn toggle_mcp_writes(mut mcp: Signal<McpWritesState>, enabled: bool) {
    {
        let mut w = mcp.write();
        w.saving = true;
        w.error = None;
        w.message = None;
    }
    spawn(async move {
        match api::set_mcp_writes(enabled).await {
            Ok(()) => {
                let server = api::get_ui_settings().await.ok();
                let mut w = mcp.write();
                w.saving = false;
                if let Some(s) = server {
                    w.enabled = s.mcp_writes_enabled;
                    w.remaining_secs = s.mcp_writes_remaining_secs;
                } else {
                    w.enabled = enabled;
                }
                w.message = Some(if enabled {
                    match w.remaining_secs {
                        Some(secs) => format!(
                            "MCP writes enabled. {} left.",
                            format_duration_secs(secs as f64)
                        ),
                        None => "MCP writes enabled.".to_string(),
                    }
                } else {
                    "MCP writes disabled.".to_string()
                });
            }
            Err(err) => {
                let mut w = mcp.write();
                w.saving = false;
                w.error = Some(format!("Failed: {}", err));
            }
        }
    });
}

#[component]
pub fn Settings() -> Element {
    let mut thresholds = use_context::<Signal<HealthThresholds>>();
    let mut cs = use_signal(SettingsState::default);
    let mut mcp = use_signal(McpWritesState::default);
    let mut uiw = use_signal(UiWritesState::default);
    let mut server = use_signal(ServerState::default);

    // Poll both enable states so the countdowns stay in sync and each card
    // flips back to "Disabled" when its 10-minute window lapses. One poller
    // drives both — `/api/Settings` returns them together.
    {
        let loaded = mcp.read().loaded;
        if !loaded {
            mcp.write().loaded = true;
            spawn(async move {
                loop {
                    if let Ok(s) = api::get_ui_settings().await {
                        {
                            let mut w = mcp.write();
                            w.enabled = s.mcp_writes_enabled;
                            w.remaining_secs = s.mcp_writes_remaining_secs;
                        }
                        let mut w = uiw.write();
                        w.enabled = s.ui_writes_enabled;
                        w.remaining_secs = s.ui_writes_remaining_secs;
                    }
                    dioxus_utils::js::sleep(Duration::from_secs(1)).await;
                }
            });
        }
    }

    // The server card is read once, not polled: every field of it comes out of
    // a settings file that cannot change without a restart. The one thing in
    // `server` that does move — the up-time — is on the topbar badge, which is
    // polled anyway because it is drawn on every page; showing a second copy of
    // it here would buy a status call every few seconds for a number already on
    // screen. Retried until it answers, then the task ends.
    {
        let loaded = server.read().loaded;
        if !loaded {
            server.write().loaded = true;
            spawn(async move {
                loop {
                    match api::get_status().await {
                        Ok(status) => {
                            server.write().info = Some(status.server);
                            return;
                        }
                        Err(err) => {
                            dioxus_utils::console_log(format!("Status error: {}", err));
                        }
                    }
                    dioxus_utils::js::sleep(Duration::from_secs(5)).await;
                }
            });
        }
    }

    // Sync local form fields whenever the context value changes (e.g. on initial load).
    let t = *thresholds.read();
    {
        let mut w = cs.write();
        if w.warn_ms.is_empty() {
            w.warn_ms = t.warn_ms.to_string();
        }
        if w.bad_ms.is_empty() {
            w.bad_ms = t.bad_ms.to_string();
        }
    }

    let cs_ra = cs.read();
    let warn_str = cs_ra.warn_ms.clone();
    let bad_str = cs_ra.bad_ms.clone();
    let saving = cs_ra.saving;
    let message = cs_ra.message.clone();
    let error = cs_ra.error.clone();
    drop(cs_ra);

    let save = move |_| {
        let warn_parsed = cs.read().warn_ms.parse::<u32>();
        let bad_parsed = cs.read().bad_ms.parse::<u32>();
        let (Ok(warn_ms), Ok(bad_ms)) = (warn_parsed, bad_parsed) else {
            let mut w = cs.write();
            w.error = Some("Both fields must be positive whole numbers (ms).".to_string());
            w.message = None;
            return;
        };
        if warn_ms >= bad_ms {
            let mut w = cs.write();
            w.error = Some("Green→Yellow threshold must be smaller than Yellow→Red.".to_string());
            w.message = None;
            return;
        }
        {
            let mut w = cs.write();
            w.saving = true;
            w.error = None;
            w.message = None;
        }
        let new_t = HealthThresholds { warn_ms, bad_ms };
        // Applied before the round trip, and applied whatever the round trip
        // answers: these two numbers only colour this UI, so the colouring must
        // not wait for — or be undone by — a server that has nowhere to keep
        // them.
        thresholds.set(new_t);
        spawn(async move {
            match api::set_health_thresholds(new_t).await {
                Ok(()) => {
                    let mut w = cs.write();
                    w.saving = false;
                    w.message = Some("Saved.".to_string());
                }
                Err(err) => {
                    let mut w = cs.write();
                    w.saving = false;
                    // Not an error: this server has no store for the thresholds
                    // and answers `GET /api/Settings` with its own defaults, so
                    // a refused save is the expected answer — and the values are
                    // in force either way.
                    w.message = Some(format!(
                        "Applied to this browser. This server does not store them ({}).",
                        err
                    ));
                }
            }
        });
    };

    let reset = move |_| {
        let mut w = cs.write();
        w.warn_ms = DEFAULT_WARN_MS.to_string();
        w.bad_ms = DEFAULT_BAD_MS.to_string();
        w.error = None;
        w.message = None;
    };

    let footer = if let Some(m) = message.clone() {
        rsx! {
            div { style: "color: var(--ok); font-size: 12px;", "{m}" }
        }
    } else if let Some(e) = error.clone() {
        rsx! {
            div { style: "color: var(--danger); font-size: 12px;", "{e}" }
        }
    } else {
        rsx! {}
    };

    // ----- Server card -----
    let server_info = server.read().info.clone();
    let server_card = match server_info {
        Some(info) => render_server_card(&info),
        None => render_server_card_placeholder(),
    };

    // ----- MCP writes card state & handlers -----
    let mcp_ra = mcp.read();
    let mcp_enabled = mcp_ra.enabled;
    let mcp_remaining_secs = mcp_ra.remaining_secs;
    let mcp_saving = mcp_ra.saving;
    let mcp_message = mcp_ra.message.clone();
    let mcp_error = mcp_ra.error.clone();
    drop(mcp_ra);

    let mcp_enable = move |_| toggle_mcp_writes(mcp, true);
    let mcp_disable = move |_| toggle_mcp_writes(mcp, false);

    let mcp_footer = if let Some(m) = mcp_message.clone() {
        rsx! { div { style: "color: var(--ok); font-size: 12px;", "{m}" } }
    } else if let Some(e) = mcp_error.clone() {
        rsx! { div { style: "color: var(--danger); font-size: 12px;", "{e}" } }
    } else {
        rsx! {}
    };

    let mcp_status_label = if mcp_enabled {
        match mcp_remaining_secs {
            Some(secs) => format!("enabled — ~{} left", format_duration_secs(secs as f64)),
            None => "enabled".to_string(),
        }
    } else {
        "disabled".to_string()
    };
    let mcp_status_color = if mcp_enabled {
        "var(--ok)"
    } else {
        "var(--text-muted)"
    };
    let mcp_remaining_label = match mcp_remaining_secs {
        Some(secs) => format_duration_secs(secs as f64),
        None => "—".to_string(),
    };

    // ----- Write access card state & handlers -----
    let uiw_ra = uiw.read();
    let uiw_enabled = uiw_ra.enabled;
    let uiw_remaining_secs = uiw_ra.remaining_secs;
    let uiw_saving = uiw_ra.saving;
    let uiw_message = uiw_ra.message.clone();
    let uiw_error = uiw_ra.error.clone();
    drop(uiw_ra);

    let uiw_enable = move |_| toggle_ui_writes(uiw, true);
    let uiw_disable = move |_| toggle_ui_writes(uiw, false);

    let uiw_footer = if let Some(m) = uiw_message.clone() {
        rsx! { div { style: "color: var(--ok); font-size: 12px;", "{m}" } }
    } else if let Some(e) = uiw_error.clone() {
        rsx! { div { style: "color: var(--danger); font-size: 12px;", "{e}" } }
    } else {
        rsx! {}
    };

    let uiw_status_label = if uiw_enabled {
        match uiw_remaining_secs {
            Some(secs) => format!("enabled — ~{} left", format_duration_secs(secs as f64)),
            None => "enabled".to_string(),
        }
    } else {
        "disabled".to_string()
    };
    let uiw_status_color = if uiw_enabled {
        "var(--ok)"
    } else {
        "var(--text-muted)"
    };
    let uiw_remaining_label = match uiw_remaining_secs {
        Some(secs) => format_duration_secs(secs as f64),
        None => "—".to_string(),
    };

    rsx! {
        section { class: "page page--padded",
            div { style: "max-width: 640px; display: flex; flex-direction: column; gap: 14px;",

                {server_card}

                div { class: "card",
                    div { class: "card__header",
                        span { class: "card__title", "Reader health thresholds" }
                        span { class: "card__subtitle", "milliseconds since last incoming" }
                    }
                    div { class: "card__body", style: "display: flex; flex-direction: column; gap: 14px;",
                        p { style: "margin: 0; color: var(--text-muted); font-size: 12.5px;",
                            "Below "
                            b { style: "color: var(--ok); font-family: var(--font-mono);", "Green" }
                            " — healthy. Between Green and Yellow — slow. Above "
                            b { style: "color: var(--danger); font-family: var(--font-mono);", "Yellow" }
                            " — stalled. Compared against how long ago each reader last asked for "
                            "changes, which "
                            code { style: "font-family: var(--font-mono);", "/api/Status" }
                            " reports as a number of seconds. They are this browser's two numbers: "
                            "this server keeps no store for them and answers with its own defaults, "
                            "so a change lasts until the page is reloaded."
                        }

                        div { class: "settings-row",
                            label { class: "settings-row__label",
                                span { class: "state state--ok", span { class: "state__dot" } }
                                "Green → Yellow"
                            }
                            div { class: "settings-row__field",
                                input {
                                    class: "filter-input",
                                    r#type: "number",
                                    min: "0",
                                    value: "{warn_str}",
                                    oninput: move |evt| {
                                        let mut w = cs.write();
                                        w.warn_ms = evt.value();
                                        w.message = None;
                                    },
                                }
                                span { class: "settings-row__unit", "ms" }
                            }
                        }

                        div { class: "settings-row",
                            label { class: "settings-row__label",
                                span { class: "state state--bad", span { class: "state__dot" } }
                                "Yellow → Red"
                            }
                            div { class: "settings-row__field",
                                input {
                                    class: "filter-input",
                                    r#type: "number",
                                    min: "0",
                                    value: "{bad_str}",
                                    oninput: move |evt| {
                                        let mut w = cs.write();
                                        w.bad_ms = evt.value();
                                        w.message = None;
                                    },
                                }
                                span { class: "settings-row__unit", "ms" }
                            }
                        }

                        {footer}
                    }
                    div { class: "card__footer", style: "display: flex; justify-content: flex-end; gap: 6px; padding: 10px 14px;",
                        button { class: "btn btn--ghost btn--sm", onclick: reset, "Reset to defaults" }
                        button {
                            class: "btn btn--primary btn--sm",
                            disabled: saving,
                            onclick: save,
                            if saving { "Applying…" } else { "Apply" }
                        }
                    }
                }

                // ----- Write access card (UI writes) -----
                div { class: "card",
                    div { class: "card__header",
                        span { class: "card__title", "Write access" }
                        span {
                            class: "card__subtitle",
                            style: "color: {uiw_status_color};",
                            "{uiw_status_label}"
                        }
                    }
                    div { class: "card__body", style: "display: flex; flex-direction: column; gap: 14px;",
                        p { style: "margin: 0; color: var(--text-muted); font-size: 12.5px;",
                            "Controls destructive operations in this UI ("
                            b { "delete row" }
                            ", "
                            b { "bulk delete" }
                            ", "
                            b { "paste & delete" }
                            ", "
                            b { "restore from backup" }
                            "). They are disabled by default. Click "
                            b { "Enable" }
                            " to allow them for "
                            b { "10 minutes" }
                            "; they auto-disable after that, or click "
                            b { "Disable" }
                            " to turn them off now. A server restart leaves them disabled. "
                            "Browsing data and making backups are always available. "
                            "This window is independent of MCP writes."
                        }

                        if uiw_enabled {
                            div {
                                class: "settings-row",
                                style: "align-items: center;",
                                label { class: "settings-row__label", "Time remaining" }
                                div { class: "settings-row__field",
                                    span {
                                        style: "color: var(--ok); font-family: var(--font-mono); font-weight: 600; font-size: 15px;",
                                        "{uiw_remaining_label}"
                                    }
                                }
                            }
                        }

                        {uiw_footer}
                    }
                    div { class: "card__footer", style: "display: flex; justify-content: flex-end; gap: 6px; padding: 10px 14px;",
                        if uiw_enabled {
                            button {
                                class: "btn btn--ghost btn--sm",
                                disabled: uiw_saving,
                                onclick: uiw_disable,
                                "Disable"
                            }
                            button {
                                class: "btn btn--primary btn--sm",
                                disabled: uiw_saving,
                                onclick: uiw_enable,
                                if uiw_saving { "Working…" } else { "Extend +10 min" }
                            }
                        } else {
                            button {
                                class: "btn btn--primary btn--sm",
                                disabled: uiw_saving,
                                onclick: uiw_enable,
                                if uiw_saving { "Working…" } else { "Enable for 10 min" }
                            }
                        }
                    }
                }

                // ----- MCP writes card -----
                div { class: "card",
                    div { class: "card__header",
                        span { class: "card__title", "MCP writes" }
                        span {
                            class: "card__subtitle",
                            style: "color: {mcp_status_color};",
                            "{mcp_status_label}"
                        }
                    }
                    div { class: "card__body", style: "display: flex; flex-direction: column; gap: 14px;",
                        p { style: "margin: 0; color: var(--text-muted); font-size: 12.5px;",
                            "Controls the MCP write tools ("
                            code { style: "font-family: var(--font-mono);", "delete_row" }
                            ", "
                            code { style: "font-family: var(--font-mono);", "insert_or_replace_row" }
                            ", "
                            code { style: "font-family: var(--font-mono);", "clean_table" }
                            ", …) served on "
                            code { style: "font-family: var(--font-mono);", "/mcp" }
                            ". They are disabled by default, and this button is the only way to open "
                            "them — there is deliberately no tool for it, so an agent cannot open its "
                            "own window. Click "
                            b { "Enable" }
                            " to allow them for "
                            b { "10 minutes" }
                            "; they auto-disable after that, or click "
                            b { "Disable" }
                            " to turn them off now. A server restart leaves them disabled. "
                            "Read-only MCP tools are always available."
                        }

                        if mcp_enabled {
                            div {
                                class: "settings-row",
                                style: "align-items: center;",
                                label { class: "settings-row__label", "Time remaining" }
                                div { class: "settings-row__field",
                                    span {
                                        style: "color: var(--ok); font-family: var(--font-mono); font-weight: 600; font-size: 15px;",
                                        "{mcp_remaining_label}"
                                    }
                                }
                            }
                        }

                        {mcp_footer}
                    }
                    div { class: "card__footer", style: "display: flex; justify-content: flex-end; gap: 6px; padding: 10px 14px;",
                        if mcp_enabled {
                            button {
                                class: "btn btn--ghost btn--sm",
                                disabled: mcp_saving,
                                onclick: mcp_disable,
                                "Disable"
                            }
                            button {
                                class: "btn btn--primary btn--sm",
                                disabled: mcp_saving,
                                onclick: mcp_enable,
                                if mcp_saving { "Working…" } else { "Extend +10 min" }
                            }
                        } else {
                            button {
                                class: "btn btn--primary btn--sm",
                                disabled: mcp_saving,
                                onclick: mcp_enable,
                                if mcp_saving { "Working…" } else { "Enable for 10 min" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One `label: value` line of the server card, in the same markup the editable
/// rows below it use — the card is read-only, so the right-hand side is a value
/// rather than an input.
fn server_row(label: &str, value: String, mono: bool) -> Element {
    let value_style = if mono {
        "font-family: var(--font-mono); font-size: 12.5px; word-break: break-all;"
    } else {
        "font-size: 12.5px;"
    };

    rsx! {
        div { class: "settings-row", style: "align-items: baseline;",
            label { class: "settings-row__label", "{label}" }
            div { class: "settings-row__field",
                span { style: "{value_style}", "{value}" }
            }
        }
    }
}

/// What the server says about itself. Nothing here is editable and nothing here
/// is a metric — the numbers of the running server are the Overview's job, this
/// is the "which server am I looking at" card.
fn render_server_card(server: &ServerApiModel) -> Element {
    let title = format!("{} {}", server.name, server.version)
        .trim()
        .to_string();

    let location = or_dash(&server.location);
    let ports = format!("grpc {} · http {}", server.grpc_port, server.http_port);
    // The moment, not the up-time: the topbar counts the up-time and this card
    // is read once, so a span rendered here would be as old as the page.
    let started = format_moment(&server.started_at);
    let persistence = or_dash(&server.persistence_dest);
    // Server-wide and about the files on disk, not about the rows in memory:
    // this server has no per-table row compression to toggle.
    let on_disk = if server.compress_data {
        "compressed"
    } else {
        "plain"
    };

    let backups = if server.backups.configured {
        // Absent is not zero in either of these. No interval means backups
        // happen when somebody asks for one; no limit means every one of them
        // is kept.
        let interval = match server.backups.interval_secs {
            Some(secs) => format!("every {}", format_duration_secs(secs as f64)),
            None => "on request only".to_string(),
        };
        let retention = match server.backups.max_backups {
            Some(amount) => format!("keep {}", amount),
            None => "keep all".to_string(),
        };
        rsx! {
            span { class: "badge badge--ok", "configured" }
            span { style: "font-size: 12.5px; color: var(--text-muted);", "{interval} · {retention}" }
        }
    } else {
        rsx! {
            span { class: "badge badge--neutral", "not configured" }
            span { style: "font-size: 12.5px; color: var(--text-muted);",
                "no destination named — every backup call says so"
            }
        }
    };

    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Server" }
                span { class: "card__subtitle", "{title}" }
            }
            div { class: "card__body", style: "display: flex; flex-direction: column; gap: 10px;",
                {server_row("Location", location, true)}
                {server_row("Ports", ports, true)}
                // The zone is in the label because `format_moment` deliberately
                // keeps it out of the value — every moment this server sends is
                // UTC, and saying so once beats saying it in every row.
                {server_row("Started (UTC)", started, false)}
                {server_row("Data folder", persistence, true)}
                {server_row("Persisted files", on_disk.to_string(), false)}

                div { class: "settings-row", style: "align-items: baseline;",
                    label { class: "settings-row__label", "Backups" }
                    div { class: "settings-row__field", style: "display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap;",
                        {backups}
                    }
                }
            }
            div { class: "card__footer",
                "Read from "
                code { style: "font-family: var(--font-mono);", "GET /api/Status" }
                " — all of it comes from the server's settings file and is not editable here."
            }
        }
    }
}

/// The same card before the first answer. It says which question is unanswered
/// rather than showing a server made of empty strings.
fn render_server_card_placeholder() -> Element {
    rsx! {
        div { class: "card",
            div { class: "card__header",
                span { class: "card__title", "Server" }
                span { class: "card__subtitle", style: "color: var(--text-muted);", "no answer" }
            }
            div { class: "card__body",
                p { style: "margin: 0; color: var(--text-muted); font-size: 12.5px;",
                    "Waiting for "
                    code { style: "font-family: var(--font-mono);", "GET /api/Status" }
                    "."
                }
            }
        }
    }
}

fn or_dash(value: &str) -> String {
    if value.is_empty() {
        "—".to_string()
    } else {
        value.to_string()
    }
}
