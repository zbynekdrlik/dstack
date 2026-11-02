//! A media packet from a transmitter: its header, then its frames, read
//! once with the slot count and width the first two bytes give raw (the
//! refusals) and once mapped onto what a flow can carry (the frame loop).
#![no_main]

use dstack::media::packet;
use libfuzzer_sys::fuzz_target;

/// The sample widths a flow carries, bytes.
const WIDTHS: [usize; 3] = [2, 3, 4];

fuzz_target!(|data: &[u8]| {
    let Some((&[s, b], buf)) = data.split_first_chunk::<2>() else {
        return;
    };
    let _ = packet::read_header(buf);
    let _ = packet::read_frames(buf, usize::from(s), usize::from(b), |_, _, _| {});
    let slots = 1 + usize::from(s) % 64;
    let bps = WIDTHS[usize::from(b) % WIDTHS.len()];
    let _ = packet::read_frames(buf, slots, bps, |_, _, _| {});
});
