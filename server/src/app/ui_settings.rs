use my_json::json_reader::JsonFirstLineIterator;
use my_json::json_writer::JsonObjectWriter;

/// Defaults the UI itself falls back to, so a server that has never been asked
/// answers the same numbers the page would have used anyway.
pub const DEFAULT_WARN_MS: u32 = 3_000;
pub const DEFAULT_BAD_MS: u32 = 10_000;

/// The file, inside the persistence root. A file and not a folder, so namespace
/// discovery walks past it - it only ever looks at directories.
const FILE_NAME: &str = "ui-settings.json";

/// What the UI is allowed to remember on the server.
///
/// Two thresholds and nothing else: how long a reader may go without asking
/// before the page paints it yellow, and before red. They live here rather than
/// in the browser because they are a statement about *this server* - the person
/// who tunes them is saying what is slow for this deployment, and the next
/// person to open the page should see the same answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiSettings {
    pub warn_ms: u32,
    pub bad_ms: u32,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            warn_ms: DEFAULT_WARN_MS,
            bad_ms: DEFAULT_BAD_MS,
        }
    }
}

impl UiSettings {
    /// Warn before bad, whatever was asked for. A pair the wrong way round
    /// would paint everything red past the warning point, and refusing it with
    /// an error is worse than the obvious reading of what the person meant.
    pub fn sanitized(self) -> Self {
        if self.warn_ms > self.bad_ms {
            return Self {
                warn_ms: self.bad_ms,
                bad_ms: self.warn_ms,
            };
        }

        self
    }
}

fn file_path(persistence_dest: &str) -> String {
    format!(
        "{}{}{}",
        persistence_dest.trim_end_matches(std::path::MAIN_SEPARATOR),
        std::path::MAIN_SEPARATOR,
        FILE_NAME
    )
}

/// Never fails: a missing file is a server nobody has tuned, and a file somebody
/// hand-edited into nonsense is not a reason to refuse to serve the page which
/// is the only way to fix it.
pub async fn load(persistence_dest: &str) -> UiSettings {
    let Ok(content) = tokio::fs::read(file_path(persistence_dest).as_str()).await else {
        return UiSettings::default();
    };

    match parse(&content) {
        Ok(settings) => settings.sanitized(),
        Err(err) => {
            println!("WARNING: {FILE_NAME} is not readable and is ignored: {err}");
            UiSettings::default()
        }
    }
}

/// A missing field keeps its default rather than failing the whole file: the
/// two numbers are independent, and one of them hand-edited into nonsense is no
/// reason to forget the other.
pub fn parse(content: &[u8]) -> Result<UiSettings, String> {
    parse_patch(content, UiSettings::default())
}

/// The same reading, but over values that are already stored: a body naming one
/// of the two numbers leaves the other where it was.
pub fn parse_patch(content: &[u8], onto: UiSettings) -> Result<UiSettings, String> {
    let mut result = onto;

    let reader = JsonFirstLineIterator::new(content);

    while let Some(next) = reader.get_next() {
        let (name, value) = next.map_err(|err| format!("{err:?}"))?;
        let name = name.as_str().map_err(|err| format!("{err:?}"))?.to_string();

        // The name is matched before the value is read: a key this version
        // does not know is a file written by a newer one, and skipping it must
        // not depend on what type its value happens to be.
        let field = match name.as_str() {
            "warnMs" => Field::Warn,
            "badMs" => Field::Bad,
            _ => continue,
        };

        let number = value
            .unwrap_as_number()
            .map_err(|err| format!("'{name}' is not a number of milliseconds: {err:?}"))?
            .ok_or_else(|| format!("'{name}' has no value"))?;

        let number = u32::try_from(number)
            .map_err(|_| format!("'{name}' is not a number of milliseconds"))?;

        match field {
            Field::Warn => result.warn_ms = number,
            Field::Bad => result.bad_ms = number,
        }
    }

    Ok(result)
}

enum Field {
    Warn,
    Bad,
}

fn render(settings: UiSettings) -> String {
    JsonObjectWriter::new()
        .write("warnMs", settings.warn_ms)
        .write("badMs", settings.bad_ms)
        .build()
}

pub async fn save(persistence_dest: &str, settings: UiSettings) -> Result<UiSettings, String> {
    let settings = settings.sanitized();

    let content = render(settings);

    // The root may not exist yet on a server which has never persisted
    // anything, and a person tuning the page before the first write is not an
    // error case.
    if let Err(err) = tokio::fs::create_dir_all(persistence_dest).await {
        return Err(format!("Can not create {persistence_dest}: {err}"));
    }

    tokio::fs::write(file_path(persistence_dest).as_str(), content.as_bytes())
        .await
        .map_err(|err| format!("Can not write {FILE_NAME}: {err}"))?;

    Ok(settings)
}

#[cfg(test)]
mod tests {
    use rust_extensions::date_time::DateTimeAsMicroseconds;

    use super::*;

    #[test]
    fn a_pair_the_wrong_way_round_is_swapped_rather_than_refused() {
        let swapped = UiSettings {
            warn_ms: 9_000,
            bad_ms: 1_000,
        }
        .sanitized();

        assert_eq!(swapped.warn_ms, 1_000);
        assert_eq!(swapped.bad_ms, 9_000);
    }

    #[test]
    fn an_ordered_pair_is_left_alone() {
        let kept = UiSettings {
            warn_ms: 1_000,
            bad_ms: 9_000,
        }
        .sanitized();

        assert_eq!(kept.warn_ms, 1_000);
        assert_eq!(kept.bad_ms, 9_000);
    }

    #[tokio::test]
    async fn a_server_nobody_tuned_answers_the_defaults() {
        let loaded = load("/tmp/there-is-no-such-folder-here").await;

        assert_eq!(loaded, UiSettings::default());
    }

    #[test]
    fn a_file_missing_a_field_keeps_the_default_for_it() {
        let parsed = parse(br#"{"badMs":12000}"#).unwrap();

        assert_eq!(parsed.warn_ms, DEFAULT_WARN_MS);
        assert_eq!(parsed.bad_ms, 12_000);
    }

    #[test]
    fn a_key_this_version_does_not_know_is_skipped() {
        let parsed = parse(br#"{"warnMs":1000,"somethingNewer":"not a number"}"#).unwrap();

        assert_eq!(parsed.warn_ms, 1_000);
    }

    #[tokio::test]
    async fn what_was_saved_is_what_loads_back() {
        let dir = format!(
            "/tmp/my-no-sql-grpc-ui-settings-{}",
            DateTimeAsMicroseconds::now().unix_microseconds
        );

        let saved = save(
            dir.as_str(),
            UiSettings {
                warn_ms: 4_000,
                bad_ms: 12_000,
            },
        )
        .await
        .unwrap();

        assert_eq!(load(dir.as_str()).await, saved);

        tokio::fs::remove_dir_all(dir.as_str()).await.unwrap();
    }
}
