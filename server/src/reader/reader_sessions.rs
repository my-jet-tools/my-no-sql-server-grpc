use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ahash::AHashMap;
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use super::ReaderSession;

/// A session which has not asked for anything in this long is forgotten. The
/// reader that comes back with it is told the session is unknown and starts over
/// - the same thing a dropped TCP connection used to mean.
pub const SESSION_TTL_SECS: i64 = 30;

/// Every live reader session, keyed by its id. Looked up on every `GetChange`
/// and changed only when a reader appears or goes away, so it is copy-on-write
/// behind an `ArcSwap`.
pub struct ReaderSessions {
    inner: ArcSwap<AHashMap<String, Arc<ReaderSession>>>,
    write_lock: Mutex<()>,
    next_id: AtomicU64,
}

impl ReaderSessions {
    pub fn new() -> Self {
        Self {
            inner: ArcSwap::from_pointee(AHashMap::new()),
            write_lock: Mutex::new(()),
            next_id: AtomicU64::new(0),
        }
    }

    pub fn create(
        &self,
        app_name: String,
        version: String,
        namespace: String,
        ip: String,
    ) -> Arc<ReaderSession> {
        let _guard = self.write_lock.lock();

        // The id only has to be unique among the sessions this process is
        // holding: a session never outlives the process, and a reader which comes
        // back to a restarted server is told the id is unknown anyway.
        let id = format!(
            "{}-{}",
            DateTimeAsMicroseconds::now().unix_microseconds,
            self.next_id.fetch_add(1, Ordering::SeqCst)
        );

        let session = Arc::new(ReaderSession::new(
            id.clone(),
            app_name,
            version,
            namespace,
            ip,
        ));

        let mut map = self.inner.load().as_ref().clone();
        map.insert(id, session.clone());
        self.inner.store(Arc::new(map));

        session
    }

    pub fn get(&self, session_id: &str) -> Option<Arc<ReaderSession>> {
        self.inner.load().get(session_id).cloned()
    }

    /// Every live session. The guard is dropped with the clone, so nothing that
    /// renders them holds the map.
    pub fn get_all(&self) -> Vec<Arc<ReaderSession>> {
        self.inner.load().values().cloned().collect()
    }

    pub fn remove(&self, session_id: &str) -> Option<Arc<ReaderSession>> {
        let _guard = self.write_lock.lock();

        let mut map = self.inner.load().as_ref().clone();
        let removed = map.remove(session_id)?;
        self.inner.store(Arc::new(map));

        Some(removed)
    }

    /// Sessions of this namespace which are subscribed to this table - the list a
    /// write has to notify.
    pub fn get_subscribed(&self, namespace: &str, table_name: &str) -> Vec<Arc<ReaderSession>> {
        self.inner
            .load()
            .values()
            .filter(|session| session.namespace == namespace && session.is_subscribed(table_name))
            .cloned()
            .collect()
    }

    /// Drops sessions which went quiet. Returns their ids.
    pub fn gc(&self, now: DateTimeAsMicroseconds, ttl_secs: i64) -> Vec<String> {
        let expired: Vec<String> = self
            .inner
            .load()
            .values()
            .filter(|session| {
                now.unix_microseconds - session.get_last_incoming().unix_microseconds
                    > ttl_secs * 1_000_000
            })
            .map(|session| session.id.clone())
            .collect();

        for session_id in expired.iter() {
            self.remove(session_id);
        }

        expired
    }
}

impl Default for ReaderSessions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_found_by_its_id_and_forgotten_when_removed() {
        let sessions = ReaderSessions::new();

        let session = sessions.create(
            "app".to_string(),
            "1.0".to_string(),
            "default".to_string(),
            "127.0.0.1:1".to_string(),
        );

        assert!(sessions.get(&session.id).is_some());
        assert!(sessions.remove(&session.id).is_some());
        assert!(sessions.get(&session.id).is_none());
    }

    #[test]
    fn only_sessions_of_the_same_namespace_and_table_are_notified() {
        let sessions = ReaderSessions::new();

        let mine = sessions.create(
            "a".to_string(),
            "1".to_string(),
            "default".to_string(),
            "127.0.0.1:1".to_string(),
        );
        mine.subscribe("traders");

        let other_table = sessions.create(
            "b".to_string(),
            "1".to_string(),
            "default".to_string(),
            "127.0.0.1:1".to_string(),
        );
        other_table.subscribe("orders");

        let other_namespace = sessions.create(
            "c".to_string(),
            "1".to_string(),
            "alpha".to_string(),
            "127.0.0.1:1".to_string(),
        );
        other_namespace.subscribe("traders");

        let subscribed = sessions.get_subscribed("default", "traders");

        assert_eq!(subscribed.len(), 1);
        assert_eq!(subscribed[0].id, mine.id);
    }

    #[test]
    fn a_session_which_went_quiet_is_collected() {
        let sessions = ReaderSessions::new();
        let session = sessions.create(
            "a".to_string(),
            "1".to_string(),
            "default".to_string(),
            "127.0.0.1:1".to_string(),
        );

        let now = DateTimeAsMicroseconds::now();
        assert!(sessions.gc(now, SESSION_TTL_SECS).is_empty());

        let much_later =
            DateTimeAsMicroseconds::new(now.unix_microseconds + (SESSION_TTL_SECS + 1) * 1_000_000);

        assert_eq!(
            sessions.gc(much_later, SESSION_TTL_SECS),
            vec![session.id.clone()]
        );
        assert!(sessions.get(&session.id).is_none());
    }
}
