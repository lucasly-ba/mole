//! Cancelling an in-flight OCR read.
//!
//! The background pre-warm and an on-demand hint share one OCR-at-a-time lock so
//! they never fire `tesseract` together and thrash the cores. The price is a
//! *collision*: if a hint is triggered while the pre-warm is mid-read, the hint
//! would have to wait out the whole pre-warm OCR (up to ~1s) before it could even
//! plan its own scan.
//!
//! [`Cancel`] lets the daemon preempt that. When an interaction starts it
//! [`abort`](Cancel::abort)s the pre-warm's token, which kills every running
//! `tesseract` child, so the pre-warm drops the cache lock at once and the hint
//! proceeds. The aborted read returns an error and — because the cache only
//! adopts a band into its baseline when that band is actually spliced in (see
//! [`cache`](super::cache)) — leaves no stale words behind. The on-demand path
//! carries its own token that is never aborted.

use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A shared cancellation token over a set of OCR subprocesses. Cheap to clone;
/// all clones share one state.
#[derive(Clone)]
pub struct Cancel {
    inner: Arc<Inner>,
}

struct Inner {
    aborted: AtomicBool,
    next_id: AtomicU64,
    /// Children currently running under this token, keyed by a registration id.
    children: Mutex<Vec<(u64, Arc<Mutex<Child>>)>>,
}

impl Cancel {
    pub fn new() -> Self {
        Cancel {
            inner: Arc::new(Inner {
                aborted: AtomicBool::new(false),
                next_id: AtomicU64::new(0),
                children: Mutex::new(Vec::new()),
            }),
        }
    }

    /// True once [`abort`](Cancel::abort) has been called and before the next
    /// [`reset`](Cancel::reset).
    pub fn aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::SeqCst)
    }

    /// Kill every currently-running child and refuse further reads until reset.
    pub fn abort(&self) {
        self.inner.aborted.store(true, Ordering::SeqCst);
        for (_, child) in self.inner.children.lock().unwrap().iter() {
            let _ = child.lock().unwrap().kill();
        }
    }

    /// Clear the aborted flag so the token can drive the next read.
    pub fn reset(&self) {
        self.inner.aborted.store(false, Ordering::SeqCst);
    }

    /// Register a running child so [`abort`](Cancel::abort) can kill it, returning
    /// an id to [`unregister`](Cancel::unregister) it once reaped. If an abort
    /// already landed, kill the child at once — this closes the window where a
    /// child is spawned just after `abort` walked its list.
    pub(super) fn register(&self, child: Arc<Mutex<Child>>) -> u64 {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        self.inner
            .children
            .lock()
            .unwrap()
            .push((id, Arc::clone(&child)));
        if self.aborted() {
            let _ = child.lock().unwrap().kill();
        }
        id
    }

    /// Drop a child from the kill set once it has exited.
    pub(super) fn unregister(&self, id: u64) {
        self.inner
            .children
            .lock()
            .unwrap()
            .retain(|(i, _)| *i != id);
    }
}

impl Default for Cancel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abort_then_reset_flips_the_flag() {
        let c = Cancel::new();
        assert!(!c.aborted());
        c.abort();
        assert!(c.aborted());
        c.reset();
        assert!(!c.aborted());
    }

    #[test]
    fn clones_share_state() {
        let a = Cancel::new();
        let b = a.clone();
        a.abort();
        assert!(b.aborted(), "abort on one clone is visible on the other");
    }

    #[test]
    fn unregister_after_abort_is_harmless() {
        let c = Cancel::new();
        // An id that was never (or no longer) registered: removing it is a no-op.
        c.abort();
        c.unregister(42);
        assert!(c.aborted());
    }
}
