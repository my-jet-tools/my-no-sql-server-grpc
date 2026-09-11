use my_no_sql_grpc_core::db::TransactionAction;
use parking_lot::Mutex;
use rust_extensions::date_time::{AtomicDateTimeAsMicroseconds, DateTimeAsMicroseconds};

use crate::data_sync_period::DataSyncPeriod;

/// A transaction being built. It is bound to one table, named once when it was
/// started: the actions never name a table again, they are resolved through the
/// transaction they belong to.
///
/// The actions are the only mutable part, and every critical section is a push
/// or a take, so a `parking_lot` mutex is right - nothing under it awaits.
pub struct Transaction {
    pub id: String,
    pub namespace: String,
    pub table_name: String,
    pub sync_period: DataSyncPeriod,
    pub started: DateTimeAsMicroseconds,
    last_incoming: AtomicDateTimeAsMicroseconds,
    actions: Mutex<Vec<TransactionAction>>,
}

impl Transaction {
    pub fn new(
        id: String,
        namespace: String,
        table_name: String,
        sync_period: DataSyncPeriod,
    ) -> Self {
        Self {
            id,
            namespace,
            table_name,
            sync_period,
            started: DateTimeAsMicroseconds::now(),
            last_incoming: AtomicDateTimeAsMicroseconds::now(),
            actions: Mutex::new(Vec::new()),
        }
    }

    pub fn touch(&self) {
        self.last_incoming.update(DateTimeAsMicroseconds::now());
    }

    pub fn get_last_incoming(&self) -> DateTimeAsMicroseconds {
        self.last_incoming.as_date_time()
    }

    /// Appends a whole post at once. The stream that carried it has already
    /// ended cleanly by this point, which is what makes a broken post cost
    /// nothing: the transaction never saw the half of it.
    pub fn append(&self, actions: Vec<TransactionAction>) {
        self.actions.lock().extend(actions);
    }

    /// How much has been posted into it so far.
    pub fn get_actions_amount(&self) -> usize {
        self.actions.lock().len()
    }

    pub fn take_actions(&self) -> Vec<TransactionAction> {
        std::mem::take(&mut *self.actions.lock())
    }
}
