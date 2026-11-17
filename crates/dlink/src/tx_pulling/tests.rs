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
