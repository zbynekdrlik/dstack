//! An info request the device does not answer (dstack #273): Companion's
//! Dante module asks opcode 0x100a once a second of every device, which no
//! Audinate device answers either. Each (opcode, sender) is logged once at
//! INFO, then only counted (a summary a minute), never a line a request.

use super::*;

use crate::test_log::Captured;

/// Companion's 0x100a request, as the issue's capture shows its opcode.
const OP_100A: [u8; 8] = [0x07, 0x34, 0x10, 0x0a, 0, 0, 0, 0];
/// Another opcode nobody answers.
const OP_0099: [u8; 8] = [0x07, 0x38, 0x00, 0x99, 0, 0, 0, 0x64];

#[tokio::test]
async fn an_unanswered_opcode_from_one_sender_is_logged_once_not_per_request() {
    let log = Captured::start(tracing::Level::INFO);
    let (at, info_rx, _beat_rx, to, sock) = loopback(NEVER).await;
    let task = tokio::spawn(serve_info(
        sock,
        InfoBuilder::new(identity()),
        to,
        Arc::new(NoClock),
        Arc::new(Changes::default()),
        Arc::new(Requests::default()),
    ));
    // The board and product info at start.
    recv(&info_rx).await;
    recv(&info_rx).await;
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for _ in 0..5 {
        peer.send_to(&asking(OP_100A), at).await.unwrap();
    }
    for _ in 0..3 {
        peer.send_to(&asking(OP_0099), at).await.unwrap();
    }
    // A request it answers, after them: once its reply is out, the server
    // has handled every request before it.
    peer.send_to(&captured(), at).await.unwrap();
    let reply = recv(&info_rx).await;
    assert_eq!(reply[24..32], BOARD_INFO);
    let text = log.text();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("not answered"))
        .collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].contains("0734100a00000000"), "{text}");
    assert!(lines[1].contains("0738009900000064"), "{text}");
    task.abort();
}
