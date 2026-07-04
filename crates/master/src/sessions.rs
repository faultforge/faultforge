//! The per-hostname outbound-session map (design D4): dispatch needs "send
//! this frame to that agent". Entries live and die with their gRPC stream —
//! a different lifecycle from the registry (which keeps entries after
//! disconnect), so this is a separate map, one owner, no lock coupling.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tonic::Status;

use faultforge_proto::Hostname;
use faultforge_proto::v1::ServerMessage;

/// The outbound half of one agent's `Session` stream.
pub type SessionTx = mpsc::Sender<Result<ServerMessage, Status>>;

/// Live sessions keyed by hostname. A new `Register` for a known hostname
/// supersedes the previous handle (the newest stream wins, matching the
/// registry's supersede-on-reconnect rule); a closing stream removes its entry
/// only while it is still the current one.
#[derive(Debug, Default)]
pub struct SessionMap {
    inner: Mutex<HashMap<Hostname, (u64, SessionTx)>>,
    epochs: AtomicU64,
}

impl SessionMap {
    /// Insert (or supersede) the session for `hostname`, returning the epoch
    /// token the owning stream must present to remove itself later.
    ///
    /// # Panics
    ///
    /// Panics if the map mutex is poisoned.
    pub fn insert(&self, hostname: &Hostname, tx: SessionTx) -> u64 {
        let epoch = self.epochs.fetch_add(1, Ordering::Relaxed);
        #[allow(clippy::expect_used)]
        // mutex poison means a previous thread panicked; propagating is correct
        self.inner
            .lock()
            .expect("session map lock poisoned")
            .insert(hostname.clone(), (epoch, tx));
        epoch
    }

    /// Remove the entry for `hostname` only if it still belongs to `epoch` —
    /// a superseded stream closing late must not evict its successor.
    ///
    /// # Panics
    ///
    /// Panics if the map mutex is poisoned.
    pub fn remove_if_current(&self, hostname: &Hostname, epoch: u64) {
        #[allow(clippy::expect_used)]
        // mutex poison means a previous thread panicked; propagating is correct
        let mut inner = self.inner.lock().expect("session map lock poisoned");
        if inner.get(hostname).is_some_and(|(e, _)| *e == epoch) {
            inner.remove(hostname);
        }
    }

    /// The current outbound sender for `hostname`, if a session is live.
    ///
    /// # Panics
    ///
    /// Panics if the map mutex is poisoned.
    #[must_use]
    pub fn sender(&self, hostname: &Hostname) -> Option<SessionTx> {
        #[allow(clippy::expect_used)]
        // mutex poison means a previous thread panicked; propagating is correct
        self.inner
            .lock()
            .expect("session map lock poisoned")
            .get(hostname)
            .map(|(_, tx)| tx.clone())
    }

    /// Whether `hostname` has a live session right now.
    ///
    /// # Panics
    ///
    /// Panics if the map mutex is poisoned.
    #[must_use]
    pub fn is_connected(&self, hostname: &Hostname) -> bool {
        #[allow(clippy::expect_used)]
        // mutex poison means a previous thread panicked; propagating is correct
        self.inner
            .lock()
            .expect("session map lock poisoned")
            .contains_key(hostname)
    }
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;

    fn hostname(s: &str) -> Hostname {
        Hostname::parse(s).unwrap()
    }

    fn tx() -> SessionTx {
        mpsc::channel(1).0
    }

    #[test]
    fn insert_and_sender_round_trip() {
        let map = SessionMap::default();
        assert!(!map.is_connected(&hostname("web-01")));
        map.insert(&hostname("web-01"), tx());
        assert!(map.is_connected(&hostname("web-01")));
        assert!(map.sender(&hostname("web-01")).is_some());
        assert!(map.sender(&hostname("db-01")).is_none());
    }

    #[test]
    fn newer_register_supersedes_and_late_close_does_not_evict_it() {
        let map = SessionMap::default();
        let first_epoch = map.insert(&hostname("web-01"), tx());
        let second_epoch = map.insert(&hostname("web-01"), tx());
        assert_ne!(first_epoch, second_epoch);

        // The superseded stream closes late: the current session must survive.
        map.remove_if_current(&hostname("web-01"), first_epoch);
        assert!(map.is_connected(&hostname("web-01")));

        map.remove_if_current(&hostname("web-01"), second_epoch);
        assert!(!map.is_connected(&hostname("web-01")));
    }
}
