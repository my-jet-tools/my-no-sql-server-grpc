use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::as_json;

#[http_route(
    method: "GET",
    route: "/api/Status",
    controller: "Monitoring",
    description: "Everything this server can say about itself: settings, every namespace with its tables, the persist queue, the readers attached and the transactions open",
    summary: "Returns the state of the whole server",
    result:[
        {status_code: 200, description: "Status"},
    ]
)]
pub struct GetStatusAction {
    app: Arc<AppContext>,
}

impl GetStatusAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetStatusAction,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let json = crate::monitoring::status::render(&action.app, DateTimeAsMicroseconds::now());

    // `false` is "do not write telemetry": this is polled, and a monitoring call
    // has no business filling the log it is meant to help read.
    as_json(json).into_ok_result(false)
}
