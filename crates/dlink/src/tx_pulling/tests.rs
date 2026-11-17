// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use crate::bridge::Idle;

/// dstack #363: the TX pull's converter (`Rated` with no RX side) delays
/// TX exactly as the pump path's `Rated` does and as the stated (c) TX
/// says (`rated::delay_ns`, which `dlink::split` uses): no latency term
/// moves at any rate pair or block.
#[test]
fn the_pull_s_tx_converter_states_the_pump_path_s_tx_delay() {
    for (device, app) in [
        (96_000, 48_000),
        (96_000, 44_100),
        (48_000, 44_100),
        (96_000, 32_000),
        (48_000, 96_000),
    ] {
        for block in [32, 128] {
            let stated = crate::rated::delay_ns(device, app, block).unwrap().1;
            let pulled = Rated::new(Idle, device, app, block, 0, 2).unwrap();
            let pumped = Rated::new(Idle, device, app, block, 2, 2).unwrap();
            assert_eq!(pulled.delay_ns().1, stated, "{device}/{app}/{block}");
            assert_eq!(pumped.delay_ns().1, stated, "{device}/{app}/{block}");
        }
    }
}

/// A pull of `outputs` channels on a 48 kHz FIFO of `outputs` channels.
fn pull(outputs: usize) -> crate::bridge::TxPull {
    let fifo = Arc::new(crate::bridge::fifo_for(outputs, 48_000));
    let buffer = crate::bridge::AppBuffer::new(10_000_000, 2_666_667);
    let player = crate::bridge::Player::new(
        fifo,
        48_000,
        48_000,
        buffer,
        buffer.chunk_ns(),
        Arc::new(crate::bridge::Stats::default()),
    )
    .unwrap();
    crate::bridge::TxPull::new(player, outputs)
}

/// The start's refusal, which must come before anything runs (the place
/// is never reached).
fn refused(map: Map, fifo_channels: usize, outputs: usize) -> String {
    let config = Config::new("app-link", "/tmp/never-written.toml")
        .rx(["In 1", "In 2"])
        .tx(["Out 1", "Out 2"]);
    let e = Link::start_pulling_with_tx_on(
        &config,
        &Place::Ip(std::net::Ipv4Addr::new(192, 0, 2, 1)),
        map,
        Idle,
        Arc::new(crate::bridge::fifo_for(fifo_channels, 48_000)),
        pull(outputs),
        ClockSource::System,
    )
    .err()
    .expect("refused");
    match e {
        HostError::Setup(why) => why,
        other => panic!("{other:?}"),
    }
}

/// dstack #364: a bridge pulled both ways checks its RX FIFO and its TX
/// pull against the map and the device before it starts.
#[test]
fn a_pull_both_ways_that_does_not_fit_is_refused_before_the_start() {
    let map = |rx: Vec<usize>, tx: Vec<usize>| Map { rx, tx };
    assert_eq!(
        refused(map(vec![0, 1], vec![0]), 1, 1),
        crate::bridge::pull::check(&[0, 1], 2, 1).unwrap_err()
    );
    assert_eq!(
        refused(map(vec![0], vec![0, 1]), 1, 1),
        crate::bridge::tx_pull::check(&[0, 1], 2, 1).unwrap_err()
    );
    assert_eq!(
        refused(map(vec![0], vec![2]), 1, 1),
        "the app's TX channel 3 is past the device's 2 TX channels"
    );
}
