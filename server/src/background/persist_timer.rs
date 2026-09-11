use std::sync::Arc;

use rust_extensions::date_time::DateTimeAsMicroseconds;
use rust_extensions::{MyTimerTick, RepeatTimerIteration};

use crate::app::AppContext;

pub struct PersistTimer {
    app: Arc<AppContext>,
}

impl PersistTimer {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

#[async_trait::async_trait]
impl MyTimerTick for PersistTimer {
    async fn tick(&self) -> RepeatTimerIteration {
        // `persist` writes one queued task per namespace and reports whether it
        // did any work. If it did, there may be more waiting - ask the timer to
        // run us again straight away, each iteration with its own timeout window,
        // instead of draining the whole queue inside one tick.
        if crate::operations::persist(&self.app, Some(DateTimeAsMicroseconds::now())).await {
            RepeatTimerIteration::Immediately
        } else {
            RepeatTimerIteration::WithInterval
        }
    }
}
