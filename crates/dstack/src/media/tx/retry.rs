// SPDX-License-Identifier: MIT OR Apache-2.0

//! A send the NIC refused for a moment, sent again (dstack #1): the FOH
//! PC's NIC, carrying other traffic, refused sends with WSAENOBUFS in
//! short bursts, and each refused packet was a hole at every receiver.
//! The decision ([`send_retry`]) and the loop ([`send_with_retries`]) are
//! pure; the TX pass gives them the real send, age and wait.

use std::io;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::LaneStats;

/// Sends again a packet the NIC refused for a moment: at most this many
/// more tries (dstack #1).
pub const SEND_RETRIES: u32 = 3;

/// The wait before each retry, ns.
pub const SEND_RETRY_WAIT_NS: u64 = 100_000;

/// A retry goes out only while the packet's age after the wait stays at
/// least this far under its flow's latency floor, ns.
pub const SEND_RETRY_HEADROOM_NS: u64 = 100_000;

/// Whether the raw OS error `code` of a failed send on `os` is a transient
/// refusal (the NIC's transmit ring or the socket full for a moment):
/// WSAENOBUFS (10055) and WSAEWOULDBLOCK (10035) on Windows; ENOBUFS (105),
/// EAGAIN/EWOULDBLOCK (11) and ENOMEM (12) elsewhere: Linux's numbers
/// (dstack builds for Linux and Windows only; another Unix numbers them
/// otherwise and would not retry). Every other error is lasting.
pub fn transient_refusal(os: crate::clock::Os, code: i32) -> bool {
    match os {
        crate::clock::Os::Windows => matches!(code, 10055 | 10035),
        crate::clock::Os::Other => matches!(code, 105 | 11 | 12),
    }
}

/// The wait before sending again a packet whose send failed with the raw
/// OS error `code` (`None`: the error has none), after `attempt` retries,
/// `age_ns` past its stamp now, on a flow whose latency floor is
/// `floor_ns` ([`flow_latency_ns`](super::flow_latency_ns)); `None`: no
/// retry. A transient refusal ([`transient_refusal`]) is retried [`SEND_RETRIES`] times at most,
/// [`SEND_RETRY_WAIT_NS`] apart, and only while the packet's age after the
/// wait stays [`SEND_RETRY_HEADROOM_NS`] under the floor: a packet that
/// would arrive late anyway is not held (dstack #1).
pub fn send_retry(
    os: crate::clock::Os,
    code: Option<i32>,
    attempt: u32,
    age_ns: u64,
    floor_ns: u64,
) -> Option<std::time::Duration> {
    let transient = code.is_some_and(|c| transient_refusal(os, c));
    let in_time = age_ns.saturating_add(SEND_RETRY_WAIT_NS + SEND_RETRY_HEADROOM_NS) <= floor_ns;
    (transient && attempt < SEND_RETRIES && in_time)
        .then(|| std::time::Duration::from_nanos(SEND_RETRY_WAIT_NS))
}

/// One packet's send with its retries: `send` once, then again after each
/// wait [`send_retry`] gives (`age` the packet's age now, read before each
/// decision; `wait` the wait itself). The final outcome and the retries
/// made. No allocation: the TX thread's hot path.
pub(super) fn send_with_retries(
    os: crate::clock::Os,
    floor_ns: u64,
    mut age: impl FnMut() -> u64,
    mut send: impl FnMut() -> io::Result<bool>,
    mut wait: impl FnMut(std::time::Duration),
) -> (io::Result<bool>, u32) {
    let mut sent = send();
    let mut retries = 0;
    while let Err(e) = &sent
        && let Some(d) = send_retry(os, e.raw_os_error(), retries, age(), floor_ns)
    {
        wait(d);
        retries += 1;
        sent = send();
    }
    (sent, retries)
}

/// Waits `d` on the calling thread, yielding while it spins (the TX
/// thread's wait before a retry: far below a timer's resolution on
/// Windows). The same yielding spin as the clock's tail on Windows
/// (`clock::sys_windows`), on `Instant` so it runs on every OS.
pub(super) fn spin_for(d: std::time::Duration) {
    let start = Instant::now();
    while !d.saturating_sub(start.elapsed()).is_zero() {
        std::thread::yield_now();
    }
}

/// Counts a packet sent after `retries` retries ([`send_with_retries`]):
/// one in `send_retries` when it went out after any; a final refusal is
/// `count_send`'s.
pub(super) fn count_retried(stats: &LaneStats, sent: &io::Result<bool>, retries: u32) {
    if sent.is_ok() && retries > 0 {
        stats.send_retries.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
