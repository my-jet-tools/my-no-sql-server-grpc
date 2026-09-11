mod health_banner;
pub use health_banner::*;

mod stats_row;
pub use stats_row::*;

mod reader_health_grid;
pub use reader_health_grid::*;

mod table_coverage;
pub use table_coverage::*;

mod writers_table;
pub use writers_table::*;

mod readers_table;
pub use readers_table::*;

use crate::components::atoms::StateTone;
use crate::settings::HealthThresholds;

/// How fresh a reader session — or a transaction — is, from the number of
/// seconds the server reported since its last activity.
///
/// This server answers with a number rather than a rendered duration, so the
/// thresholds (milliseconds) are what it is compared against directly: there
/// is no duration string left to parse a number back out of.
pub fn classify_secs_ago(secs_ago: f64, thresholds: HealthThresholds) -> StateTone {
    let ms = secs_ago * 1_000.0;

    if ms >= thresholds.bad_ms as f64 {
        StateTone::Bad
    } else if ms >= thresholds.warn_ms as f64 {
        StateTone::Warn
    } else {
        StateTone::Ok
    }
}
