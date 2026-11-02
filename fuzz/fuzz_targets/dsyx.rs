//! dantesync's time server reply (DSYX), read for the request id in the
//! first four bytes, and for the id the reply itself carries at the place
//! a reply's id sits, so the fuzzer reaches past the id check.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((id, reply)) = data.split_first_chunk::<4>() else {
        return;
    };
    let _ = dstack::clock::dsyx::parse(reply, u32::from_be_bytes(*id));
    if let Some(own) = reply.get(4..8) {
        let own = u32::from_be_bytes([own[0], own[1], own[2], own[3]]);
        let _ = dstack::clock::dsyx::parse(reply, own);
    }
});
