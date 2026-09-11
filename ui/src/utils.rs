pub fn format_bytes(n: f64) -> String {
    let mut n = n;
    if n < 1024.0 {
        return format!("{:.0}b", n);
    }
    n /= 1024.0;
    if n < 1024.0 {
        return format!("{:.2}Kb", n);
    }
    n /= 1024.0;
    if n < 1024.0 {
        return format!("{:.2}Mb", n);
    }
    n /= 1024.0;
    format!("{:.2}Gb", n)
}

/// An age, short enough to live in a table cell.
///
/// Sub-ten-second values keep a decimal: between "0.2s" and "4.0s" is the whole
/// difference between a healthy reader and a slow one, and that is what every
/// caller of this is looking at — the server sends these as a count of seconds
/// precisely so they can be compared against a threshold, and rendering one is
/// the other half of that deal.
///
/// There is no days branch on purpose. Everything measured this way is an age
/// that a live session or a live transaction carries, so "72h 00m" is already
/// past the point where the number stopped being the news. A span that really
/// can run into days — an up-time, a backup interval — goes through
/// `format_duration_secs` instead.
pub fn format_secs_ago(secs_ago: f64) -> String {
    // A server whose clock jumped backwards — or a NaN out of a bad parse —
    // would otherwise render "-3.0s" or "NaNs".
    let secs = if secs_ago.is_finite() {
        secs_ago.max(0.0)
    } else {
        0.0
    };

    if secs < 10.0 {
        return format!("{:.1}s", secs);
    }

    let total = secs as u64;

    if total < 60 {
        return format!("{}s", total);
    }

    if total < 3_600 {
        return format!("{}m {:02}s", total / 60, total % 60);
    }

    format!("{}h {:02}m", total / 3_600, (total % 3_600) / 60)
}

/// A span of seconds at the coarsest unit that still says something: a write
/// window that is closing, an up-time, a backup interval.
///
/// Coarser than `format_secs_ago` and it goes further — a server up for a week
/// should not be asked to be read as "168h 00m" — because nothing measured this
/// way turns on a fraction of a second.
pub fn format_duration_secs(secs: f64) -> String {
    let total = if secs.is_finite() {
        secs.max(0.0) as u64
    } else {
        0
    };

    if total < 60 {
        return format!("{}s", total);
    }
    if total < 3_600 {
        return format!("{}m {:02}s", total / 60, total % 60);
    }
    if total < 86_400 {
        return format!("{}h {:02}m", total / 3_600, (total % 3_600) / 60);
    }
    format!("{}d {:02}h", total / 86_400, (total % 86_400) / 3_600)
}

/// An RFC3339 moment cut down to what is readable in a cell:
/// `2026-09-11 10:20:30`.
///
/// This server sends every moment as `to_rfc3339()` of a UTC clock
/// (`2026-09-11T10:20:30.123456+00:00`); the microseconds and the zero offset
/// are noise in a column that is only glanced at. The zone is not shown, so a
/// place where it matters says so in its label rather than in every row. A
/// value that is not that shape is passed through untouched rather than cut
/// into a wrong date.
pub fn format_moment(value: &str) -> String {
    if value.is_empty() {
        return "—".to_string();
    }

    let Some((date, rest)) = value.split_once('T') else {
        return value.to_string();
    };

    // Split the clock only, never the whole value: the date carries a '-' of
    // its own, and the first of these inside the clock begins either the
    // fraction or the offset.
    let clock = rest.split(['.', '+', '-', 'Z']).next().unwrap_or(rest);
    if clock.len() < 8 {
        return value.to_string();
    }

    format!("{} {}", date, clock)
}
