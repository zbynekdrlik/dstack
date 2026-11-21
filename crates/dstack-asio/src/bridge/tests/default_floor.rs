// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #482 (dasio#190): the default target (the driver's buffer + one
//! pump block, nothing configured) is a bridge term and takes the bridge's
//! floor ([`MIN_BRIDGE_TARGET_NS`]), not dlink's 1 ms: dsg's SoundGrid at
//! 32 frames with 32-frame blocks at 96 kHz (0.666666 ms) was refused at
//! the driver's start.

use dlink::bridge::MIN_APP_BUFFER_NS;

use super::*;

/// A bridge whose pump block is `block_frames` at 96 kHz.
fn with_block(block_frames: u32) -> Bridge {
    let mut s = spec(1, 1);
    s.block_frames = block_frames;
    bridge(s)
}

/// dsg's shape: both directions' default is 0.666666 ms on the bridge's
/// floor, the start takes it and both players play it unclamped.
#[test]
fn a_32_frame_driver_starts_on_its_default_target_both_ways() {
    let at = opened(96_000.0, 32);
    let mut b = with_block(32);
    for buffer in [b.to_asio_app_buffer(&at), b.from_asio_app_buffer(&at)] {
        assert_eq!(buffer.target_ns(), 666_666);
        assert_eq!(buffer.floor_ns(), MIN_BRIDGE_TARGET_NS);
        assert_eq!(buffer.problem(), None);
        assert_eq!(buffer.clamped_ns(), 666_666);
    }
    let _dante = b.dante_side();
    assert!(b.asio_side(&at).is_ok());
    assert!((b.to_asio_stats().target_ms() - 0.666666).abs() < 1e-12);
    assert!((b.from_asio_stats().target_ms() - 0.666666).abs() < 1e-12);
}

/// A default at the floor starts; one under it is refused with the floor in
/// the text, on both directions (the start names the first refused).
#[test]
fn a_default_under_the_bridge_floor_is_still_refused() {
    let at = opened(96_000.0, 24);
    let mut b = with_block(24);
    assert_eq!(b.app_buffer(&at).target_ns(), MIN_BRIDGE_TARGET_NS);
    assert!(b.asio_side(&at).is_ok());
    // 23 frames 239583 ns + 24 frames 250000 ns (`chunk_ns` rounds each).
    let at = opened(96_000.0, 23);
    let mut b = with_block(24);
    let text = "app_buffer_ns: 489583 ns (0.489583 ms); 0.5 to 150 ms";
    assert_eq!(b.to_asio_app_buffer(&at).problem().as_deref(), Some(text));
    assert_eq!(b.from_asio_app_buffer(&at).problem().as_deref(), Some(text));
    assert_eq!(b.asio_side(&at).err().as_deref(), Some(text));
}

/// A configured shared `app_buffer_ns` keeps dlink's 1 ms (dasio's
/// `[bridge]` check reads it with dlink's range), even at the value a
/// default would take.
#[test]
fn a_configured_shared_target_keeps_dlink_s_floor() {
    let at = opened(96_000.0, 32);
    let mut s = spec(1, 1);
    s.block_frames = 32;
    s.app_buffer_ns = Some(666_666);
    let mut b = bridge(s);
    assert_eq!(b.app_buffer(&at).floor_ns(), MIN_APP_BUFFER_NS);
    assert_eq!(
        b.asio_side(&at).err().as_deref(),
        Some("app_buffer_ns: 666666 ns (0.666666 ms); 1 to 150 ms")
    );
}
