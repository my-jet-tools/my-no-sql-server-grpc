use my_http_server::controllers::HttpRoute;
use my_http_server::{
    HttpContext, HttpFailResult, HttpOkResult, HttpPath, HttpRequestHeaders, HttpServerMiddleware,
};

/// The header the key arrives in. The same spelling the JSON version uses on the
/// one route it protects, so an operator with two of these servers has one thing
/// to remember.
pub const API_KEY_HEADER: &str = "apikey";

/// The liveness probe. It is answered without a key because the thing that calls
/// it - a load balancer, a container runtime - usually cannot be taught a
/// header, and it says nothing but the name of the application, its version and
/// the clock. Gate it and a healthy server is taken out of the pool.
const OPEN_ROUTE: &str = "/api/IsAlive";

/// Turns the whole HTTP surface into something that needs a key.
///
/// A middleware rather than a check inside each action, for two reasons: a route
/// added later is protected by default instead of by somebody remembering, and
/// `/metrics` has no `controller:` - so it can not carry `authorized:` and could
/// not be exempted, or included, through the macro at all.
///
/// It is registered **after** the swagger middleware, which answers its own
/// paths and returns before this one is reached. That is deliberate: a browser
/// fetching `/swagger/v1/swagger.yaml` has no way to attach a header, so gating
/// it would only break the UI - and the UI shows the shape of the API, never a
/// row. Calls made *from* the UI go through this gate like any other.
pub struct ApiKeyMiddleware {
    api_key: String,
    /// The open route held as the router holds its own, and matched with the
    /// router's own comparison. Built once, because a request is not the moment
    /// to take a route string apart.
    open_route: HttpRoute,
}

impl ApiKeyMiddleware {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            open_route: HttpRoute::new(OPEN_ROUTE),
        }
    }

    /// Whether this request may be answered.
    ///
    /// `/metrics` is **not** exempt. It names every namespace and every table
    /// and counts their rows, which is the shape of the data even if not the
    /// data; and a key only ever exists because an operator wrote one into the
    /// settings, so the scrape configuration is being written in the same
    /// breath. A scraper that is not told is a scrape that fails loudly, which
    /// is the failure worth having.
    fn is_allowed(&self, path: &HttpPath, presented: Option<&str>) -> bool {
        if self.is_open(path) {
            return true;
        }

        match presented {
            Some(presented) => is_the_same(presented.as_bytes(), self.api_key.as_bytes()),
            None => false,
        }
    }

    /// Matched **by segments**, which is how the router decides which action a
    /// path reaches. Comparing the raw string instead makes the exemption
    /// narrower than the route it exempts: `/api/IsAlive/` still reaches the
    /// liveness action, so a string comparison answers it with a 401 - and a
    /// health check answered 401 takes every instance out of the pool, which is
    /// the failure this exemption exists to prevent.
    fn is_open(&self, path: &HttpPath) -> bool {
        self.open_route.is_my_path(path)
    }
}

/// Compared in constant time. A plain `==` returns as soon as two bytes differ,
/// and the time it took says how much of the key was right - which is enough to
/// find the rest of it one byte at a time.
///
/// The length is compared first and does leak: how long the key is is not the
/// key, and hiding it would mean hashing both sides for no gain.
fn is_the_same(presented: &[u8], expected: &[u8]) -> bool {
    if presented.len() != expected.len() {
        return false;
    }

    let mut difference = 0u8;

    for (left, right) in presented.iter().zip(expected.iter()) {
        difference |= left ^ right;
    }

    difference == 0
}

#[my_http_server::async_trait::async_trait]
impl HttpServerMiddleware for ApiKeyMiddleware {
    async fn handle_request(
        &self,
        ctx: &mut HttpContext,
    ) -> Option<Result<HttpOkResult, HttpFailResult>> {
        let presented = ctx
            .request
            .get_headers()
            .try_get_case_insensitive_as_str(API_KEY_HEADER)
            .ok()
            .flatten();

        if self.is_allowed(&ctx.request.http_path, presented) {
            // Not ours to answer - the request carries on to the controllers.
            return None;
        }

        // One message for a missing key and for a wrong one. Telling them apart
        // would answer a question the caller should not get an answer to, and
        // there is nothing an honest caller does differently with the two.
        Some(Err(HttpFailResult::as_unauthorized(Some(
            "this server asks for an 'apikey' header",
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn middleware() -> ApiKeyMiddleware {
        ApiKeyMiddleware::new("s3cret".to_string())
    }

    fn allowed(middleware: &ApiKeyMiddleware, path: &str, presented: Option<&str>) -> bool {
        middleware.is_allowed(&HttpPath::from_str(path), presented)
    }

    #[test]
    fn the_right_key_gets_in_and_nothing_else_does() {
        let middleware = middleware();

        assert!(allowed(&middleware, "/api/Row", Some("s3cret")));

        assert!(!allowed(&middleware, "/api/Row", Some("s3cre")));
        assert!(!allowed(&middleware, "/api/Row", Some("s3crett")));
        assert!(!allowed(&middleware, "/api/Row", Some("S3CRET")));
        assert!(!allowed(&middleware, "/api/Row", Some("")));
        assert!(!allowed(&middleware, "/api/Row", None));
    }

    #[test]
    fn the_liveness_probe_answers_without_one() {
        let middleware = middleware();

        assert!(allowed(&middleware, "/api/IsAlive", None));
        // The path comes from the wire, and nothing normalises its case.
        assert!(allowed(&middleware, "/api/isalive", None));
        // The router ignores a trailing slash, so this reaches the liveness
        // action - and a probe the action answers must not be turned away here.
        assert!(allowed(&middleware, "/api/IsAlive/", None));
    }

    /// Every other route is guarded, `/metrics` included. This is the list an
    /// operator has to configure a client for.
    #[test]
    fn everything_else_is_guarded() {
        let middleware = middleware();

        for path in [
            "/metrics",
            "/api/Status",
            "/api/Connections",
            "/api/Tables/List",
            "/api/Partitions",
            "/api/Row",
            "/api/Row/Statistics",
            "/api/Tables/Clean",
            "/api/Tables",
            // A route which merely starts the same way is not the open one.
            "/api/IsAlive/Rows",
        ] {
            assert!(!allowed(&middleware, path, None), "{path} was not guarded");
            assert!(allowed(&middleware, path, Some("s3cret")), "{path}");
        }
    }

    /// A near miss must not be cheaper to reject than a far one.
    #[test]
    fn the_comparison_looks_at_every_byte() {
        assert!(is_the_same(b"abc", b"abc"));
        assert!(!is_the_same(b"abd", b"abc"));
        assert!(!is_the_same(b"dbc", b"abc"));
        assert!(!is_the_same(b"", b"abc"));
        assert!(is_the_same(b"", b""));
    }
}
