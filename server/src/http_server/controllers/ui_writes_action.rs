use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::as_json;

use super::models::UiWritesInputContract;

/// Opens - or shuts - the destructive half of the UI.
///
/// The window beside the MCP one, and it is deliberately a second window: the
/// person who lets an agent write has not thereby armed the delete buttons of
/// the dashboard, and a single switch is a switch thrown for one reason and
/// forgotten for the other.
///
/// What it gates is the **UI**, not the server. The dashboard deletes through
/// the same public routes an SDK application writes through, so those routes
/// can not refuse a UI which is not allowed to write without refusing every
/// writer; the window is what the UI asks before it lets a click through. That
/// makes this an operator guardrail against the accidental click, not a
/// security boundary - the boundary is `ApiKey`, and it stands in front of this
/// route like it stands in front of every other.
///
/// Only a human call opens it: it is an HTTP route and no MCP tool, and the
/// window is held in memory, so a restart leaves the writes shut.
#[http_route(
    method: "POST",
    route: "/api/Settings/UiWrites",
    controller: "Settings",
    description: "Opens the destructive UI operations for 10 minutes, or shuts them at once. Held in memory only - a restart leaves them shut",
    summary: "Opens or shuts the destructive UI operations",
    input_data: "UiWritesInputContract",
    result:[
        {status_code: 200, description: "Whether the writes are open and for how much longer"},
    ]
)]
pub struct UiWritesAction {
    app: Arc<AppContext>,
}

impl UiWritesAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &UiWritesAction,
    input_data: UiWritesInputContract,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let now = DateTimeAsMicroseconds::now();

    if input_data.enabled {
        action.app.ui_writes.open(now);
    } else {
        action.app.ui_writes.close();
    }

    // Read back rather than assumed: the window says how long it has, and the
    // caller which just opened it gets the same number as the caller which only
    // asked.
    as_json(render(action.app.ui_writes.remaining_secs(now))).into_ok_result(true)
}

/// The same shape whichever way the switch was thrown, and the same two names
/// `POST /api/Mcp/Writes` answers with: two switches over one kind of window
/// have no business being read two ways.
fn render(remaining_secs: Option<i64>) -> String {
    JsonObjectWriter::new()
        .write("enabled", remaining_secs.is_some())
        .write_if_some("remainingSecs", remaining_secs)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opened_window_answers_with_what_is_left() {
        assert_eq!(render(Some(600)), r#"{"enabled":true,"remainingSecs":600}"#);
    }

    /// Shut is the whole answer: there is no time left to name, and `0` would
    /// have to be told apart from "expired this instant".
    #[test]
    fn a_shut_window_names_no_time() {
        assert_eq!(render(None), r#"{"enabled":false}"#);
    }
}
