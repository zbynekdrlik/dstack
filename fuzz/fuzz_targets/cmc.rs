//! CMC (UDP 8800): one datagram, answered or not.
#![no_main]

use std::net::Ipv4Addr;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = dstack::cmc::handle_cmc(data, Ipv4Addr::new(10, 0, 0, 1), 0);
});
