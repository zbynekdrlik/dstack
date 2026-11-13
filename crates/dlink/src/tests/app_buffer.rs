//! The bridge FIFO's target as a device term (dstack #247, the owner's
//! ROZHODNUTÉ of 13.11.2026): the app states its chunk, the target is that
//! and one pump block at the device's rate unless the app set one.

use super::*;

use crate::bridge::{MAX_APP_BUFFER_NS, MIN_APP_BUFFER_NS, chunk_ns};

/// OBS's program TX: a 48 kHz app on a 96 kHz device, block 128.
fn obs() -> Config {
    Config::new("obs-program", "/tmp/never-written.toml")
        .tx(["L", "R"])
        .sample_rate(48_000)
        .network_rate(96_000)
}

#[test]
fn an_app_that_states_no_chunk_has_no_bridge_buffer() {
    assert_eq!(obs().bridge_buffer(), None);
    assert!(obs().device().is_ok());
}

/// OBS's default: 1024 frames at 48 kHz (21.333 ms) + block 128 at the
/// device's 96 kHz (1.333 ms) = 22.667 ms.
#[test]
fn obs_default_is_its_chunk_and_one_block_at_the_device_rate() {
    let c = obs().app_buffer(chunk_ns(1024, 48_000), None);
    let b = c.bridge_buffer().unwrap();
    assert_eq!(b.chunk_ns(), 21_333_333);
    assert_eq!(b.block_ns(), 1_333_333);
    assert_eq!(b.target_ns(), 22_666_666);
    assert!(c.device().is_ok());
    // The block follows the config's own block and device rate.
    let b32 = obs().block(32).app_buffer(chunk_ns(1024, 48_000), None);
    assert_eq!(b32.bridge_buffer().unwrap().block_ns(), 333_333);
}

#[test]
fn a_configured_app_buffer_wins_verbatim() {
    let c = obs().app_buffer(chunk_ns(1024, 48_000), Some(30_000_001));
    assert_eq!(c.bridge_buffer().unwrap().target_ns(), 30_000_001);
    assert!(c.device().is_ok());
}

#[test]
fn an_app_buffer_out_of_range_is_refused_with_the_other_problems() {
    let low = obs().app_buffer(10_000_000, Some(MIN_APP_BUFFER_NS - 1));
    assert_eq!(
        low.device().unwrap_err(),
        ["app_buffer_ns: 999999 ns (0.999999 ms); 1 to 150 ms"]
    );
    let high = obs().app_buffer(10_000_000, Some(MAX_APP_BUFFER_NS + 1));
    assert!(high.device().is_err());
    // Listed after a latency problem, not instead of it.
    let both = high.receive_buffer_ns(0).network_latency_ns(0);
    let problems = both.device().unwrap_err();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].starts_with("network_latency_ns + receive_buffer_ns"));
    assert!(problems[1].starts_with("app_buffer_ns"));
}
