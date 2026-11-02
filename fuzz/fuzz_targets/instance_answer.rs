//! A direct instance query's answer (legacy unicast mDNS on a port of our
//! own): one datagram from anyone on the network, read into the table of
//! a wanted instance.
#![no_main]

use dstack::subscribe::instance::{Table, read};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = read(data, "vox 02@mics-01._netaudio-chan._udp.local.");
    let mut t = Table::default();
    t.lookup("mics-01", "Vox 02", 0);
    t.answer(data, 1);
    let _ = t.lookup("mics-01", "Vox 02", 2);
});
