use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::as_json;

use super::models::McpWritesInputContract;

/// Opens - or shuts - the write half of the MCP surface.
///
/// The JSON version has a button in its own UI behind this; this server serves
/// no UI, so the route *is* the switch and a person calls it. That still gates
/// the model, and gates it on the only thing that matters: an MCP client can
/// call the tools this server registers and nothing else, so a route which is
/// not a tool is a route the model has no way to reach.
///
/// It is a normal HTTP route, which means the ApiKey middleware guards it like
/// everything else. Whoever is entitled to empty a table over
/// `POST /api/Tables/Clean` is entitled to open this window, and that is the
/// right pairing: the window does not hand out authority, it decides whether an
/// agent may use the authority its operator already has.
#[http_route(
    method: "POST",
    route: "/api/Mcp/Writes",
    controller: "Mcp",
    description: "Opens the MCP write tools for 10 minutes, or shuts them at once. Held in memory only - a restart leaves them shut",
    summary: "Opens or shuts the MCP write tools",
    input_data: "McpWritesInputContract",
    result:[
        {status_code: 200, description: "Whether the writes are open and for how much longer"},
    ]
)]
pub struct McpWritesAction {
    app: Arc<AppContext>,
}

impl McpWritesAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &McpWritesAction,
    input_data: McpWritesInputContract,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let now = DateTimeAsMicroseconds::now();

    if input_data.enabled {
        action.app.open_mcp_writes(now);
    } else {
        action.app.close_mcp_writes();
    }

    as_json(render(&action.app, now)).into_ok_result(true)
}

/// The same shape whichever way the switch was thrown, and the same shape
/// `/api/Status` shows: a caller which just opened the window and a caller
/// looking at the server read one answer, not two.
fn render(app: &AppContext, now: DateTimeAsMicroseconds) -> String {
    let remaining = app.mcp_writes_remaining_secs(now);

    JsonObjectWriter::new()
        .write("enabled", remaining.is_some())
        .write_if_some("remainingSecs", remaining)
        .build()
}
