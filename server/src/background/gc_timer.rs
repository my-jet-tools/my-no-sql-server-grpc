use std::sync::Arc;

use rust_extensions::date_time::DateTimeAsMicroseconds;
use rust_extensions::{MyTimerTick, RepeatTimerIteration};

use crate::app::AppContext;
use crate::data_sync_period::DataSyncPeriod;

/// Throws out what a table said it does not want any more: rows whose `Expires`
/// has come and gone, whatever its own limits no longer have room for, and the
/// schemas no row of it names any more.
///
/// Every row pass is per table and tells the subscribers about itself the same
/// way a delete does, so a reader's cache stops holding a row at about the
/// moment the server does. The schema pass tells nobody: it is about how the
/// rows are shown, which is this server's business alone.
pub struct GcTimer {
    app: Arc<AppContext>,
}

impl GcTimer {
    pub fn new(app: Arc<AppContext>) -> Self {
        Self { app }
    }
}

#[async_trait::async_trait]
impl MyTimerTick for GcTimer {
    async fn tick(&self) -> RepeatTimerIteration {
        let now = DateTimeAsMicroseconds::now();

        // Nothing here is urgent - the row is already gone from the table, and
        // the disk only has to catch up. Asking for it sooner would turn a table
        // which expires something every pass into a write on every pass.
        let persist_moment = DataSyncPeriod::default().get_sync_moment(now);

        for db_namespace in self.app.namespaces.get_all() {
            for db_table in db_namespace.tables.get_tables().iter() {
                crate::db_operations::gc::collect(
                    &self.app,
                    &db_namespace,
                    db_table,
                    now,
                    persist_moment,
                );

                // After the rows, and on the same tick: a pass which just took
                // the last row written under some entity version is exactly the
                // one whose schema has become dead.
                crate::db_operations::gc::collect_schemas(&db_namespace, db_table, persist_moment);
            }
        }

        RepeatTimerIteration::WithInterval
    }
}
