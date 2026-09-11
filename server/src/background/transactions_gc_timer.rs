use std::sync::Arc;

use rust_extensions::date_time::DateTimeAsMicroseconds;
use rust_extensions::{MyTimerTick, RepeatTimerIteration};

use crate::app::AppContext;
use crate::transactions::TRANSACTION_TTL_SECS;

/// Throws away transactions nobody finished. Everything they accumulated goes
/// with them, and it never reached a table, so there is nothing to undo - which
/// is the whole reason a transaction may be abandoned at all.
pub struct TransactionsGcTimer {
    app: Arc<AppContext>,
}

impl TransactionsGcTimer {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

#[async_trait::async_trait]
impl MyTimerTick for TransactionsGcTimer {
    async fn tick(&self) -> RepeatTimerIteration {
        let expired = self
            .app
            .transactions
            .gc(DateTimeAsMicroseconds::now(), TRANSACTION_TTL_SECS);

        for transaction_id in expired {
            println!("Transaction {transaction_id} was never finished and was forgotten");
        }

        RepeatTimerIteration::WithInterval
    }
}
