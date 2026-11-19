//! A fake interface list for the wiring tests of a watched socket's
//! thread (dstack #409): one address on an interface whose index the test
//! moves, each read counted, so a test waits for the thread to have READ
//! the state it set, never a fixed sleep (`.claude/rules/multicast.md`).

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{Link, Lister};

/// Every wait of these helpers ends.
const BOUND: Duration = Duration::from_secs(5);

/// The list as a test moves it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Fake {
    /// The interface's index now.
    pub index: Arc<AtomicU32>,
    /// The reads so far.
    pub reads: Arc<AtomicU64>,
    /// The list gives the interface no index (dstack #417).
    pub no_index: Arc<AtomicBool>,
    /// The address is not listed at all (dstack #444: removed).
    pub gone: Arc<AtomicBool>,
}

impl Fake {
    /// `ip` on the interface "lo" at `index`.
    pub fn at(index: u32) -> Self {
        let fake = Fake::default();
        fake.index.store(index, Ordering::Relaxed);
        fake
    }

    /// The list of `ip` read every `every`, each read counted.
    pub fn lister(&self, ip: Ipv4Addr, every: Duration) -> Lister {
        let fake = self.clone();
        Lister {
            read: Box::new(move || {
                fake.reads.fetch_add(1, Ordering::Relaxed);
                if fake.gone.load(Ordering::Relaxed) {
                    return Ok(Vec::new());
                }
                Ok(vec![Link {
                    name: "lo".to_string(),
                    ip,
                    index: (!fake.no_index.load(Ordering::Relaxed))
                        .then(|| fake.index.load(Ordering::Relaxed)),
                }])
            }),
            every,
        }
    }

    /// The interface re-created: a new index (or the same one, its address
    /// added again after [`Fake::remove`]).
    pub fn recreate(&self, index: u32) {
        self.no_index.store(false, Ordering::Relaxed);
        self.gone.store(false, Ordering::Relaxed);
        self.index.store(index, Ordering::Relaxed);
    }

    /// The address removed: the list no longer holds it (dstack #444).
    pub fn remove(&self) {
        self.gone.store(true, Ordering::Relaxed);
    }

    /// The interface listed with no index (the OS gave none).
    pub fn without_index(&self) {
        self.no_index.store(true, Ordering::Relaxed);
    }

    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    /// Waits for `n` more reads than now (a read before the wait counts
    /// not), at most [`BOUND`]: one whole look happened after the call.
    pub fn read_more(&self, n: u64) {
        let at = self.reads();
        until("the interface list read", || self.reads() >= at + n);
    }
}

/// Waits until `f` is true, at most [`BOUND`].
pub(crate) fn until(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + BOUND;
    while !f() {
        assert!(Instant::now() < deadline, "{what} never happened");
        std::thread::sleep(Duration::from_millis(5));
    }
}
