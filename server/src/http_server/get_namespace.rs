use std::sync::Arc;

use my_http_server::{HttpContext, HttpFailResult, HttpOutput, HttpRequestHeaders, WebContentType};

use crate::app::{AppContext, DbNamespace, DbNamespaces};
use crate::db_operations::DbOperationError;

/// The header naming the namespace a request works in. No header - or an empty
/// one - is the default namespace, which is what every client that does not care
/// about namespaces sends.
///
/// The value is resolved **here, off the request**, for every action, so the
/// query-parameter fallback below applies everywhere. The input contracts also
/// declare the header with `#[http_header(name = "ns")]`, but only so it shows
/// up in the generated OpenAPI description: swagger is built from the contracts,
/// and a header read straight off the request is invisible to it. Those declared
/// fields are documentation, not the source of truth - reading them instead
/// would quietly skip the fallback, and a `?ns=prod` nobody read means a delete
/// addressed at the default namespace. Two ways of saying it because a browser
/// fetching a page can put it in the query but not in a header, and a service
/// calling the route has the header - the UI is who this surface answers to.
pub const NAMESPACE_HEADER: &str = "ns";

/// The name the request works under, header first and `?ns=` after it. An empty
/// string is the default namespace.
///
/// The only place this is read from anything other than
/// [`get_request_namespace`] is creating a table - the one operation whose job
/// is to bring what it names into existence, and therefore the only one which
/// resolves with `get_or_create`.
pub fn get_request_namespace_name(ctx: &HttpContext) -> &str {
    match get_namespace_header(ctx) {
        Some(namespace) => namespace,
        None => get_namespace_query_param(ctx).unwrap_or(""),
    }
}

/// Resolves the namespace of a request. Reading never creates one - a name
/// nobody has written to holds nothing, and creating it as a side effect of a
/// GET would leave empty folders behind.
pub fn get_request_namespace(
    app: &Arc<AppContext>,
    ctx: &HttpContext,
) -> Result<Arc<DbNamespace>, DbOperationError> {
    let namespace = get_request_namespace_name(ctx);

    match app.namespaces.get(namespace) {
        Some(db_namespace) => Ok(db_namespace),
        None => Err(DbOperationError::NamespaceNotFound(
            DbNamespaces::resolve_name(namespace).to_string(),
        )),
    }
}

fn get_namespace_header(ctx: &HttpContext) -> Option<&str> {
    ctx.request
        .get_headers()
        .try_get_case_insensitive_as_str(NAMESPACE_HEADER)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

/// The fallback for callers which can not attach a header at all - a browser
/// download is an `<a href>`, and a hand-written `curl` reaches for `?ns=`
/// before it reaches for `-H`. The header wins when both are there.
fn get_namespace_query_param(ctx: &HttpContext) -> Option<&str> {
    namespace_from_query(ctx.request.get_uri().query()?)
}

/// Kept apart from the request so it can be exercised without one: this is the
/// half of the resolution which was missing.
fn namespace_from_query(query: &str) -> Option<&str> {
    for pair in query.split('&') {
        // A valueless element (`?flag`) is somebody else's parameter, not the
        // end of the query string - keep looking.
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };

        if key.eq_ignore_ascii_case(NAMESPACE_HEADER) {
            if value.is_empty() {
                return None;
            }

            return Some(value);
        }
    }

    None
}

impl From<DbOperationError> for HttpFailResult {
    fn from(src: DbOperationError) -> Self {
        let status_code = match src {
            DbOperationError::NotInitialized => 503,
            DbOperationError::NamespaceNotFound(_) => 404,
            DbOperationError::TableNotFound(_) => 404,
            DbOperationError::PartitionNotFound(_) => 404,
            DbOperationError::RowNotFound => 404,
            DbOperationError::TableAlreadyExists(_) => 409,
            DbOperationError::RowAlreadyExists => 409,
            DbOperationError::OptimisticConcurrencyUpdateFails => 409,
            DbOperationError::EntityParseFail(_) => 400,
            DbOperationError::DefaultNamespaceCanNotBeDeleted => 400,
            DbOperationError::InvalidNamespaceName(_) => 400,
            DbOperationError::NamespaceFolderNotDeleted(_) => 500,
            DbOperationError::BackupFailed(_) => 412,
            DbOperationError::MigrationFailed(_) => 412,
        };

        HttpOutput::Content {
            headers: WebContentType::Text.into(),
            content: src.to_string().into_bytes(),
            status_code,
        }
        .into_http_fail_result(true, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason this exists at all: `?ns=prod` names the namespace, and a
    /// delete which did not read it addressed the default one - and dropped the
    /// wrong table without a word.
    #[test]
    fn the_namespace_is_read_out_of_the_query_string() {
        assert_eq!(namespace_from_query("ns=prod"), Some("prod"));
        assert_eq!(
            namespace_from_query("tableName=traders&ns=prod"),
            Some("prod")
        );
        assert_eq!(namespace_from_query("ns=prod&syncPeriod=i"), Some("prod"));
        // The parameter is matched the way a header is.
        assert_eq!(namespace_from_query("NS=prod"), Some("prod"));
    }

    /// Absent and empty are the same answer - the default namespace - and
    /// neither of them may be confused with somebody else's parameter.
    #[test]
    fn nothing_named_ns_means_the_default_namespace() {
        assert_eq!(namespace_from_query(""), None);
        assert_eq!(namespace_from_query("ns="), None);
        assert_eq!(namespace_from_query("tableName=traders"), None);
        // A parameter whose name merely contains "ns" is not this one.
        assert_eq!(namespace_from_query("nsx=prod&answers=1"), None);
        // A valueless element is not the end of the query string.
        assert_eq!(namespace_from_query("flag&ns=prod"), Some("prod"));
    }
}
