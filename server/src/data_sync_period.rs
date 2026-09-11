use my_http_server::macros::MyHttpStringEnum;
use rust_extensions::date_time::DateTimeAsMicroseconds;

/// How long a write may wait in memory before the persist loop has to take it to
/// disk. It is a delay, not a mode: a hot table which is rewritten constantly is
/// flushed once per period instead of once per write.
///
/// The same enum on both transports: gRPC maps its own numbers onto it, and the
/// HTTP spellings are the ones the JSON version has always taken (`i`, `1`, `5`,
/// `15`, `30`, `60`, `a`), so a `curl` written against one server works against
/// the other.
// `Default` comes from `MyHttpStringEnum`, out of the case marked `default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, MyHttpStringEnum)]
pub enum DataSyncPeriod {
    #[http_enum_case(id = "0", value = "i", description = "Persist immediately")]
    Immediately,
    #[http_enum_case(id = "1", value = "1", description = "Persist within 1 second")]
    Sec1,
    #[http_enum_case(
        id = "5",
        value = "5",
        description = "Persist within 5 seconds",
        default
    )]
    Sec5,
    #[http_enum_case(id = "15", value = "15", description = "Persist within 15 seconds")]
    Sec15,
    #[http_enum_case(id = "30", value = "30", description = "Persist within 30 seconds")]
    Sec30,
    #[http_enum_case(id = "60", value = "60", description = "Persist within 1 minute")]
    Min1,
    #[http_enum_case(
        id = "6",
        value = "a",
        description = "Persist as soon as the loop gets to it"
    )]
    Asap,
}

impl DataSyncPeriod {
    /// `MyHttpInput` expands a bare `default` on an enum field into
    /// `Type::create_default()?`, while `MyHttpStringEnum` only derives `Default`
    /// from the case marked `default`. This bridges the two.
    pub fn create_default() -> Result<Self, my_http_utils::http_input::HttpParseError> {
        Ok(Self::default())
    }

    /// The moment the persist loop is allowed to write this change no earlier
    /// than. `Immediately` and `Asap` both mean "the next tick".
    pub fn get_sync_moment(&self, now: DateTimeAsMicroseconds) -> DateTimeAsMicroseconds {
        let mut result = now;

        match self {
            DataSyncPeriod::Immediately => {}
            DataSyncPeriod::Asap => {}
            DataSyncPeriod::Sec1 => result.add_seconds(1),
            DataSyncPeriod::Sec5 => result.add_seconds(5),
            DataSyncPeriod::Sec15 => result.add_seconds(15),
            DataSyncPeriod::Sec30 => result.add_seconds(30),
            DataSyncPeriod::Min1 => result.add_minutes(1),
        }

        result
    }
}
