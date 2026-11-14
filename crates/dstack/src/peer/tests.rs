// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

use std::path::PathBuf;

fn peer_args() -> PeerArgs {
    match args::parse(
        &"--ip 10.0.0.2 --name ci-peer --tone-hz 440,1000 --rx-from obs-test:01 \
          --test-clock-ppm 6.5 --ports 4441,8801,4456 --process-id 1 --status /tmp/p.json"
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>(),
    ) {
        Ok(Parsed::Run(a)) => *a,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_device_config_carries_the_args() {
    let mut a = peer_args();
    a.rx_from = Some(("obs-test".into(), vec!["Out 1".into(), "Out 2".into()]));
    let cfg = device_config(&a).unwrap();
    assert_eq!(cfg.name, "ci-peer");
    assert_eq!(
        (cfg.manufacturer.as_str(), cfg.model.as_str()),
        ("dstack", "dstack-peer")
    );
    assert_eq!(cfg.tx, ["TX 1", "TX 2"]);
    assert_eq!(cfg.rx, ["RX 1", "RX 2"]);
    assert_eq!(
        (cfg.sample_rate, cfg.encoding, cfg.latency_ns),
        (48_000, 24, 10_000_000)
    );
    assert_eq!((cfg.block, cfg.work_margin), (128, 288));
    assert_eq!(cfg.timing, crate::device::TimingProfile::Normal);
    assert_eq!(
        (cfg.arc_port, cfg.cmc_port, cfg.flows_port, cfg.process_id),
        (4441, 8801, 4456, 1)
    );
    assert_eq!(cfg.test_clock_ppm, Some(TestClockPpm(6.5)));
    assert_eq!(cfg.state_file, PathBuf::from("/tmp/p.json.state.toml"));
    let subs: Vec<(usize, &str, &str)> = cfg
        .subscribe
        .iter()
        .map(|d| (d.rx, d.tx_channel.as_str(), d.tx_device.as_str()))
        .collect();
    assert_eq!(subs, [(1, "Out 1", "obs-test"), (2, "Out 2", "obs-test")]);
}

#[test]
fn a_plain_peer_has_default_ports_no_skew_and_no_rx() {
    let mut a = peer_args();
    a.ports = None;
    a.test_clock_ppm = None;
    a.rx_from = None;
    a.rate = 96_000;
    let cfg = device_config(&a).unwrap();
    assert_eq!((cfg.arc_port, cfg.test_clock_ppm), (4440, None));
    assert!(cfg.rx.is_empty() && cfg.subscribe.is_empty());
    assert_eq!(cfg.work_margin, 576, "6 ms at 96 kHz");
    a.name = "bad name".into();
    assert!(device_config(&a).unwrap_err().contains("name"));
}

#[test]
fn the_play_times_give_the_media_clock_s_rate() {
    // 48 000 samples (1 s of media time) over 999 994 000 ns of play time:
    // the media clock 6 ppm fast.
    let ppm = play_at_ppm((0, 0), (48_000, 999_994_000), 48_000).unwrap();
    assert!((ppm - 6.000036).abs() < 1e-6, "{ppm}");
    let ppm = play_at_ppm((96_000, 5), (192_000, 1_000_000_005), 96_000).unwrap();
    assert!(ppm.abs() < 1e-9, "{ppm}");
    assert_eq!(play_at_ppm((0, 7), (128, 7), 48_000), None);
    assert_eq!(play_at_ppm((0, 7), (128, 6), 48_000), None);
}

/// The processor on fake blocks: the TX tone on the media grid, the RX
/// check, the play-time rate.
#[test]
fn the_processor_sends_its_tones_and_checks_what_it_hears() {
    let mut p = Tones::new(vec![1000], 1, 48_000);
    let shared = p.shared();
    let frames = 128;
    let mut out = vec![0.0; frames];
    for b in 0..100u64 {
        let start = 1_000_000 + b * frames as u64;
        let input: Vec<f64> = (0..frames as u64)
            .map(|i| tone::tone_at(start + i, 440, 48_000))
            .collect();
        // The play time runs 10 ppm slow against the media time.
        let at = (start as f64 * 1e9 / 48_000.0 / 1.00001) as i64;
        let mut block = Block::new(frames, &input, &mut out).at(start).played_at(at);
        p.process(&mut block);
        for (i, y) in out.iter().enumerate() {
            assert_eq!(*y, tone::tone_at(start + i as u64, 1000, 48_000));
        }
    }
    let rx = shared.rx[0].view();
    assert_eq!((rx.gaps, rx.samples, rx.locked), (0, 12_800, true));
    assert!((rx.hz - 440.0).abs() < 1e-6, "{rx:?}");
    assert_eq!(shared.blocks.load(Ordering::Relaxed), 100);
    let ppm = f64::from_bits(shared.play_at_ppm.load(Ordering::Relaxed));
    assert!((ppm - 10.0).abs() < 0.01, "{ppm}");
    // A free block (no media start): the RX still checked, no tone sent.
    let input = vec![0.0; frames];
    out.fill(0.0);
    let mut block = Block::new(frames, &input, &mut out);
    p.process(&mut block);
    assert!(out.iter().all(|&y| y == 0.0));
    assert_eq!(shared.blocks.load(Ordering::Relaxed), 100);
    assert_eq!(shared.rx[0].view().samples, 12_928);
}

#[test]
fn the_status_holds_the_peer_and_the_device() {
    let a = peer_args();
    let p = Tones::new(a.tones.clone(), 1, a.rate);
    let shared = p.shared();
    let s = status_json(&a, &shared, "{\"name\":\"ci-peer\"}");
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["device"]["name"], "ci-peer");
    let peer = &v["peer"];
    assert_eq!(peer["name"], "ci-peer");
    assert_eq!(peer["version"], crate::daemon::VERSION);
    assert_eq!(peer["test_clock_ppm"], 6.5);
    assert_eq!(peer["tx"][1]["channel"], "TX 2");
    assert_eq!(peer["tx"][1]["tone_hz"], 1000);
    assert_eq!(peer["rx"][0]["channel"], "RX 1");
    assert_eq!(peer["rx"][0]["from"], "obs-test:01");
    assert_eq!(peer["rx"][0]["locked"], false);
    assert!(peer["rx"][0]["tone_hz"].is_null() && peer["rx"][0]["level_dbfs"].is_null());
    assert_eq!(peer["rx"][0]["tone_gaps"], 0);
    assert!(peer["play_at_ppm"].is_null());
    // Locked: the frequency and level; the play-time rate once known.
    let mut check = ToneCheck::default();
    let x: Vec<f64> = (0..1280).map(|i| tone::tone_at(i, 1000, 48_000)).collect();
    for b in x.chunks(128) {
        check.block(b);
    }
    check.publish(&shared.rx[0], 1280, 48_000);
    shared
        .play_at_ppm
        .store(6.0f64.to_bits(), Ordering::Relaxed);
    let v: serde_json::Value = serde_json::from_str(&status_json(&a, &shared, "not json")).unwrap();
    assert_eq!(v["device"], "not json");
    let rx = &v["peer"]["rx"][0];
    assert_eq!(
        (rx["locked"].as_bool(), rx["samples"].as_u64()),
        (Some(true), Some(1280))
    );
    assert!((rx["tone_hz"].as_f64().unwrap() - 1000.0).abs() < 1e-6);
    assert!((rx["level_dbfs"].as_f64().unwrap() + 6.0206).abs() < 1e-3);
    assert_eq!(v["peer"]["play_at_ppm"], 6.0);
}

#[test]
fn the_status_file_is_written_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    write_status(&path, "{\"a\":1}").unwrap();
    write_status(&path, "{\"a\":2}").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":2}");
    assert!(!dir.path().join("s.json.tmp").exists());
    assert!(write_status(&dir.path().join("none/s.json"), "x").is_err());
}

/// A start off the last block's end (a pump restart) starts the play-time
/// rate afresh: the jump is not read as a rate.
#[test]
fn a_start_jump_starts_the_play_time_rate_afresh() {
    let mut p = Tones::new(vec![1000], 0, 48_000);
    let shared = p.shared();
    let frames = 128u64;
    let input: Vec<f64> = Vec::new();
    let mut out = vec![0.0; frames as usize];
    let mut run = |p: &mut Tones, from: u64, at0: i64, blocks: u64| {
        for b in 0..blocks {
            let start = from + b * frames;
            let at = at0 + ((b * frames) as f64 * 1e9 / 48_000.0 / 1.00001) as i64;
            let mut block = Block::new(frames as usize, &input, &mut out)
                .at(start)
                .played_at(at);
            p.process(&mut block);
        }
    };
    run(&mut p, 0, 0, 50);
    // One second of media time skipped, the play times going on.
    run(&mut p, 50 * frames + 48_000, 134_000_000, 50);
    let ppm = f64::from_bits(shared.play_at_ppm.load(Ordering::Relaxed));
    assert!((ppm - 10.0).abs() < 0.1, "{ppm}");
}
