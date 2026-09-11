use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::app::ui_settings;
use crate::http_server::as_json;

use super::models::SetSettingsInputContract;

/// Saves the two reader health thresholds the UI paints its status colours by.
///
/// The whole of what a page may store on this server, and deliberately so: they
/// are a statement about this deployment - what counts as slow here - and the
/// next person to open the page should see what the last one decided. Anything
/// that is one browser's preference belongs in that browser, not in a file next
/// to the data.
///
/// It answers the same document `GET /api/Settings` does, so the page can take
/// the answer as its new state instead of asking again.
#[http_route(
    method: "POST",
    route: "/api/Settings",
    controller: "Settings",
    description: "Saves the UI health thresholds - `warnMs` and `badMs`, in milliseconds - and answers the settings as they now are. An omitted field keeps its stored value",
    summary: "Saves the UI health thresholds",
    input_data: "SetSettingsInputContract",
    result:[
        {status_code: 200, description: "Settings as they now are"},
        {status_code: 400, description: "The body is not an object of `warnMs` / `badMs` numbers, or the file could not be written"},
    ]
)]
pub struct SetSettingsAction {
    app: Arc<AppContext>,
}

impl SetSettingsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &SetSettingsAction,
    input_data: SetSettingsInputContract,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let persistence_dest = action.app.settings.get_persistence_dest();

    // Merged onto what is stored, not onto the defaults: a body carrying one of
    // the two numbers must not quietly reset the other.
    let mut ui_settings = ui_settings::load(persistence_dest.as_str()).await;

    let arriving = ui_settings::parse_patch(&input_data.body, ui_settings)
        .map_err(|err| HttpFailResult::as_fatal_error(format!("Invalid body: {err}")))?;

    ui_settings = ui_settings::save(persistence_dest.as_str(), arriving)
        .await
        .map_err(|err| HttpFailResult::as_fatal_error(format!("Failed to save settings: {err}")))?;

    let now = DateTimeAsMicroseconds::now();

    let json = super::get_settings_action::render(
        &action.app.settings,
        action.app.backups.is_configured(),
        action.app.mcp_writes_remaining_secs(now),
        action.app.ui_writes.remaining_secs(now),
        ui_settings,
    );

    as_json(json).into_ok_result(true)
}
