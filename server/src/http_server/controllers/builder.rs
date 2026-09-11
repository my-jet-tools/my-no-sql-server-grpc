use std::sync::Arc;

use my_http_server::HttpConnectionsCounter;
use my_http_server::controllers::ControllersMiddleware;

use crate::app::AppContext;

pub fn build(
    app: &Arc<AppContext>,
    http_connections: HttpConnectionsCounter,
) -> ControllersMiddleware {
    let mut result = ControllersMiddleware::new(None, None);

    result.register_get_action(Arc::new(super::IsAliveAction));
    result.register_get_action(Arc::new(super::GetStatusAction::new(app.clone())));
    result.register_get_action(Arc::new(super::GetConnectionsAction::new(app.clone())));
    result.register_get_action(Arc::new(super::MetricsAction::new(
        app.clone(),
        http_connections,
    )));
    result.register_get_action(Arc::new(super::GetTablesAction::new(app.clone())));
    result.register_get_action(Arc::new(super::GetPartitionsAction::new(app.clone())));
    result.register_get_action(Arc::new(super::GetPartitionDetailsAction::new(app.clone())));
    // Deliberately namespace-less: it is the call that says which namespaces
    // exist, and resolving the header would create the one being asked about.
    result.register_get_action(Arc::new(super::GetNamespacesAction::new(app.clone())));
    result.register_get_action(Arc::new(super::GetRowsAction::new(app.clone())));
    result.register_get_action(Arc::new(super::GetRowStatisticsAction::new(app.clone())));
    // A top level navigation, so this one takes the namespace as `?ns=` - an
    // `<a href>` has nowhere to put a header.
    result.register_get_action(Arc::new(super::DownloadRowsAction::new(app.clone())));

    // What the UI's settings page shows, and the switch under its own write
    // window. The MCP switch stays where it is - two surfaces, two windows.
    result.register_get_action(Arc::new(super::GetSettingsAction::new(app.clone())));
    result.register_post_action(Arc::new(super::SetSettingsAction::new(app.clone())));
    result.register_post_action(Arc::new(super::UiWritesAction::new(app.clone())));

    // Browsing a backup without restoring it: the archive is a zip per
    // namespace, and these four walk it from the outside in.
    result.register_get_action(Arc::new(super::GetBackupsAction::new(app.clone())));
    result.register_get_action(Arc::new(super::GetBackupTablesAction::new(app.clone())));
    result.register_get_action(Arc::new(super::GetBackupPartitionsAction::new(app.clone())));
    result.register_get_action(Arc::new(super::GetBackupRowsAction::new(app.clone())));

    // The three that change something, each behind the UI write window.
    result.register_post_action(Arc::new(super::MakeBackupAction::new(app.clone())));
    result.register_post_action(Arc::new(super::RestoreBackupAction::new(app.clone())));
    result.register_post_action(Arc::new(super::RestoreBackupPartitionAction::new(
        app.clone(),
    )));

    // The writes which carry no entity: keys and attributes, nothing that would
    // have to be turned from JSON into protobuf. Anything carrying an entity
    // needs its schema, and the schema travels with the write on gRPC.
    result.register_delete_action(Arc::new(super::DeleteRowAction::new(app.clone())));
    result.register_delete_action(Arc::new(super::DeletePartitionsAction::new(app.clone())));
    result.register_post_action(Arc::new(super::BulkDeleteAction::new(app.clone())));
    result.register_post_action(Arc::new(super::CleanTableAction::new(app.clone())));
    result.register_delete_action(Arc::new(super::DeleteTableAction::new(app.clone())));
    result.register_post_action(Arc::new(super::CreateTableAction::new(app.clone())));
    result.register_post_action(Arc::new(super::CreateTableIfNotExistsAction::new(
        app.clone(),
    )));
    result.register_put_action(Arc::new(super::SetTableAttributesAction::new(app.clone())));

    // The switch under the MCP write tools. Not a write itself - it decides
    // whether the agent may make one.
    result.register_post_action(Arc::new(super::McpWritesAction::new(app.clone())));

    result
}

#[cfg(test)]
mod tests {
    use my_http_server::MyHttpServer;
    use my_http_server::controllers::actions::HttpAction;

    use super::*;
    use crate::settings_reader::SettingsModel;

    fn routes(actions: &[HttpAction]) -> Vec<&str> {
        actions
            .iter()
            .map(|action| action.http_route.route.as_str())
            .collect()
    }

    /// These five are named after the controller they belong to, which is not
    /// how the JSON version spells them. Nothing but this test holds the
    /// spelling and the verb together: the routes live in the attribute of each
    /// action, and a rename there is invisible until something calls the route.
    #[test]
    fn every_route_is_registered_under_the_spelling_and_the_verb_it_declares() {
        let app = Arc::new(AppContext::new(Arc::new(SettingsModel {
            persistence_dest: String::new(),
            location: "test".to_string(),
            compress_data: true,
            skip_broken_partitions: false,
            backups_dest: None,
            backup_interval_secs: None,
            max_backups: None,
            api_key: None,
        })));

        // Not started, so nothing is bound - the counter is all that is wanted.
        let http_server = MyHttpServer::new("127.0.0.1:0".parse().unwrap());
        let middleware = build(&app, http_server.get_http_connections_counter());

        let get = routes(middleware.list_of_get_route_actions());
        let post = routes(middleware.list_of_post_route_actions());
        let delete = routes(middleware.list_of_delete_route_actions());

        assert!(delete.contains(&"/api/Tables"));
        assert!(delete.contains(&"/api/Partitions"));
        assert!(post.contains(&"/api/Rows/BulkDelete"));
        assert!(get.contains(&"/api/Row/Statistics"));
        assert!(post.contains(&"/api/Tables/Clean"));
    }

    /// The switch under the MCP write tools. It is held here rather than beside
    /// them because that is the point of it: the spelling in this list is the
    /// only way a person opens the window, and the tools themselves quote it
    /// back to the model as a string.
    #[test]
    fn the_mcp_write_switch_is_registered_where_the_tools_say_it_is() {
        let app = Arc::new(AppContext::new(Arc::new(SettingsModel {
            persistence_dest: String::new(),
            location: "test".to_string(),
            compress_data: true,
            skip_broken_partitions: false,
            backups_dest: None,
            backup_interval_secs: None,
            max_backups: None,
            api_key: None,
        })));

        let http_server = MyHttpServer::new("127.0.0.1:0".parse().unwrap());
        let middleware = build(&app, http_server.get_http_connections_counter());

        assert!(routes(middleware.list_of_post_route_actions()).contains(&"/api/Mcp/Writes"));
    }
}
