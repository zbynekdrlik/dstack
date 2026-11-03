// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::time::Duration;

const MS: u64 = 1_000_000;

#[test]
fn only_a_transient_refusal_is_retried() {
    use crate::clock::Os;
    // WSAENOBUFS, WSAEWOULDBLOCK on Windows.
    for code in [10055, 10035] {
        assert!(transient_refusal(Os::Windows, code), "{code}");
        assert!(!transient_refusal(Os::Other, code), "{code}");
    }
    // ENOBUFS, EAGAIN/EWOULDBLOCK, ENOMEM on Linux.
    for code in [105, 11, 12] {
        assert!(transient_refusal(Os::Other, code), "{code}");
        assert!(!transient_refusal(Os::Windows, code), "{code}");
    }
    // Refused, unreachable, reset, access, too long: never.
    for code in [0, 1, 13, 90, 101, 111, 10013, 10040, 10051, 10054, 10061] {
        assert!(!transient_refusal(Os::Windows, code), "{code}");
        assert!(!transient_refusal(Os::Other, code), "{code}");
    }
}

#[test]
fn a_refused_send_is_retried_a_few_times_within_the_flows_latency() {
    use crate::clock::Os;
    let wait = Some(Duration::from_nanos(SEND_RETRY_WAIT_NS));
    assert_eq!(wait, Some(Duration::from_micros(100)));
    let floor = MS;
    // 3 retries at most.
    for attempt in 0..SEND_RETRIES {
        assert_eq!(
            send_retry(Os::Windows, Some(10055), attempt, 0, floor),
            wait
        );
    }
    assert_eq!(SEND_RETRIES, 3);
    assert_eq!(send_retry(Os::Windows, Some(10055), 3, 0, floor), None);
    assert_eq!(send_retry(Os::Other, Some(105), 2, 0, floor), wait);
    // Not transient, no code, the other OS's code: at once.
    assert_eq!(send_retry(Os::Windows, Some(10054), 0, 0, floor), None);
    assert_eq!(send_retry(Os::Windows, None, 0, 0, floor), None);
    assert_eq!(send_retry(Os::Other, Some(10055), 0, 0, floor), None);
    // The age after the 100 µs wait must stay 100 µs under the floor.
    let age = floor - 200_000;
    assert_eq!(send_retry(Os::Other, Some(11), 0, age, floor), wait);
    assert_eq!(send_retry(Os::Other, Some(11), 0, age + 1, floor), None);
    assert_eq!(send_retry(Os::Other, Some(11), 0, floor, floor), None);
    // A packet already past its floor (or at a huge age) is never held.
    assert_eq!(send_retry(Os::Other, Some(12), 0, u64::MAX, floor), None);
    assert_eq!(send_retry(Os::Other, Some(12), 0, 0, 199_999), None);
    assert_eq!(send_retry(Os::Other, Some(12), 0, 0, 200_000), wait);
}

/// A send that fails with each of `fails` (raw OS codes) in turn, then
/// succeeds; the sends made so far.
fn flaky(
    fails: &[i32],
) -> (
    impl FnMut() -> io::Result<bool> + '_,
    std::rc::Rc<std::cell::Cell<usize>>,
) {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = std::rc::Rc::clone(&calls);
    let send = move || {
        let n = seen.get();
        seen.set(n + 1);
        match fails.get(n) {
            Some(&code) => Err(io::Error::from_raw_os_error(code)),
            None => Ok(false),
        }
    };
    (send, calls)
}

#[test]
fn send_with_retries_sends_a_refused_packet_again_and_recovers_it() {
    use crate::clock::Os;
    let mut waits = Vec::new();
    let (send, calls) = flaky(&[10055, 10035]);
    let (sent, retries) = send_with_retries(Os::Windows, MS, || 0, send, |d| waits.push(d));
    assert!(matches!(sent, Ok(false)), "{sent:?}");
    assert_eq!((retries, calls.get()), (2, 3));
    assert_eq!(waits, vec![Duration::from_micros(100); 2]);
}

#[test]
fn send_with_retries_gives_up_after_three_retries_with_the_last_error() {
    use crate::clock::Os;
    let mut waits = 0;
    let (send, calls) = flaky(&[105, 105, 105, 11, 105]);
    let (sent, retries) = send_with_retries(Os::Other, MS, || 0, send, |_| waits += 1);
    assert_eq!(sent.unwrap_err().raw_os_error(), Some(11));
    assert_eq!((retries, calls.get(), waits), (3, 4, 3));
}

#[test]
fn send_with_retries_does_not_retry_a_lasting_error_or_a_packet_near_its_latency() {
    use crate::clock::Os;
    // Connection refused: one send.
    let (send, calls) = flaky(&[111]);
    let (sent, retries) = send_with_retries(Os::Other, MS, || 0, send, |_| {});
    assert!(sent.is_err());
    assert_eq!((retries, calls.get()), (0, 1));
    // The age grows with each read: retried while it stays in the floor.
    let mut age = 0;
    let (send, calls) = flaky(&[105, 105, 105]);
    let (sent, retries) = send_with_retries(
        Os::Other,
        700_000,
        || {
            age += 250_000;
            age
        },
        send,
        |_| {},
    );
    assert!(sent.is_err());
    // 250 µs and 500 µs pass (+ 200 µs <= 700 µs), 750 µs does not.
    assert_eq!((retries, calls.get()), (2, 3));
}

#[test]
fn a_packet_sent_on_a_retry_counts_once_and_a_lost_one_not() {
    let stats = LaneStats::default();
    count_retried(&stats, &Ok(false), 0);
    count_retried(&stats, &Err(io::Error::from_raw_os_error(10055)), 3);
    assert_eq!(stats.send_retries.load(Ordering::Relaxed), 0);
    count_retried(&stats, &Ok(false), 1);
    count_retried(&stats, &Ok(true), 3);
    assert_eq!(stats.send_retries.load(Ordering::Relaxed), 2);
}

#[test]
fn the_wait_before_a_retry_spins_its_time() {
    let start = Instant::now();
    spin_for(Duration::from_micros(300));
    assert!(start.elapsed() >= Duration::from_micros(300));
    // None at all returns at once.
    spin_for(Duration::ZERO);
}
