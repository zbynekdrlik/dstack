//! Logs a test captures on its own thread (`tracing::subscriber::with_default`).
//!
//! tracing caches each callsite's interest. A callsite first hit by another
//! test thread while no subscriber wanted it could keep "never", and a
//! test's scoped subscriber then saw nothing (seen as a rare empty capture
//! in `device::tests`). [`undecided`] sets, once per test binary, a global
//! subscriber that answers "sometimes" for every callsite and enables
//! nothing: no callsite is ever cached as "never", so every event asks the
//! thread's subscriber.

use std::sync::Once;

use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};

struct Undecided;

impl Subscriber for Undecided {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        false
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// Sets the global [`Undecided`] subscriber, once.
pub fn undecided() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        tracing::subscriber::set_global_default(Undecided)
            .expect("no other global subscriber in the test binary");
    });
}

#[test]
fn a_scoped_subscriber_sees_an_event_with_the_global_one_set() {
    undecided();
    assert!(!tracing::enabled!(tracing::Level::ERROR));
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let subscriber = tracing_subscriber::fmt().with_writer({
        let seen = std::sync::Arc::clone(&seen);
        move || {
            seen.store(true, std::sync::atomic::Ordering::Relaxed);
            std::io::sink()
        }
    });
    tracing::subscriber::with_default(subscriber.finish(), || tracing::info!("x"));
    assert!(seen.load(std::sync::atomic::Ordering::Relaxed));
}

/// The WARN lines `f` logs on this thread, as text.
pub fn capture_warnings(f: impl FnOnce()) -> String {
    capture_at(tracing::Level::WARN, f)
}

/// The lines at `level` and above `f` logs on this thread, as text.
pub fn capture_at(level: tracing::Level, f: impl FnOnce()) -> String {
    let captured = Captured::start(level);
    f();
    captured.text()
}

/// The lines at a level and above logged on this thread while it lives:
/// [`capture_at`]'s guard form, for async code its closure cannot await
/// (a current-thread runtime runs the spawned tasks on this thread too).
pub struct Captured {
    sink: Sink,
    _guard: tracing::subscriber::DefaultGuard,
}

#[derive(Clone)]
struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    /// Captures the lines at `level` and above from now on.
    pub fn start(level: tracing::Level) -> Captured {
        let sink = Sink(std::sync::Arc::default());
        let out = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || out.clone())
            .with_ansi(false)
            .with_max_level(level)
            .finish();
        undecided();
        Captured {
            sink,
            _guard: tracing::subscriber::set_default(subscriber),
        }
    }

    /// What was captured so far, as text.
    pub fn text(&self) -> String {
        let bytes = self
            .sink
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        String::from_utf8(bytes).unwrap_or_default()
    }
}
