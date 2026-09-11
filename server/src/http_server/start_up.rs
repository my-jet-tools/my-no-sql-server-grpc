use std::net::SocketAddr;
use std::sync::Arc;

use my_http_server::MyHttpServer;
use my_http_server::controllers::swagger::SwaggerMiddleware;

use crate::app::AppContext;

use super::ApiKeyMiddleware;

/// The HTTP surface: reads rendered through their schema, which is what makes a
/// row readable by a human, and the writes which carry no entity - keys and
/// attributes. Anything carrying an entity stays on gRPC.
pub fn start(app: &Arc<AppContext>, port: u16) {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    println!("Listening HTTP at: {addr}");

    let mut http_server = MyHttpServer::new(addr);

    // The counter has to be taken from the server before it is started - it is
    // the only handle on the connections it will accept.
    let controllers = Arc::new(crate::http_server::controllers::builder::build(
        app,
        http_server.get_http_connections_counter(),
    ));

    let swagger_middleware = Arc::new(SwaggerMiddleware::new(
        controllers.clone(),
        crate::app::APP_NAME.to_string(),
        crate::app::APP_VERSION.to_string(),
    ));

    http_server.add_middleware(swagger_middleware);

    // Between swagger and the controllers: the UI and the yaml it fetches stay
    // reachable by a browser, which can not attach a header, and every route
    // that answers about the data is behind the key.
    match app.settings.api_key.as_ref() {
        Some(api_key) => {
            println!(
                "HTTP asks for the '{}' header. Open without it: /api/IsAlive and the swagger UI",
                crate::http_server::API_KEY_HEADER
            );

            http_server.add_middleware(Arc::new(ApiKeyMiddleware::new(api_key.clone())));
        }
        None => println!("HTTP is open: no ApiKey is set in the settings"),
    }

    println!(
        "MCP is served at {}. Its write tools are shut until POST /api/Mcp/Writes?enabled=true",
        crate::mcp::MCP_PATH
    );

    // After the key, like the controllers: an MCP client attaches headers, and
    // the tools behind this one answer about the data.
    http_server.add_middleware(Arc::new(crate::mcp::build_middleware(app)));

    http_server.add_middleware(controllers);

    http_server.start(app.states.clone(), my_logger::LOGGER.clone());
}
