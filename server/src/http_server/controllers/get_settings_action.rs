use std::sync::Arc;

use my_http_server::macros::http_route;
use my_http_server::{HttpContext, HttpFailResult, HttpOkResult};
use my_json::json_writer::JsonObjectWriter;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::AppContext;
use crate::app::ui_settings::UiSettings;
use crate::http_server::as_json;
use crate::settings_reader::SettingsModel;

/// What the settings page shows: where this server runs, where it puts its
/// data, whether backups are configured, whether a key guards the surface, and
/// the state of the two write windows.
///
/// It is a **page**, not a monitoring dump - `/api/Status` is that, and it
/// answers a different question, so the numbers of the running server are not
/// repeated here.
///
/// Nothing in the answer is a secret, and one field exists to keep it that way:
/// the `ApiKey` is reported as whether it is set. `SettingsModel` writes its own
/// `Debug` for the same reason, and a route which showed the key would undo it -
/// the settings of a server are the most likely place for a secret to escape.
#[http_route(
    method: "GET",
    route: "/api/Settings",
    controller: "Settings",
    description: "The server's settings as a page shows them - location, where data and backups go, whether an ApiKey is set (never its value) - and whether each of the two write windows is open",
    summary: "Returns the settings and the state of the two write windows",
    result:[
        {status_code: 200, description: "Settings"},
    ]
)]
pub struct GetSettingsAction {
    app: Arc<AppContext>,
}

impl GetSettingsAction {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

async fn handle_request(
    action: &GetSettingsAction,
    _ctx: &mut HttpContext,
) -> Result<HttpOkResult, HttpFailResult> {
    let now = DateTimeAsMicroseconds::now();

    // Read on every call rather than cached: the file is two numbers, and a
    // person who has just edited it should not have to restart the server to
    // see the page agree with them.
    let ui_settings =
        crate::app::ui_settings::load(action.app.settings.get_persistence_dest().as_str()).await;

    let json = render(
        &action.app.settings,
        action.app.backups.is_configured(),
        action.app.mcp_writes_remaining_secs(now),
        action.app.ui_writes.remaining_secs(now),
        ui_settings,
    );

    // `false` is "do not write telemetry": the settings page polls this once a
    // second to keep both countdowns honest, and a poll has no business filling
    // the log.
    as_json(json).into_ok_result(false)
}

/// The answer itself, kept apart from the request so the shape - and above all
/// what is *not* in it - can be exercised without one.
///
/// Whether backups are configured comes from the repository rather than from the
/// setting: the repository is what a backup call actually asks, and a path that
/// did not resolve is not a configured destination.
pub fn render(
    settings: &SettingsModel,
    backups_configured: bool,
    mcp_writes_remaining_secs: Option<i64>,
    ui_writes_remaining_secs: Option<i64>,
    ui_settings: UiSettings,
) -> String {
    JsonObjectWriter::new()
        .write("location", settings.location.as_str())
        // Resolved, not as written: `~/data` is not an answer to "where is my
        // data", and this is the one page a person opens to find out.
        .write("persistenceDest", settings.get_persistence_dest())
        .write_json_object("backups", |backups| {
            backups
                .write("configured", backups_configured)
                // Absent means "only by hand" and "keep all of them" - the two
                // states a number can not say, which is why they are omitted
                // rather than sent as 0.
                .write_if_some("intervalSecs", settings.backup_interval_secs)
                .write_if_some("maxBackups", settings.max_backups)
        })
        // Whether, never which.
        .write("apiKeySet", settings.api_key.is_some())
        // A window nobody can see is a window nobody remembers to shut, and
        // these two live only in memory - asking is the only way to know. The
        // four names are what the UI deserialises off this route.
        .write("mcpWritesEnabled", mcp_writes_remaining_secs.is_some())
        .write_if_some("mcpWritesRemainingSecs", mcp_writes_remaining_secs)
        .write("uiWritesEnabled", ui_writes_remaining_secs.is_some())
        .write_if_some("uiWritesRemainingSecs", ui_writes_remaining_secs)
        // The UI's own two thresholds, kept on the server because they are a
        // statement about this deployment: the next person to open the page
        // should see what the last one decided is slow here.
        .write("warnMs", ui_settings.warn_ms)
        .write("badMs", ui_settings.bad_ms)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    const API_KEY: &str = "the-key-nobody-may-see";

    fn settings() -> SettingsModel {
        SettingsModel {
            persistence_dest: "/var/lib/mynosql".to_string(),
            location: "de-01".to_string(),
            compress_data: true,
            skip_broken_partitions: false,
            backups_dest: None,
            backup_interval_secs: None,
            max_backups: None,
            api_key: None,
        }
    }

    /// Written out in full because every name in it is somebody's decision, and
    /// four of them are read by the UI off this route.
    #[test]
    fn the_whole_answer_of_a_plain_server() {
        assert_eq!(
            render(&settings(), false, None, None, UiSettings::default()),
            concat!(
                r#"{"location":"de-01","persistenceDest":"/var/lib/mynosql","#,
                r#""backups":{"configured":false},"apiKeySet":false,"#,
                r#""mcpWritesEnabled":false,"uiWritesEnabled":false,"#,
                r#""warnMs":3000,"badMs":10000}"#
            )
        );
    }

    /// The point of the field: a page which showed the key would undo the
    /// hand-written `Debug` that keeps it out of the log.
    #[test]
    fn the_api_key_is_reported_as_set_and_never_shown() {
        let mut settings = settings();
        settings.api_key = Some(API_KEY.to_string());

        let json = render(&settings, false, None, None, UiSettings::default());

        assert!(json.contains(r#""apiKeySet":true"#));
        assert!(
            !json.contains(API_KEY),
            "the key itself must not be in the answer: {}",
            json
        );
    }

    #[test]
    fn a_configured_backup_names_its_interval_and_how_many_are_kept() {
        let mut settings = settings();
        settings.backups_dest = Some("/var/lib/backups".to_string());
        settings.backup_interval_secs = Some(3_600);
        settings.max_backups = Some(24);

        let json = render(&settings, true, None, None, UiSettings::default());

        assert!(
            json.contains(r#""backups":{"configured":true,"intervalSecs":3600,"maxBackups":24}"#),
            "{}",
            json
        );
        // The destination of the backups is deliberately not in the answer -
        // "configured" is the question a page asks, and the path is the
        // operator's business, in the same file as the key.
        assert!(!json.contains("/var/lib/backups"));
    }

    /// Both windows, spelled the way the UI reads them - and each one on its
    /// own, so a window open for an agent can not be read as one open for the
    /// dashboard.
    #[test]
    fn each_write_window_is_reported_on_its_own() {
        let json = render(&settings(), false, Some(600), None, UiSettings::default());

        assert!(json.contains(r#""mcpWritesEnabled":true,"mcpWritesRemainingSecs":600"#));
        assert!(json.contains(r#""uiWritesEnabled":false"#));
        assert!(!json.contains("uiWritesRemainingSecs"));

        let json = render(&settings(), false, None, Some(42), UiSettings::default());

        assert!(json.contains(r#""mcpWritesEnabled":false"#));
        assert!(!json.contains("mcpWritesRemainingSecs"));
        assert!(json.contains(r#""uiWritesEnabled":true,"uiWritesRemainingSecs":42"#));
    }
}
