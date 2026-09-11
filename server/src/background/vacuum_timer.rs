use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use rust_extensions::date_time::DateTimeAsMicroseconds;
use rust_extensions::{MyTimerTick, RepeatTimerIteration};

use crate::app::AppContext;

/// How long a vacuum waits for the next one.
const VACUUM_INTERVAL_SECS: u64 = 60 * 60;

/// Reclaims the disk that deleted and relocated partitions left behind. Wakes up
/// on the minute and does the work once an hour: it rewrites page-files while it
/// holds the `FilesRepo` of a namespace, and the persist loop waits for that same
/// lock **holding `persist_lock`**, so a pass stalls every other namespace's
/// queue behind it. It has no business running next to the write path more often
/// than the disk it frees is worth.
pub struct VacuumTimer {
    app: Arc<AppContext>,
    schedule: VacuumSchedule,
}

impl VacuumTimer {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self {
            app,
            schedule: VacuumSchedule::new(DateTimeAsMicroseconds::now()),
        }
    }
}

#[async_trait::async_trait]
impl MyTimerTick for VacuumTimer {
    async fn tick(&self) -> RepeatTimerIteration {
        if !self.schedule.take_turn(DateTimeAsMicroseconds::now()) {
            return RepeatTimerIteration::WithInterval;
        }

        for db_namespace in self.app.namespaces.get_all() {
            db_namespace.persist_repo.vacuum().await;
        }

        RepeatTimerIteration::WithInterval
    }
}

/// When the next pass is allowed.
///
/// The moment of the last pass lives **in memory only**: a restart is the one
/// thing which already rewrote nothing, and reading a stamp off the disk to
/// decide whether to rewrite the disk is a file whose only reader is this.
/// Seeded with the moment the server came up, so the first vacuum of a process
/// is an interval in - a server which is restarted every few minutes never
/// vacuums, which is what somebody restarting it every few minutes wants.
struct VacuumSchedule {
    last_run_unix_micros: AtomicI64,
}

impl VacuumSchedule {
    fn new(now: DateTimeAsMicroseconds) -> Self {
        Self {
            last_run_unix_micros: AtomicI64::new(now.unix_microseconds),
        }
    }

    /// Whether this tick is the one which vacuums; taking the turn moves the
    /// stamp, so the next one is an interval away.
    fn take_turn(&self, now: DateTimeAsMicroseconds) -> bool {
        let last_run =
            DateTimeAsMicroseconds::new(self.last_run_unix_micros.load(Ordering::Relaxed));

        if now.duration_since(last_run).as_positive_or_zero().as_secs() < VACUUM_INTERVAL_SECS {
            return false;
        }

        self.last_run_unix_micros
            .store(now.unix_microseconds, Ordering::Relaxed);

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plus_secs(from: DateTimeAsMicroseconds, secs: i64) -> DateTimeAsMicroseconds {
        DateTimeAsMicroseconds::new(from.unix_microseconds + secs * 1_000_000)
    }

    #[test]
    fn a_tick_before_the_interval_does_not_vacuum() {
        let started = DateTimeAsMicroseconds::new(1_700_000_000_000_000);
        let schedule = VacuumSchedule::new(started);

        for minute in 1..60 {
            assert!(
                !schedule.take_turn(plus_secs(started, minute * 60)),
                "the pass of minute {minute} rewrites page-files nobody asked to rewrite"
            );
        }
    }

    #[test]
    fn one_pass_per_interval_and_no_more() {
        let started = DateTimeAsMicroseconds::new(1_700_000_000_000_000);
        let schedule = VacuumSchedule::new(started);

        assert!(schedule.take_turn(plus_secs(started, VACUUM_INTERVAL_SECS as i64)));

        // The minute after the pass is a minute after the pass, not another one.
        assert!(!schedule.take_turn(plus_secs(started, VACUUM_INTERVAL_SECS as i64 + 60)));

        assert!(schedule.take_turn(plus_secs(started, VACUUM_INTERVAL_SECS as i64 * 2)));
    }
}
