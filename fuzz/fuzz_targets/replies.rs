//! Replies other devices send to our requests: ARC (our directory reads),
//! flow control (our subscriptions), the flow handle in a grant, and a
//! string at an offset of a body. Each reply is read for the seq and op1
//! the input's first bytes give, and for its own (header bytes 4..8), so
//! the fuzzer reaches past the match.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((head, d)) = data.split_first_chunk::<6>() else {
        return;
    };
    let seq = u16::from_be_bytes([head[0], head[1]]);
    let op1 = u16::from_be_bytes([head[2], head[3]]);
    let offset = u16::from_be_bytes([head[4], head[5]]);
    let _ = dstack::arc::client::reply_to(d, seq, op1);
    let _ = dstack::flows::client::reply_of(d, seq, op1);
    let _ = dstack::flows::control::reply_handle(d);
    let _ = dstack::wire::string_at(d, offset);
    if let Some(own) = d.get(4..8) {
        let seq = u16::from_be_bytes([own[0], own[1]]);
        let op1 = u16::from_be_bytes([own[2], own[3]]);
        let _ = dstack::arc::client::reply_to(d, seq, op1);
        let _ = dstack::flows::client::reply_of(d, seq, op1);
    }
});
