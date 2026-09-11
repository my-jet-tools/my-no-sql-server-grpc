use std::sync::atomic::{AtomicI64, Ordering};

use rust_extensions::date_time::DateTimeAsMicroseconds;

/// How long a write window stays open once somebody opens it.
///
/// Ten minutes is long enough for a person to say what they want done and short
/// enough that a window left open is a window which closes itself. It is the
/// Both windows - the MCP one and the UI one - are this type and this figure:
/// one decision, one spelling.
pub const WRITE_WINDOW_SECS: i64 = 600;

/// The window in which one surface is allowed to write.
///
/// Held in memory and never persisted: a restart leaves the writes shut, which
/// is the state anybody would assume of a server they have just brought up.
///
/// Every surface holds **its own**. Opening the window for an agent must not
/// unlock the delete buttons of the UI, and neither window is a fact about the
/// other - one switch for both is a switch somebody throws for one reason and
/// forgets is still on for the other.
pub struct WriteWindow {
    /// When the window closes, in unix microseconds. `0` - closed.
    open_until: AtomicI64,
}

impl WriteWindow {
    pub fn new() -> Self {
        Self {
            open_until: AtomicI64::new(0),
        }
    }

    /// Opens the window for [`WRITE_WINDOW_SECS`], from now.
    ///
    /// Calling it again while a window is open moves the end further out rather
    /// than adding to it: the question being answered is "how long from now",
    /// and two clicks a minute apart must not add up to twenty minutes.
    pub fn open(&self, now: DateTimeAsMicroseconds) -> i64 {
        let mut until = now;
        until.add_seconds(WRITE_WINDOW_SECS);

        self.open_until
            .store(until.unix_microseconds, Ordering::SeqCst);

        WRITE_WINDOW_SECS
    }

    pub fn close(&self) {
        self.open_until.store(0, Ordering::SeqCst);
    }

    /// How many seconds the window still has, or `None` when it is shut.
    ///
    /// Asking is the only way to know: the window closes by itself, with nobody
    /// to write down that it did.
    pub fn remaining_secs(&self, now: DateTimeAsMicroseconds) -> Option<i64> {
        let until = self.open_until.load(Ordering::SeqCst);

        if until <= now.unix_microseconds {
            return None;
        }

        // Rounded up: with 1.4 seconds left the honest answer is "under two",
        // and `0` would read as shut while the window is still open.
        let left = until - now.unix_microseconds;
        let mut secs = left / 1_000_000;

        if left % 1_000_000 != 0 {
            secs += 1;
        }

        Some(secs)
    }

    /// Whether the window is open right now.
    ///
    /// Reads the clock itself, unlike [`Self::remaining_secs`]: a caller which
    /// only has to decide whether to let a write through has no other use for a
    /// moment, and one passed in would be a moment it could get wrong.
    pub fn is_open(&self) -> bool {
        self.remaining_secs(DateTimeAsMicroseconds::now()).is_some()
    }
}

impl Default for WriteWindow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000_000;

    fn moment(unix_microseconds: i64) -> DateTimeAsMicroseconds {
        DateTimeAsMicroseconds::new(unix_microseconds)
    }

    /// A window nobody opened is shut - which is what a freshly started process
    /// has to answer, because nothing about the window survives a restart.
    #[test]
    fn a_window_starts_shut() {
        let window = WriteWindow::new();

        assert_eq!(window.remaining_secs(moment(NOW)), None);
    }

    #[test]
    fn an_open_window_reports_what_is_left_and_lapses_by_itself() {
        let window = WriteWindow::new();

        assert_eq!(window.open(moment(NOW)), WRITE_WINDOW_SECS);
        assert_eq!(window.remaining_secs(moment(NOW)), Some(600));
        assert_eq!(
            window.remaining_secs(moment(NOW + 599_000_000)),
            Some(1),
            "a second before the end the window is still open"
        );
        assert_eq!(window.remaining_secs(moment(NOW + 600_000_000)), None);
    }

    /// Part of a second left is a second left, not `0`: `0` reads as shut while
    /// the window is still open.
    #[test]
    fn a_part_of_a_second_is_rounded_up() {
        let window = WriteWindow::new();
        window.open(moment(NOW));

        assert_eq!(window.remaining_secs(moment(NOW + 500_000)), Some(600));
        assert_eq!(window.remaining_secs(moment(NOW + 599_500_000)), Some(1));
    }

    /// Re-opening moves the end out rather than adding to it.
    #[test]
    fn opening_an_open_window_moves_its_end_instead_of_adding_to_it() {
        let window = WriteWindow::new();
        window.open(moment(NOW));
        window.open(moment(NOW + 60_000_000));

        assert_eq!(window.remaining_secs(moment(NOW + 60_000_000)), Some(600));
    }

    /// The clock-free form the callers who only gate a write use.
    #[test]
    fn is_open_answers_the_same_question_off_its_own_clock() {
        let window = WriteWindow::new();

        assert!(!window.is_open());

        window.open(DateTimeAsMicroseconds::now());
        assert!(window.is_open());

        window.close();
        assert!(!window.is_open());
    }

    #[test]
    fn closing_shuts_the_window_at_once() {
        let window = WriteWindow::new();
        window.open(moment(NOW));
        window.close();

        assert_eq!(window.remaining_secs(moment(NOW)), None);
    }
}
