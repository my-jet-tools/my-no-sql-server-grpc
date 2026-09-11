use std::sync::Arc;

use my_http_server::{HttpConnectionsCounter, macros::http_route};
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput, WebContentType};

use crate::app::AppContext;

/// The scrape endpoint, deliberately without a `controller:`.
///
/// Without one the macro produces no api description, which is exactly what
/// keeps `/metrics` out of swagger - it is not part of the API a client
/// programmes against, and it does not answer JSON.
///
/// The same omission means the route can not declare its own authorization
/// through the macro at all - which is one of the reasons the `ApiKey` is a
/// middleware rather than an attribute. It guards this route like every other:
/// a scrape names every namespace and table and counts their rows.
#[http_route(
    method: "GET",
    route: "/metrics",
)]
pub struct MetricsAction {
    app: Arc<AppContext>,
    http_connections: HttpConnectionsCounter,
}

impl MetricsAction {
    pub fn new(app: Arc<AppContext>, http_connections: HttpConnectionsCounter) -> Self {
        Self {
            app,
            http_connections,
        }
    }
}

async fn handle_request(
    action: &MetricsAction,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let content = crate::monitoring::metrics::render(
        &action.app,
        action.http_connections.get_connections_amount(),
    );

    HttpOutput::Content {
        headers: WebContentType::Text.into(),
        content: content.into_bytes(),
        status_code: 200,
    }
    .into_ok_result(false)
}
