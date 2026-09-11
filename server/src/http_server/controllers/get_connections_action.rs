use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::http_server::as_json;

/// An object with one key rather than a bare array: the only connections this
/// server knows are readers, because a writer holds a channel and never
/// introduces itself, but a bare array could never grow a second kind.
#[http_route(
    method: "GET",
    route: "/api/Connections",
    controller: "Monitoring",
    description: "Reader sessions attached right now, what each is subscribed to and how much is queued for it",
    summary: "Returns the readers attached to this server",
    result:[
        {status_code: 200, description: "Connections"},
    ]
)]
pub struct GetConnectionsAction {
    app: Arc<AppContext>,
}

impl GetConnectionsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetConnectionsAction,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let now = DateTimeAsMicroseconds::now();
    let readers = crate::monitoring::readers::collect(&action.app);

    let json = JsonObjectWriter::new()
        .write_json_array("readers", |writer| {
            let mut writer = writer;

            for reader in readers.iter() {
                writer = writer
                    .write_json_object(|itm| crate::monitoring::readers::write(itm, reader, now));
            }

            writer
        })
        .build();

    as_json(json).into_ok_result(false)
}
