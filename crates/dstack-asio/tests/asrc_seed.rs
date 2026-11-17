// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #382 (dasio#120), whole bridge on simulated time: a run's locked
//! ASRC rates, kept in the seed file, start the next run, which then holds
//! its rate from the first second instead of slewing from 0 until its fit
//! locks at 60 s. Its own binary: two sinc players at 96 kHz for 150
//! simulated seconds (about 10 s unoptimised), too long for the mutation
//! gate's per-test limit.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use dstack_asio::bridge::{Bridge, Seed, Spec};
use dstack_asio::{Block, Opened, Process, Sample};

const RATE: u32 = 96_000;
/// The driver's buffer and the pump's block, frames (dsg's shape).
const FRAMES: usize = 128;
/// The driver's clock against Dante's, ppm (dsg's measured rate).
const DRIFT: f64 = 7.5;

fn opened() -> Opened {
    Opened {
        rate: f64::from(RATE),
        buffer_frames: FRAMES as u32,
        driver_inputs: 1,
        driver_outputs: 1,
        inputs: vec![0],
        outputs: vec![0],
        input_sample: Some(Sample::Int32),
        output_sample: Sample::Int32,
    }
}

/// The bridge after `seconds`, the to-ASIO player's correction at its
/// start (before its first window closes) and the largest |from-ASIO
/// correction - `around`| over the run's first 60 s, read after every
/// driver switch: the pump's blocks on Dante's clock, the driver's
/// switches `DRIFT` ppm fast, both ways, every block naming its start (the
/// players steer on the producer's line, as in the field) and every FIFO on
/// the simulated time. (The pump-written to-ASIO side brakes an offset of
/// its own start in the first 20 s, seeded or not; the field's to-ASIO
/// side is pulled. Its first-minute rate is judged on the from-ASIO side.)
fn run(seed: Option<Seed>, seconds: f64, around: f64) -> (Bridge, (f64, f64)) {
    let now = Arc::new(AtomicI64::new(0));
    let clock = Arc::clone(&now);
    let mut bridge = Bridge::with_clock(
        Spec {
            dante_rate: RATE,
            block_frames: FRAMES as u32,
            to_asio: 1,
            from_asio: 1,
            app_buffer_ns: None,
            to_asio_app_buffer_ns: None,
            from_asio_app_buffer_ns: None,
        },
        Arc::new(move || clock.load(Ordering::Relaxed)),
    )
    .unwrap();
    if let Some(seed) = seed {
        bridge.seed_from(seed);
    }
    let mut dante = bridge.dante_side().unwrap();
    let mut asio = bridge.asio_side(&opened()).unwrap();
    let (to, from) = (bridge.to_asio_stats(), bridge.from_asio_stats());
    let net = f64::from(RATE);
    let card = net * (1.0 + DRIFT * 1e-6);
    let (mut dante_frames, mut asio_frames) = (0u64, 0u64);
    let input = vec![0.25; FRAMES];
    let mut output = vec![0.0; FRAMES];
    let mut worst: (f64, f64) = (0.0, 0.0);
    loop {
        let t_dante = dante_frames as f64 / net;
        let t_asio = asio_frames as f64 / card;
        let t = t_dante.min(t_asio);
        if t >= seconds {
            break;
        }
        now.store((t * 1e9) as i64, Ordering::Relaxed);
        if t_dante <= t_asio {
            dante.process(&mut Block::new(FRAMES, &input, &mut output).at(dante_frames));
            dante_frames += FRAMES as u64;
        } else {
            asio.process(&mut Block::new(FRAMES, &input, &mut output).at(asio_frames));
            asio_frames += FRAMES as u64;
            if t < 0.5 {
                // Before the first window closes: the start.
                worst.0 = to.ppm();
            }
            if t < 60.0 {
                worst.1 = worst.1.max((from.ppm() - around).abs());
            }
        }
    }
    assert_eq!((to.underruns(), from.underruns()), (0, 0));
    (bridge, worst)
}

#[test]
fn a_run_seeded_from_the_last_one_holds_its_rate_from_the_start() {
    // The first run: no seed, its fits lock after 60 s.
    let (first, (to_start, slewed)) = run(None, 90.0, -DRIFT);
    let seed = first.seed().expect("a seed after the run");
    let (to, from) = (seed.to_asio_ppm.unwrap(), seed.from_asio_ppm.unwrap());
    println!(
        "first run: locked {to:.3} / {from:.3} ppm; to-ASIO starts at {to_start} ppm, \
         from-ASIO first minute off by {slewed:.3} ppm"
    );
    // To ASIO the driver's clock reads Dante's audio (+d); from ASIO Dante
    // takes the driver's (-d).
    assert!((to - DRIFT).abs() < 0.5, "{to}");
    assert!((from + DRIFT).abs() < 0.5, "{from}");
    assert_eq!(to_start, 0.0, "unseeded: from 0");
    assert!(slewed > 6.0, "unseeded: about the drift short: {slewed}");
    // Through the file, as the app keeps it.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.seed");
    seed.write(&path).unwrap();
    let kept = Seed::read(&path).unwrap().expect("the file");
    assert_eq!(kept, seed);
    // The next run starts at the kept rates and stays there.
    let (next, (to_start, held)) = run(Some(kept), 70.0, from);
    println!(
        "seeded run: to-ASIO starts at {to_start} ppm, from-ASIO first minute off by \
         {held:.3} ppm"
    );
    assert_eq!(to_start, to, "seeded: from the kept rate");
    assert!(held < 0.5, "seeded: {held}");
    let again = next.seed().unwrap();
    assert!((again.to_asio_ppm.unwrap() - to).abs() < 0.5, "{again:?}");
    assert!(
        (again.from_asio_ppm.unwrap() - from).abs() < 0.5,
        "{again:?}"
    );
}
