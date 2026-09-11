//! What the server says about itself: `/api/Status`, `/api/Connections` and
//! `/metrics`.
//!
//! All three are reads and all three live on the HTTP port. They share their
//! collectors so that a reader row is the same row wherever it is shown, and
//! none of them touches the persist repository - its mutex is held across file
//! I/O, and a scrape arriving during a vacuum would wait for the vacuum.

pub mod metrics;
mod metrics_writer;
pub mod readers;
pub mod status;
#[cfg(test)]
mod tests;

use rust_extensions::date_time::DateTimeAsMicroseconds;

/// How long ago, in seconds, rounded to the millisecond.
///
/// A number rather than a rendered duration: what a caller does with it is
/// compare it against a threshold, and `"1.523s"` has to be parsed first.
/// Negative is clamped - a moment stamped a microsecond after `now` was read is
/// not "-0.000001 seconds ago".
fn secs_ago(now: DateTimeAsMicroseconds, moment: DateTimeAsMicroseconds) -> f64 {
    let micros = now.unix_microseconds - moment.unix_microseconds;

    if micros <= 0 {
        return 0.0;
    }

    (micros / 1000) as f64 / 1000.0
}
