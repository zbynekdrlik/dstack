//! The info server's multicast sends failing for a long time (dstack
//! #340): on dsg (Windows, 15.-16.11.2026) every heartbeat and info packet
//! failed with WSAEADDRNOTAVAIL (10049) for ~24 h, logged as one WARN a
//! packet (11 678 lines). A sustained failure is counted and logged at
//! most once a minute, with the count and the OS error.

use super::*;

use crate::test_log::Captured;

/// Where a send always fails on Linux: the broadcast address without
/// `SO_BROADCAST` (EACCES, os error 13), as each send failed on dsg.
const REFUSED: &str = "255.255.255.255:9";

#[tokio::test]
async fn a_sustained_send_failure_is_one_warning_not_one_a_packet() {
    let log = Captured::start(tracing::Level::WARN);
    let (_at, info_rx, _beat_rx, mut to, sock) = loopback(Duration::from_millis(5)).await;
    to.heartbeat = REFUSED.parse().unwrap();
    let task = tokio::spawn(serve_info(
        sock,
        InfoBuilder::new(identity()),
        to,
        Arc::new(NoClock),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    // The board and product info still go out.
    recv(&info_rx).await;
    recv(&info_rx).await;
    // A heartbeat every 5 ms for 300 ms: dozens of refused sends.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "the server goes on: {:?}", task.await);
    task.abort();
    let text = log.text();
    let warnings = text.lines().filter(|l| l.contains("WARN")).count();
    assert_eq!(warnings, 1, "one warning, not one a packet:\n{text}");
    assert!(text.contains("os error 13"), "the OS error is named:\n{text}");
}
