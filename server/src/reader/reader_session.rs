use std::collections::VecDeque;
use std::time::Duration;

use ahash::AHashSet;
use parking_lot::Mutex;
use rust_extensions::date_time::{AtomicDateTimeAsMicroseconds, DateTimeAsMicroseconds};
use tokio::sync::Notify;

use super::SyncChunk;

/// How far behind a reader is allowed to fall before the server stops holding
/// its backlog. `GetChange` answers with one chunk per call, so a reader which is
/// keeping up drains a thousand of them in about a second of round trips; one
/// which is a thousand behind is not catching up one round trip at a time. The
/// backlog is not only memory either - a queued `UpdateRows` holds `Arc<DbRow>`
/// handles, so it keeps rows alive which the table itself has already dropped.
pub const MAX_QUEUED_CHUNKS: usize = 1000;

struct ReaderSessionInner {
    tables: AHashSet<String>,
    queue: VecDeque<SyncChunk>,
    /// Set when the queue passed the ceiling and was thrown away. What is left
    /// is a picture with a hole in it, and the reader is told to start over
    /// instead - the failure model has exactly one recovery, and this is it.
    overflowed: bool,
}

/// One reader, from its `Greeting` until it stops asking or the server forgets
/// it. Everything it still has to be told sits in its queue, in the order the
/// server put it there.
pub struct ReaderSession {
    pub id: String,
    pub app_name: String,
    pub version: String,
    pub namespace: String,
    /// Where the greeting came from. Behind a proxy this is the proxy - there is
    /// no forwarded-for equivalent on this transport, and inventing one would be
    /// a header nobody sets.
    pub ip: String,
    pub connected: DateTimeAsMicroseconds,
    last_incoming: AtomicDateTimeAsMicroseconds,
    // parking_lot: the critical section is a push or a pop and never awaits. The
    // long poll waits on the notify, with the lock long since released.
    inner: Mutex<ReaderSessionInner>,
    has_data: Notify,
}

impl ReaderSession {
    pub fn new(
        id: String,
        app_name: String,
        version: String,
        namespace: String,
        ip: String,
    ) -> Self {
        Self {
            id,
            app_name,
            version,
            namespace,
            ip,
            connected: DateTimeAsMicroseconds::now(),
            last_incoming: AtomicDateTimeAsMicroseconds::now(),
            inner: Mutex::new(ReaderSessionInner {
                tables: AHashSet::new(),
                queue: VecDeque::new(),
                overflowed: false,
            }),
            has_data: Notify::new(),
        }
    }

    pub fn touch(&self) {
        self.last_incoming.update(DateTimeAsMicroseconds::now());
    }

    pub fn get_last_incoming(&self) -> DateTimeAsMicroseconds {
        self.last_incoming.as_date_time()
    }

    /// Meant to run under the table's write lock - see
    /// `DbTable::register_and_snapshot`.
    pub fn subscribe(&self, table_name: &str) {
        self.inner.lock().tables.insert(table_name.to_string());
    }

    pub fn is_subscribed(&self, table_name: &str) -> bool {
        self.inner.lock().tables.contains(table_name)
    }

    /// Everything the monitoring views want from behind the lock, in one take.
    /// Two getters would take it twice, and it is the same lock the long poll
    /// pops from and every write's `enqueue` pushes into.
    pub fn get_monitoring_snapshot(&self) -> (Vec<String>, usize) {
        let inner = self.inner.lock();

        let mut tables: Vec<String> = inner.tables.iter().cloned().collect();
        // The set is a hash set, so unsorted output would reshuffle on every
        // scrape and make two samples impossible to compare.
        tables.sort_unstable();

        (tables, inner.queue.len())
    }

    pub fn enqueue(&self, chunks: impl IntoIterator<Item = SyncChunk>) {
        let mut queued = false;

        {
            let mut inner = self.inner.lock();

            // Nothing is kept for a session which is already being thrown away:
            // what would be pushed now is the tail of a picture whose head is
            // gone, and the reader is going to ask for the whole of it again.
            if inner.overflowed {
                return;
            }

            for chunk in chunks {
                inner.queue.push_back(chunk);
                queued = true;
            }

            if inner.queue.len() > MAX_QUEUED_CHUNKS {
                inner.queue.clear();
                // The queue is what the backlog is made of, and a `VecDeque`
                // keeps its capacity - so does the memory this is about.
                inner.queue.shrink_to_fit();
                inner.overflowed = true;
            }
        }

        if queued {
            // notify_one keeps a permit when nobody is waiting, so a chunk queued
            // between the poll's own check and its wait is never slept through.
            // Woken even when the queue was just thrown away: the poll finds
            // nothing and answers, and the call after it is the one which tells
            // the reader to start over - five seconds sooner than waiting for the
            // long poll to give up on its own.
            self.has_data.notify_one();
        }
    }

    /// Whether this session's backlog was thrown away. The session is worth
    /// nothing after that, and whoever asks it for a change evicts it.
    pub fn has_overflowed(&self) -> bool {
        self.inner.lock().overflowed
    }

    fn pop(&self) -> Option<SyncChunk> {
        self.inner.lock().queue.pop_front()
    }

    /// The long poll: answers as soon as there is something, and gives up after
    /// `wait_for` so the caller can send the empty answer that doubles as a ping.
    pub async fn get_next_chunk(&self, wait_for: Duration) -> Option<SyncChunk> {
        if let Some(chunk) = self.pop() {
            return Some(chunk);
        }

        let _ = tokio::time::timeout(wait_for, self.has_data.notified()).await;

        self.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_session() -> ReaderSession {
        ReaderSession::new(
            "1".to_string(),
            "app".to_string(),
            "1.0".to_string(),
            "default".to_string(),
            "127.0.0.1:1".to_string(),
        )
    }

    fn clean_table() -> SyncChunk {
        SyncChunk::CleanTable {
            table_name: "traders".to_string(),
        }
    }

    /// A reader which stopped draining must not cost the server its heap: the
    /// backlog is dropped whole, and what the reader gets instead is the
    /// re-subscribe every other failure gets.
    #[test]
    fn a_backlog_which_grows_past_the_ceiling_is_thrown_away_with_the_session() {
        let session = new_session();

        session.enqueue((0..MAX_QUEUED_CHUNKS).map(|_| clean_table()));

        assert_eq!(session.get_monitoring_snapshot().1, MAX_QUEUED_CHUNKS);
        assert!(!session.has_overflowed());

        session.enqueue([clean_table()]);

        assert!(session.has_overflowed());
        assert_eq!(session.get_monitoring_snapshot().1, 0);

        // And nothing accumulates behind it while the reader is on its way back.
        session.enqueue((0..MAX_QUEUED_CHUNKS).map(|_| clean_table()));

        assert_eq!(session.get_monitoring_snapshot().1, 0);
    }

    /// The half of it which is not about memory: what is left of a queue that
    /// was thrown away is a picture with a hole in it, so none of it is served.
    #[tokio::test]
    async fn nothing_of_a_thrown_away_backlog_is_still_delivered() {
        let session = new_session();

        session.enqueue((0..MAX_QUEUED_CHUNKS + 1).map(|_| clean_table()));

        assert!(
            session
                .get_next_chunk(Duration::from_millis(1))
                .await
                .is_none()
        );
    }
}
