use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ahash::AHashMap;
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::data_sync_period::DataSyncPeriod;

use super::Transaction;

/// A transaction which has not been posted to, committed or cancelled in this
/// long is forgotten and its actions are dropped.
///
/// It is longer than a reader session's because it is not a lease anybody
/// renews on a timer: a transaction is touched only by its own calls, so the
/// window has to cover a client which stops to think between two of them.
pub const TRANSACTION_TTL_SECS: i64 = 60;

/// Every transaction being built. Same shape as the reader sessions and for the
/// same reason: looked up on every call, changed only when one appears or goes
/// away.
pub struct Transactions {
    inner: ArcSwap<AHashMap<String, Arc<Transaction>>>,
    write_lock: Mutex<()>,
    next_id: AtomicU64,
}

impl Transactions {
    pub fn new() -> Self {
        Self {
            inner: ArcSwap::from_pointee(AHashMap::new()),
            write_lock: Mutex::new(()),
            next_id: AtomicU64::new(0),
        }
    }

    pub fn create(
        &self,
        namespace: String,
        table_name: String,
        sync_period: DataSyncPeriod,
    ) -> Arc<Transaction> {
        let _guard = self.write_lock.lock();

        // The id only has to be unique among the transactions this process is
        // holding: a transaction never outlives the process, and one which comes
        // back to a restarted server is told the id is unknown anyway.
        let id = format!(
            "{}-{}",
            DateTimeAsMicroseconds::now().unix_microseconds,
            self.next_id.fetch_add(1, Ordering::SeqCst)
        );

        let transaction = Arc::new(Transaction::new(
            id.clone(),
            namespace,
            table_name,
            sync_period,
        ));

        let mut map = self.inner.load().as_ref().clone();
        map.insert(id, transaction.clone());
        self.inner.store(Arc::new(map));

        transaction
    }

    pub fn get(&self, transaction_id: &str) -> Option<Arc<Transaction>> {
        self.inner.load().get(transaction_id).cloned()
    }

    /// Every transaction still being built - none of them has reached a table.
    pub fn get_all(&self) -> Vec<Arc<Transaction>> {
        self.inner.load().values().cloned().collect()
    }

    pub fn remove(&self, transaction_id: &str) -> Option<Arc<Transaction>> {
        let _guard = self.write_lock.lock();

        let mut map = self.inner.load().as_ref().clone();
        let removed = map.remove(transaction_id)?;
        self.inner.store(Arc::new(map));

        Some(removed)
    }

    /// Drops transactions which went quiet, together with everything they had
    /// accumulated. Returns their ids.
    pub fn gc(&self, now: DateTimeAsMicroseconds, ttl_secs: i64) -> Vec<String> {
        let expired: Vec<String> = self
            .inner
            .load()
            .values()
            .filter(|transaction| {
                now.unix_microseconds - transaction.get_last_incoming().unix_microseconds
                    > ttl_secs * 1_000_000
            })
            .map(|transaction| transaction.id.clone())
            .collect();

        for transaction_id in expired.iter() {
            self.remove(transaction_id);
        }

        expired
    }
}

impl Default for Transactions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(transactions: &Transactions) -> Arc<Transaction> {
        transactions.create(
            "default".to_string(),
            "traders".to_string(),
            DataSyncPeriod::default(),
        )
    }

    #[test]
    fn a_transaction_is_found_by_its_id_and_forgotten_when_removed() {
        let transactions = Transactions::new();
        let transaction = create(&transactions);

        assert!(transactions.get(&transaction.id).is_some());
        assert!(transactions.remove(&transaction.id).is_some());
        assert!(transactions.get(&transaction.id).is_none());
        // Removing it twice is not an error - that is what lets a client cancel
        // without checking whether it still exists.
        assert!(transactions.remove(&transaction.id).is_none());
    }

    #[test]
    fn a_transaction_which_went_quiet_is_collected() {
        let transactions = Transactions::new();
        let transaction = create(&transactions);

        let now = DateTimeAsMicroseconds::now();
        assert!(transactions.gc(now, TRANSACTION_TTL_SECS).is_empty());

        let much_later = DateTimeAsMicroseconds::new(
            now.unix_microseconds + (TRANSACTION_TTL_SECS + 1) * 1_000_000,
        );

        assert_eq!(
            transactions.gc(much_later, TRANSACTION_TTL_SECS),
            vec![transaction.id.clone()]
        );
        assert!(transactions.get(&transaction.id).is_none());
    }
}
