//! The single home for the master's mutex-lock policy (issue #25): one helper
//! so the `expect`-on-poison rationale lives in exactly one place instead of
//! being copy-pasted at every lock site.

use std::sync::{Mutex, MutexGuard};

/// Locks `mutex`, propagating a poisoned lock by panicking.
///
/// # Panics
///
/// Panics if the lock is poisoned.
#[allow(clippy::expect_used)]
// mutex poison means a previous thread panicked; propagating is correct
pub(crate) fn lock_poison_free<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("lock poisoned")
}
