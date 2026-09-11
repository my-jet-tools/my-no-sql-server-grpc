use std::sync::Arc;

use rust_extensions::date_time::DateTimeAsMicroseconds;
use rust_extensions::{MyTimerTick, RepeatTimerIteration};

use crate::app::AppContext;

/// Takes a backup on a schedule and keeps the last few.
///
/// It runs only when the operator asked for it - `BackupsDest` says where they
/// go and `BackupIntervalSecs` says how often. How often to back up and how many
/// to keep is policy, and a server which invented one would be writing gigabytes
/// nobody asked for.
pub struct BackupTimer {
    app: Arc<AppContext>,
    interval_secs: u64,
    last_taken: parking_lot::Mutex<Option<DateTimeAsMicroseconds>>,
}

impl BackupTimer {
    pub fn new(app: Arc<AppContext>, interval_secs: u64) -> Self {
        Self {
            app,
            interval_secs,
            last_taken: parking_lot::Mutex::new(None),
        }
    }

    /// The timer it is registered on ticks far more often than a backup is
    /// wanted, so the interval is kept here rather than by having a timer per
    /// interval somebody might configure.
    fn is_due(&self, now: DateTimeAsMicroseconds) -> bool {
        let mut last_taken = self.last_taken.lock();

        let due = match *last_taken {
            Some(last) => {
                now.unix_microseconds - last.unix_microseconds
                    >= self.interval_secs as i64 * 1_000_000
            }
            // The first one is taken at the first tick after the server is up -
            // a server which restarts every hour would otherwise never back up.
            None => true,
        };

        if due {
            *last_taken = Some(now);
        }

        due
    }
}

#[async_trait::async_trait]
impl MyTimerTick for BackupTimer {
    async fn tick(&self) -> RepeatTimerIteration {
        let now = DateTimeAsMicroseconds::now();

        if !self.is_due(now) {
            return RepeatTimerIteration::WithInterval;
        }

        let taken = match crate::db_operations::backup::make(&self.app, now).await {
            Ok(taken) => taken,
            Err(err) => {
                println!("Can not take a backup: {err}");
                return RepeatTimerIteration::WithInterval;
            }
        };

        for backup in taken.iter() {
            println!(
                "Backup {} of namespace '{}' is taken",
                backup.name, backup.name_space
            );
        }

        // Counted per namespace, because a backup is of one namespace: a busy
        // namespace would otherwise push a quiet one's snapshots out.
        let Some(max_backups) = self.app.settings.max_backups else {
            return RepeatTimerIteration::WithInterval;
        };

        for backup in taken {
            match self
                .app
                .backups
                .keep_last(&backup.name_space, max_backups)
                .await
            {
                Ok(removed) => {
                    for name in removed {
                        println!("Backup {name} is past the last {max_backups} and was removed");
                    }
                }
                Err(err) => println!("Can not tidy the backups up: {err}"),
            }
        }

        RepeatTimerIteration::WithInterval
    }
}
