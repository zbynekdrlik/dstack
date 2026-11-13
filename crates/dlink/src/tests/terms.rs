//! dlink's latency in named terms (dstack #244, the owner's ROZHODNUTÉ of
//! 12.11.2026): (a) the Dante network latency, (b) the receive buffer,
//! P = (a) + (b), and (c) the app hand-off, block plus converter (W plus
//! converter on the TX side). The defaults keep v0.6.105's P.

use super::*;

use Directions::{Both, Rx, Tx};
use dstack::stream::{BLOCKS, MAX_WORK_MARGIN};

fn rx_only(name: &str, app: u32, net: u32) -> Config {
    Config::new(name, "/tmp/never-written.toml")
        .rx(["In"])
        .sample_rate(app)
        .network_rate(net)
}

/// The editing PC's OBS device (camera-box's dante-input): RX only, a
/// 96 kHz network, a 48 kHz app, block 128. v0.6.105 gave it P 13.041666 ms
/// (15 ms less B 1.333333 ms less the half-band's RX delay 0.625 ms); the
/// same P is now (a) 1 ms + (b) 12.041666 ms, each term named.
#[test]
fn obs_on_the_editing_pc_keeps_p_13_041666_ms_in_named_terms() {
    let c = rx_only("obs", 48_000, 96_000);
    let s = c.split().unwrap();
    assert_eq!(
        s,
        Split {
            network_latency_ns: 1_000_000,
            receive_buffer_ns: 12_041_666,
            latency_ns: 13_041_666,
            block_ns: 1_333_333,
            rx_convert_ns: 625_000,
            tx_convert_ns: 635_416,
            rx_handoff_ns: 1_958_333,
            tx_handoff_ns: 15_635_416,
            margin: 1_440,
            work_margin_ns: 15_000_000,
            tx_path_ns: 28_677_082,
            rx_path_ns: 14_999_999,
        }
    );
    assert_eq!(latency_samples(s.latency_ns, 96_000), 1_252);
    let cfg = c.device().unwrap();
    assert_eq!((cfg.latency_ns, cfg.work_margin), (13_041_666, 1_440));
    // The owner sizes (b) from measurement: 1 ms + 3 ms is P 4 ms, the RX
    // path 5.958333 ms.
    let tight = c.receive_buffer_ns(3_000_000).split().unwrap();
    assert_eq!(
        (tight.receive_buffer_ns, tight.latency_ns, tight.rx_path_ns),
        (3_000_000, 4_000_000, 5_958_333)
    );
}

/// dstack #244: an app that sets no receive buffer keeps the P of v0.6.105
/// (the old 15 ms budget's split; dstack #1's tables), for both directions
/// and RX only, every common rate pair, blocks 128 and 256: (b) = P - 1 ms,
/// W 6 ms with TX and 15 ms RX only, each path P + its hand-off.
#[test]
fn the_defaults_keep_the_latency_of_v0_6_105() {
    let ms10 = |ns: u64| ns / 10_000;
    // (device, app, block, directions) -> (P ns, TX path, RX path in 10 us),
    // v0.6.105's `the_default_15_ms_fits_every_common_config_with_w_6_ms`.
    for ((dev, app, block, dir), want) in [
        ((48_000, 48_000, 128, Both), (9_000_000, 1_500, 1_166)),
        ((96_000, 96_000, 128, Both), (9_000_000, 1_500, 1_033)),
        ((96_000, 48_000, 128, Both), (8_364_583, 1_499, 1_032)),
        ((96_000, 44_100, 128, Both), (5_916_666, 1_499, 856)),
        ((48_000, 44_100, 128, Both), (5_916_666, 1_499, 1_123)),
        ((48_000, 96_000, 128, Both), (8_375_000, 1_500, 1_167)),
        ((48_000, 48_000, 256, Both), (9_000_000, 1_500, 1_433)),
        ((96_000, 48_000, 256, Both), (8_364_583, 1_499, 1_165)),
        ((96_000, 44_100, 256, Both), (5_916_666, 1_499, 989)),
        ((48_000, 44_100, 256, Both), (5_916_666, 1_499, 1_390)),
        ((48_000, 48_000, 128, Rx), (12_333_333, 2_733, 1_499)),
        ((96_000, 96_000, 128, Rx), (13_666_666, 2_866, 1_499)),
        ((96_000, 48_000, 128, Rx), (13_041_666, 2_867, 1_499)),
        ((96_000, 44_100, 128, Rx), (12_343_750, 3_042, 1_499)),
        ((48_000, 44_100, 128, Rx), (9_666_666, 2_774, 1_498)),
        ((48_000, 96_000, 128, Rx), (11_687_500, 2_731, 1_498)),
    ] {
        let s = split(Terms::default(), dev, app, block, dir).unwrap();
        let at = format!("{dev} {app} {block} {dir:?}");
        assert_eq!(
            (s.latency_ns, ms10(s.tx_path_ns), ms10(s.rx_path_ns)),
            want,
            "{at}"
        );
        assert_eq!(s.network_latency_ns, NETWORK_LATENCY_NS, "{at}");
        assert_eq!(s.receive_buffer_ns, s.latency_ns - 1_000_000, "{at}");
        let w = if dir == Rx { 15_000_000 } else { 6_000_000 };
        assert_eq!(s.margin, latency_samples(w, dev), "{at}");
        assert_eq!(s.rx_path_ns, s.latency_ns + s.rx_handoff_ns, "{at}");
        assert_eq!(s.tx_path_ns, s.latency_ns + s.tx_handoff_ns, "{at}");
        assert_eq!(s.rx_handoff_ns, s.block_ns + s.rx_convert_ns, "{at}");
        assert_eq!(s.tx_handoff_ns, s.work_margin_ns + s.tx_convert_ns, "{at}");
        assert_eq!(
            s.work_margin_ns,
            s.margin * 1_000_000_000 / u64::from(dev),
            "{at}"
        );
        if dir == Both {
            assert!(s.latency_ns >= tx_floor_ns(dev), "{at}");
        }
    }
    // Every block of the stack, every pair of the three rates.
    for dev in [44_100, 48_000, 96_000] {
        for app in [44_100, 48_000, 96_000] {
            for b in BLOCKS {
                let s = split(Terms::default(), dev, app, b, Both);
                assert!(s.is_ok(), "{dev} {app} {b}: {s:?}");
            }
        }
    }
}

/// dstack #244: the terms are independent. Setting (a) alone keeps the
/// default (b) and moves P by the difference; setting (b) alone keeps
/// (a) 1 ms; W changes neither, only the TX hand-off.
#[test]
fn the_terms_are_independent() {
    let base = Terms::default();
    let at = |t: Terms| split(t, 48_000, 48_000, 128, Both).unwrap();
    let d = at(base);
    let a2 = at(Terms {
        network_latency_ns: Some(2_000_000),
        ..base
    });
    assert_eq!(a2.network_latency_ns, 2_000_000);
    assert_eq!(a2.receive_buffer_ns, d.receive_buffer_ns);
    assert_eq!(a2.latency_ns, d.latency_ns + 1_000_000);
    let b5 = at(Terms {
        receive_buffer_ns: Some(5_000_000),
        ..base
    });
    assert_eq!(
        (b5.network_latency_ns, b5.receive_buffer_ns, b5.latency_ns),
        (1_000_000, 5_000_000, 6_000_000)
    );
    let w8 = at(Terms {
        work_margin_ns: Some(8_000_000),
        ..base
    });
    assert_eq!(w8.latency_ns, d.latency_ns);
    assert_eq!((w8.margin, w8.work_margin_ns), (384, 8_000_000));
    assert_eq!(w8.tx_handoff_ns, 8_000_000);
    assert_eq!(w8.rx_handoff_ns, d.rx_handoff_ns);
    // (a) under 1 ms with the default (b) can reach the floor.
    let a0 = at(Terms {
        network_latency_ns: Some(0),
        ..base
    });
    assert_eq!(a0.latency_ns, 8_000_000);
}

/// dstack #244: P is (a) + (b) rounded UP to whole samples, never under
/// what the terms ask; (b) is stated as P - (a), so the terms add up.
#[test]
fn p_is_a_plus_b_rounded_up_to_whole_samples() {
    let t = |a: u64, b: u64| Terms {
        network_latency_ns: Some(a),
        receive_buffer_ns: Some(b),
        work_margin_ns: None,
    };
    // Whole samples already: 4 ms at 48 kHz is 192.
    let s = split(t(1_000_000, 3_000_000), 48_000, 48_000, 128, Rx).unwrap();
    assert_eq!((s.latency_ns, s.receive_buffer_ns), (4_000_000, 3_000_000));
    // 4 ms at 44.1 kHz is 176.4 samples: P is 177 (4.013605 ms).
    let s = split(t(1_000_000, 3_000_000), 44_100, 44_100, 128, Rx).unwrap();
    assert_eq!((s.latency_ns, s.receive_buffer_ns), (4_013_605, 3_013_605));
    assert_eq!(latency_samples(s.latency_ns, 44_100), 177);
    // One ns over a whole sample takes the next one.
    let s = split(t(1_000_000, 3_000_001), 48_000, 48_000, 128, Rx).unwrap();
    assert_eq!(s.latency_ns, 4_020_833);
    // Huge terms saturate instead of wrapping.
    let s = split(t(u64::MAX, 1), 48_000, 48_000, 128, Rx).unwrap();
    assert!(s.latency_ns > 4_000_000);
}

/// dstack #1, #244: P's floor is the normal profile's 4 ms on an RX-only
/// device and a TX packet's own age with TX (4916666 ns at 48 kHz, 236
/// samples): the least sum that rounds up to it is taken, one ns less is
/// refused with the floor named.
#[test]
fn a_latency_under_its_floor_is_a_problem() {
    assert_eq!(MIN_LATENCY_NS, 4_000_000);
    let t = |b: u64| Terms {
        network_latency_ns: Some(1_000_000),
        receive_buffer_ns: Some(b),
        work_margin_ns: None,
    };
    assert_eq!(
        split(t(3_000_000), 48_000, 48_000, 128, Rx)
            .unwrap()
            .latency_ns,
        4_000_000
    );
    // 3999999 ns rounds up to 192 samples, the floor itself.
    assert_eq!(
        split(t(2_999_999), 48_000, 48_000, 128, Rx)
            .unwrap()
            .latency_ns,
        4_000_000
    );
    assert_eq!(
        split(t(2_979_166), 48_000, 48_000, 128, Rx).unwrap_err(),
        "network_latency_ns + receive_buffer_ns: 1000000 + 2979166 = 3979166 ns of Dante \
         latency; at least 4000000 (the normal profile's floor)"
    );
    for dir in [Tx, Both] {
        let s = split(t(3_895_834), 48_000, 48_000, 128, dir).unwrap();
        assert_eq!(s.latency_ns, 4_916_666, "{dir:?}");
        assert_eq!(
            split(t(3_895_833), 48_000, 48_000, 128, dir).unwrap_err(),
            "network_latency_ns + receive_buffer_ns: 1000000 + 3895833 = 4895833 ns of Dante \
             latency; at least 4916666 (a TX packet's own age, dlink::tx_floor_ns)",
            "{dir:?}"
        );
    }
}

/// dstack #1, #244: W unset is 6 ms with TX and 15 ms RX only, within the
/// block and 2 ms and the stack's largest margin; W set must lie there.
#[test]
fn the_work_margin_is_its_own_term_within_the_stack_s_bounds() {
    assert_eq!(default_work_margin(96_000, 128, Both), 576);
    assert_eq!(default_work_margin(96_000, 128, Tx), 576);
    assert_eq!(default_work_margin(48_000, 128, Both), 288);
    assert_eq!(default_work_margin(96_000, 128, Rx), 1_440);
    assert_eq!(default_work_margin(48_000, 128, Rx), 720);
    assert_eq!(default_work_margin(44_100, 128, Rx), 662);
    // The block is its least (6 ms at 44.1 kHz is 265 samples).
    assert_eq!(default_work_margin(44_100, 256, Both), 265);
    assert_eq!(default_work_margin(22_050, 256, Both), 256);
    // The stack's largest margin is its most (15 ms at 384 kHz is 5760).
    assert_eq!(default_work_margin(384_000, 128, Rx), MAX_WORK_MARGIN);
    const _: () = assert!(
        dstack::device::MAX_LATENCY_SAMPLES + MAX_WORK_MARGIN + 256
            <= dstack::media::ring::SLOTS as u64 / 2
    );
    let w = |ns: u64| Terms {
        work_margin_ns: Some(ns),
        ..Terms::default()
    };
    // Set: its own samples, at both ends of the range.
    let at = |ns: u64| split(w(ns), 96_000, 96_000, 128, Both);
    assert_eq!(at(12_000_000).unwrap().margin, 1_152);
    assert_eq!(at(2_000_000).unwrap().margin, 192);
    assert_eq!(at(42_666_667).unwrap().margin, MAX_WORK_MARGIN);
    assert_eq!(
        at(1_989_583).unwrap_err(),
        "work_margin_ns: 1989583 is 191 samples at 96000 Hz; 192 to 4096 \
         (the block and 2 ms, the stack's largest margin)"
    );
    assert!(
        at(42_677_084)
            .unwrap_err()
            .starts_with("work_margin_ns: 42677084 is 4097 samples")
    );
    // A W set leaves the default (b) and P as they were.
    assert_eq!(
        at(12_000_000).unwrap().latency_ns,
        split(Terms::default(), 96_000, 96_000, 128, Both)
            .unwrap()
            .latency_ns
    );
    // Config::device refuses it with the other problems.
    let bad = Config::new("-bad", "/tmp/x.toml")
        .tx(["Out"])
        .work_margin_ns(1_000_000)
        .device()
        .unwrap_err();
    assert_eq!(bad.len(), 2, "{bad:?}");
    assert!(bad[0].starts_with("work_margin_ns: 1000000"), "{bad:?}");
}

/// dstack #244: the start log names every term, one line each, never one
/// lump: the Dante latency with (a) and (b) and who set them, the RX
/// hand-off on a device with RX, the TX hand-off with W on one with TX.
#[test]
fn the_start_lines_name_every_term() {
    let obs = rx_only("obs", 48_000, 96_000);
    assert_eq!(
        obs.split().unwrap().lines(obs.directions(), obs.terms()),
        [
            "Dante latency P 13.041666 ms = network latency (a) 1 ms (default) + receive \
             buffer (b) 12.041666 ms (default)",
            "RX app hand-off (c) 1.958333 ms = block 1.333333 ms + converter 0.625 ms; RX \
             path P + (c) 14.999999 ms",
        ]
    );
    let tx = Config::new("tx", "/tmp/never-written.toml")
        .tx(["Out"])
        .network_latency_ns(500_000)
        .receive_buffer_ns(4_500_000)
        .work_margin_ns(8_000_000);
    assert_eq!(
        tx.split().unwrap().lines(tx.directions(), tx.terms()),
        [
            "Dante latency P 5 ms = network latency (a) 0.5 ms (set) + receive buffer (b) \
             4.5 ms (set)",
            "TX app hand-off (c) 8 ms = work margin W 8 ms (set) + converter 0 ms; TX path \
             (c) + P 13 ms",
        ]
    );
    let both = config();
    assert_eq!(
        both.split().unwrap().lines(both.directions(), both.terms()),
        [
            "Dante latency P 9 ms = network latency (a) 1 ms (default) + receive buffer (b) \
             8 ms (default)",
            "RX app hand-off (c) 2.666666 ms = block 2.666666 ms + converter 0 ms; RX path \
             P + (c) 11.666666 ms",
            "TX app hand-off (c) 6 ms = work margin W 6 ms (default) + converter 0 ms; TX \
             path (c) + P 15 ms",
        ]
    );
}

#[test]
fn ms_prints_up_to_six_decimals_without_trailing_zeros() {
    assert_eq!(ms(0), "0");
    assert_eq!(ms(1_000_000), "1");
    assert_eq!(ms(13_041_666), "13.041666");
    assert_eq!(ms(625_000), "0.625");
    assert_eq!(ms(500_000), "0.5");
    assert_eq!(ms(1), "0.000001");
    assert_eq!(ms(10_000_010), "10.00001");
}

/// dstack #1 (camera-box, 4.11.2026, v0.6.31 live): OBS dante-input, RX
/// only, was preempted about 6 ms, right at W 6 ms, and re-bootstrapped.
/// An RX-only device's W is in none of its paths, so it is 15 ms: a pump
/// 8 ms late still runs its block. A device with TX keeps W 6 ms (it is in
/// its TX path), so 8 ms late restarts it. P and the RX path are v0.6.105's.
#[test]
fn at_the_default_margin_an_rx_only_pump_8_ms_late_still_runs_its_block() {
    use dstack::stream::{Blocks, Due, Timing};
    // (config, P ns, RX path in 10 us, W samples).
    for (c, p, rx_path, w) in [
        (rx_only("obs", 48_000, 96_000), 13_041_666, 1_499, 1_440),
        (rx_only("rx-48", 48_000, 48_000), 12_333_333, 1_499, 720),
        (rx_only("rx-96", 96_000, 96_000), 13_666_666, 1_499, 1_440),
        (rx_only("rx-cd", 44_100, 96_000), 12_343_750, 1_499, 1_440),
    ] {
        let cfg = c.device().unwrap();
        let s = c.split().unwrap();
        assert_eq!(cfg.latency_ns, p, "{}", cfg.name);
        assert_eq!(s.rx_path_ns / 10_000, rx_path, "{}", cfg.name);
        let t = Timing::of(&cfg);
        assert_eq!(t.margin, w, "{}", cfg.name);
        assert_eq!(
            t.margin,
            latency_samples(RX_WORK_MARGIN_NS, cfg.sample_rate)
        );
        let late = latency_samples(8_000_000, cfg.sample_rate);
        let due = Blocks::bootstrap(0, t).due();
        for (behind, runs) in [(late, true), (t.margin, true), (t.margin + 1, false)] {
            let mut blocks = Blocks::bootstrap(0, t);
            let got = blocks.next(due + behind);
            assert_eq!(
                matches!(got, Due::Run { .. }),
                runs,
                "{} at {behind} behind: {got:?} ({t:?})",
                cfg.name
            );
        }
    }
    let both = config().sample_rate(48_000).network_rate(96_000);
    let t = dstack::stream::Timing::of(&both.device().unwrap());
    assert_eq!(t.margin, 576);
    let mut blocks = Blocks::bootstrap(0, t);
    let due = blocks.due();
    assert!(matches!(
        blocks.next(due + latency_samples(8_000_000, 96_000)),
        Due::Rebootstrap { .. }
    ));
}

/// dstack #1 (songplayer, 4.11.2026): a receiver at a P under a TX packet's
/// own age counted packets late. A device with TX gets, by default, a P
/// over that age (32 frames at 96 kHz + 0.25 ms + 4 ms, 4583333 ns), with
/// W at least its least.
#[test]
fn a_96_khz_tx_device_with_a_48_khz_app_gets_p_over_a_tx_packet_s_age() {
    let age = 32 * 1_000_000_000 / 96_000 + 250_000 + 4_000_000;
    assert_eq!(age, 4_583_333);
    assert_eq!(tx_floor_ns(96_000), age);
    for dir in [Tx, Both] {
        let s = split(Terms::default(), 96_000, 48_000, 128, dir).unwrap();
        assert!(s.latency_ns >= age, "{dir:?}: {s:?}");
        assert!(s.margin >= least_work_margin(96_000, 128), "{dir:?}: {s:?}");
    }
    let songplayer = Config::new("tx-only", "/tmp/never-written.toml")
        .tx(["Out L", "Out R"])
        .sample_rate(48_000)
        .network_rate(96_000);
    let s = songplayer.split().unwrap();
    assert_eq!(s.latency_ns, 8_364_583);
    assert_eq!(songplayer.device().unwrap().latency_ns, s.latency_ns);
}

/// dstack #241 (obs #41), in terms: the OBS device with TX and RX, a 96 kHz
/// network, a 48 kHz app, block 128, at what v0.6.105 gave its 20 ms
/// budget: W 8 ms and P 11.364583 ms ((a) 1 ms + (b) 10.364583 ms). RX path
/// P + B + the half-band's RX delay 13.322916 ms, TX path W + its TX delay
/// + P 19.999999 ms.
#[test]
fn obs_s_device_at_its_old_20_ms_numbers_in_terms() {
    let cfg = config()
        .sample_rate(48_000)
        .network_rate(96_000)
        .work_margin_ns(8_000_000)
        .receive_buffer_ns(10_364_583)
        .block(128);
    assert_eq!(cfg.directions(), Both);
    let s = cfg.split().unwrap();
    assert_eq!(s.margin, 768);
    assert_eq!(s.latency_ns, 11_364_583);
    assert_eq!(s.rx_path_ns, 13_322_916);
    assert_eq!(s.tx_path_ns, 19_999_999);
}

/// v0.6.105's `split` at its default 15 ms budget, kept verbatim (only
/// renamed) as the reference the defaults must reproduce (dstack #244):
/// `(P ns, W samples, TX path ns, RX path ns)`, or its refusal (with the W
/// it had chosen for a budget refusal). Its own copies of v0.6.105's
/// `least_work_margin`, `tx_floor_ns` and floors (with their literals), so
/// a change to the live ones cannot move the reference with them; only
/// the converter's delays (`rated::delay_ns`) are shared.
fn v0_6_105_split(
    device_rate: u32,
    app_rate: u32,
    block: u64,
    directions: Directions,
) -> Result<(u64, u64, u64, u64), (String, u64)> {
    const BUDGET_NS: u64 = 15_000_000;
    const MIN_LATENCY_NS: u64 = 4_000_000;
    let least_work_margin = |rate: u32, block: u64| {
        let b = if [16, 32, 64, 128, 256].contains(&block) {
            block
        } else {
            128
        };
        b.max(latency_samples(2_000_000, rate))
    };
    // 32 frames (FPP_MAX) + the TX margin 0.25 ms + 4 ms of lateness.
    let tx_floor_ns = |rate: u32| 32 * 1_000_000_000 / u64::from(rate.max(1)) + 250_000 + 4_000_000;
    let work_margin = |rate: u32, block: u64| {
        latency_samples(BUDGET_NS / 5 * 2, rate)
            .min(MAX_WORK_MARGIN)
            .max(least_work_margin(rate, block))
    };
    let rx_work_margin = |rate: u32, block: u64| {
        latency_samples(BUDGET_NS, rate)
            .min(MAX_WORK_MARGIN)
            .max(least_work_margin(rate, block))
    };
    if device_rate == 0 {
        return Err(("sample_rate: the device's rate is 0 Hz".to_string(), 0));
    }
    let b = if [16, 32, 64, 128, 256].contains(&block) {
        block
    } else {
        128
    };
    let (convert_rx, convert_tx) =
        rated::delay_ns(device_rate, app_rate, usize::try_from(b).unwrap())
            .map_err(|e| (format!("rate conversion: {e}"), 0))?;
    let rate = u128::from(device_rate);
    let ns = |samples: u64| (u128::from(samples) * 1_000_000_000 / rate) as u64;
    let samples_in = |ns: u64| (u128::from(ns) * rate / 1_000_000_000) as u64;
    let (margin, floor) = if directions == Directions::Rx {
        (rx_work_margin(device_rate, b), MIN_LATENCY_NS)
    } else {
        let floor = tx_floor_ns(device_rate);
        let room = BUDGET_NS.saturating_sub(convert_tx + floor);
        let margin = work_margin(device_rate, b)
            .min(samples_in(room).saturating_sub(1))
            .max(least_work_margin(device_rate, b));
        (margin, floor)
    };
    let tx_work = ns(margin) + convert_tx;
    let rx_work = ns(b) + convert_rx;
    let work = match directions {
        Directions::Both => tx_work.max(rx_work),
        Directions::Rx => rx_work,
        Directions::Tx => tx_work,
    };
    let latency_ns = ns(samples_in(BUDGET_NS.saturating_sub(work)));
    if latency_ns < floor {
        return Err((format!("budget_ns: under the floor {floor}"), margin));
    }
    Ok((
        latency_ns,
        margin,
        tx_work + latency_ns,
        rx_work + latency_ns,
    ))
}

/// dstack #244 (review): the defaults reproduce v0.6.105 exactly, P, W
/// and both paths, over every device rate of the site and the PCs, the
/// app rates around them, every block and all three directions; W gives
/// way to P's TX floor where the old budget made it. A config v0.6.105
/// refused for its budget now starts at P's floor, with the same W.
#[test]
fn the_defaults_reproduce_v0_6_105_over_the_whole_grid() {
    let mut refused_before = 0;
    for dev in [44_100, 48_000, 88_200, 96_000] {
        for app in [
            8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400,
            192_000,
        ] {
            for block in BLOCKS {
                for dir in [Both, Rx, Tx] {
                    let at = format!("{dev} {app} {block} {dir:?}");
                    let new = split(Terms::default(), dev, app, block, dir);
                    match v0_6_105_split(dev, app, block, dir) {
                        Ok((p, w, tx, rx)) => {
                            let s = new.unwrap_or_else(|e| panic!("{at}: {e}"));
                            assert_eq!(
                                (s.latency_ns, s.margin, s.tx_path_ns, s.rx_path_ns),
                                (p, w, tx, rx),
                                "{at}"
                            );
                        }
                        Err((e, w)) if e.starts_with("budget_ns") => {
                            refused_before += 1;
                            let s = new.unwrap_or_else(|e| panic!("{at}: {e}"));
                            let floor = if dir == Rx {
                                MIN_LATENCY_NS
                            } else {
                                tx_floor_ns(dev)
                            };
                            assert!(s.latency_ns >= floor, "{at}: {s:?}");
                            assert!(
                                s.latency_ns - floor < 1_000_000_000 / u64::from(dev),
                                "{at}: {s:?}"
                            );
                            // The same W as v0.6.105 chose, each path P + its
                            // hand-off.
                            assert_eq!(s.margin, w, "{at}: {s:?}");
                            assert_eq!(s.tx_path_ns, s.tx_handoff_ns + s.latency_ns, "{at}");
                            assert_eq!(s.rx_path_ns, s.rx_handoff_ns + s.latency_ns, "{at}");
                        }
                        Err((e, _)) => assert_eq!(new.unwrap_err(), e, "{at}"),
                    }
                }
            }
        }
    }
    // The grid holds such configs (8, 11.025 and 16 kHz apps, for one).
    assert!(refused_before > 0);
}

/// dstack #244 (review): an app that sets only (a), under the default 1 ms,
/// on a config whose default P sits one sample over the TX floor (a
/// 22.05 kHz app on a 96 kHz network: W gives way, P 4.59375 ms, (b)
/// 3.59375 ms) moves P down with it only to the floor: the default (b)
/// grows there, never refused for a (b) the app did not set.
#[test]
fn a_small_network_latency_alone_never_takes_the_default_p_under_its_floor() {
    let d = split(Terms::default(), 96_000, 22_050, 128, Both).unwrap();
    assert_eq!((d.latency_ns, d.receive_buffer_ns), (4_593_750, 3_593_750));
    for (a, p) in [
        (0, 4_583_333),
        (250_000, 4_583_333),
        (989_583, 4_583_333),
        (999_999, 4_593_750),
    ] {
        let t = Terms {
            network_latency_ns: Some(a),
            ..Terms::default()
        };
        let s = split(t, 96_000, 22_050, 128, Both).unwrap();
        assert_eq!(s.latency_ns, p, "{a}");
        assert_eq!(s.network_latency_ns + s.receive_buffer_ns, s.latency_ns);
    }
    // Over 1 ms it moves P by the difference.
    let t = Terms {
        network_latency_ns: Some(2_000_000),
        ..Terms::default()
    };
    assert_eq!(
        split(t, 96_000, 22_050, 128, Both).unwrap().latency_ns,
        5_593_750
    );
}

/// dstack #244 (review): a (b) the app set off a whole sample is logged as
/// set and rounded, so the line never hides the rounding.
#[test]
fn a_receive_buffer_off_a_whole_sample_is_logged_as_rounded_up() {
    let c = Config::new("rx", "/tmp/never-written.toml")
        .rx(["In"])
        .receive_buffer_ns(3_000_001);
    assert_eq!(
        c.split().unwrap().lines(c.directions(), c.terms())[0],
        "Dante latency P 4.020833 ms = network latency (a) 1 ms (default) + receive buffer \
         (b) 3.020833 ms (set 3.000001 ms, rounded up to whole samples)"
    );
    // On a whole sample it reads "set".
    let whole = c.clone().receive_buffer_ns(3_000_000);
    assert!(
        whole
            .split()
            .unwrap()
            .lines(whole.directions(), whole.terms())[0]
            .ends_with("(b) 3 ms (set)")
    );
}
