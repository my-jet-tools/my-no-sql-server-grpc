use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonArrayWriter;

use crate::app::AppContext;
use crate::http_server::as_json;

/// Every namespace of this server, with how many tables each one holds.
///
/// **The one read on this surface which does not resolve the namespace of the
/// request**, and it can not: this is the call the UI makes to find out which
/// namespaces there are, so it answers about the server rather than about one of
/// them. Resolving `ns` here would mean a name the server does not know turns
/// the list itself into a 404 - and a picker handed a 404 has nothing left to
/// pick the right name out of, so the one mistyped header would be
/// unrecoverable from the UI.
///
/// An array, not an object: a namespace is named by its name and holds nothing
/// else worth counting at this level, so there is no second thing this route
/// would ever grow a field for.
#[http_route(
    method: "GET",
    route: "/api/Namespaces/List",
    controller: "Namespaces",
    description: "Every namespace of this server with the amount of tables it holds, ordered by name. 'default' is where callers which name no namespace land. The only read which ignores the `ns` of the request - it is what tells a caller which names exist",
    summary: "Returns every namespace with the amount of tables it holds",
    result:[
        {status_code: 200, description: "Namespaces as a JSON array of `name` and `tablesAmount`"},
    ]
)]
pub struct GetNamespacesAction {
    app: Arc<AppContext>,
}

impl GetNamespacesAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetNamespacesAction,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let mut namespaces: Vec<Namespace> = action
        .app
        .namespaces
        .get_all()
        .into_iter()
        .map(|db_namespace| Namespace {
            name: db_namespace.name.clone(),
            tables_amount: db_namespace.tables.get_tables().len(),
        })
        .collect();

    // The registry behind them is a hash map and has no order of its own. A
    // picker whose entries change places between two calls reads as a server
    // changing under the hand - and an order is what makes the answer
    // assertable at all.
    namespaces.sort_by(|left, right| left.name.cmp(&right.name));

    as_json(render(&namespaces)).into_ok_result(true)
}

/// One entry, collected before anything is formatted: the tables of a namespace
/// are asked for once, and the render walks what was collected.
struct Namespace {
    name: String,
    tables_amount: usize,
}

/// The answer itself, kept apart from the request so the shape can be exercised
/// without one. Both field names are what `NamespaceApiModel` in the UI
/// deserialises, which makes them as much of a contract as the route.
fn render(namespaces: &[Namespace]) -> String {
    let mut writer = JsonArrayWriter::new();

    for namespace in namespaces {
        writer = writer.write_json_object(|entry| {
            entry
                .write("name", namespace.name.as_str())
                .write("tablesAmount", namespace.tables_amount)
        });
    }

    writer.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namespaces(entries: &[(&str, usize)]) -> Vec<Namespace> {
        entries
            .iter()
            .map(|(name, tables_amount)| Namespace {
                name: name.to_string(),
                tables_amount: *tables_amount,
            })
            .collect()
    }

    /// `name` and `tablesAmount`, spelled exactly so: the UI deserialises those
    /// two names off this route into `NamespaceApiModel`, and renaming either of
    /// them leaves the namespace picker empty with nothing in the log to say
    /// why.
    #[test]
    fn the_answer_names_every_namespace_and_its_table_count() {
        assert_eq!(
            render(&namespaces(&[("default", 3), ("prod", 1)])),
            r#"[{"name":"default","tablesAmount":3},{"name":"prod","tablesAmount":1}]"#
        );
    }

    /// A namespace holding nothing is still a namespace: it has a folder on
    /// disk and it is where a caller naming it lands, so leaving it out of the
    /// picker would hide the name somebody is about to write a table into.
    #[test]
    fn a_namespace_with_no_tables_is_still_listed() {
        assert_eq!(
            render(&namespaces(&[("archive", 0)])),
            r#"[{"name":"archive","tablesAmount":0}]"#
        );
    }

    /// An array either way rather than nothing at all - the UI parses one model
    /// off this route, and it has to hold for a server which has not finished
    /// loading its namespaces yet.
    #[test]
    fn a_server_with_no_namespaces_answers_the_same_shape() {
        assert_eq!(render(&[]), "[]");
    }
}
