use std::sync::Arc;

use rust_extensions::date_time::DateTimeAsMicroseconds;
use rust_extensions::{MyTimerTick, RepeatTimerIteration};

use crate::app::AppContext;
use crate::reader::SESSION_TTL_SECS;

/// Forgets readers which stopped asking. The one that comes back is told its
/// session is unknown and starts over - the same thing a dropped TCP connection
/// used to mean.
pub struct ReaderSessionsGcTimer {
    app: Arc<AppContext>,
}

impl ReaderSessionsGcTimer {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

#[async_trait::async_trait]
impl MyTimerTick for ReaderSessionsGcTimer {
    async fn tick(&self) -> RepeatTimerIteration {
        let expired = self
            .app
            .reader_sessions
            .gc(DateTimeAsMicroseconds::now(), SESSION_TTL_SECS);

        for session_id in expired {
            println!("Reader session {session_id} went quiet and was forgotten");
        }

        RepeatTimerIteration::WithInterval
    }
}
