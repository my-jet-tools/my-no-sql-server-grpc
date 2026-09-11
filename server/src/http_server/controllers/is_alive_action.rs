use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult, HttpOutput};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::models::IsAliveResponse;

#[http_route(
    method: "GET",
    route: "/api/IsAlive",
    controller: "Monitoring",
    description: "Returns a model which shows that the service is alive",
    summary: "Returns a model which shows that the service is alive",
    result:[
        {status_code: 200, description: "Monitoring result", model: "IsAliveResponse"},
    ]
)]
pub struct IsAliveAction;

async fn handle_request(
    _: &IsAliveAction,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let response = IsAliveResponse {
        name: crate::app::APP_NAME.to_string(),
        version: crate::app::APP_VERSION.to_string(),
        time: DateTimeAsMicroseconds::now().to_rfc3339(),
    };

    HttpOutput::as_json(response).into_ok_result(false)
}
