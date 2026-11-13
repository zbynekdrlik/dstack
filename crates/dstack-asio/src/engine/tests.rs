// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::atomic::AtomicUsize;

use super::*;

/// Each output channel `o` is input `o` times (o + 2), or (o + 2) / 8 with
/// no input; counts its blocks and its discontinuities.
struct Gain {
    blocks: Arc<AtomicUsize>,
    restarts: Arc<AtomicUsize>,
    panic_at: Option<usize>,
}

impl Process for Gain {
    fn process(&mut self, block: &mut Block<'_>) {
        let n = self.blocks.fetch_add(1, Ordering::SeqCst);
        if self.panic_at == Some(n) {
            panic!("the app broke");
        }
        for o in 0..block.outputs() {
            let k = (o + 2) as f64;
            for f in 0..block.frames() {
                let x = block.input(o).get(f).map_or(k / 8.0, |x| x * k);
                block.output(o)[f] = x;
            }
        }
    }

    fn discontinuity(&mut self) {
        self.restarts.fetch_add(1, Ordering::SeqCst);
    }
}

fn opened(frames: u32, outputs: u32) -> Opened {
    Opened {
        rate: 48_000.0,
        buffer_frames: frames,
        driver_inputs: 4,
        driver_outputs: outputs,
        input_sample: Some(Sample::Int16),
        output_sample: Some(Sample::Int16),
    }
}

fn gain(panic_at: Option<usize>) -> (Box<Gain>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let blocks = Arc::new(AtomicUsize::new(0));
    let restarts = Arc::new(AtomicUsize::new(0));
    let g = Gain {
        blocks: Arc::clone(&blocks),
        restarts: Arc::clone(&restarts),
        panic_at,
    };
    (Box::new(g), blocks, restarts)
}

fn i16s(v: &[u8]) -> Vec<i16> {
    v.as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect()
}

#[test]
fn inputs_go_through_the_app_to_the_mapped_outputs_and_the_rest_is_silent() {
    // Driver inputs 2 and 0 are the app's 0 and 1; its outputs 0 and 1 go
    // to driver outputs 3 and 1.
    let channels = Channels {
        inputs: vec![2, 0],
        outputs: vec![3, 1],
    };
    let (p, blocks, restarts) = gain(None);
    let mut e = Engine::new(p, &opened(3, 4), &channels);
    assert_eq!((e.frames(), e.inputs(), e.driver_outputs()), (3, 2, 4));
    let a: Vec<u8> = [1000i16, 2000, 3000]
        .iter()
        .flat_map(|x| x.to_le_bytes())
        .collect();
    let b: Vec<u8> = [-100i16, 0, 100]
        .iter()
        .flat_map(|x| x.to_le_bytes())
        .collect();
    e.decode_input(0, &a);
    e.decode_input(1, &b);
    e.decode_input(2, &a); // no such input: ignored
    e.run();
    let mut out = vec![vec![0xAAu8; 6]; 4];
    for (ch, buf) in out.iter_mut().enumerate() {
        e.encode_output(ch, buf);
    }
    assert_eq!(
        i16s(&out[3]),
        vec![2000, 4000, 6000],
        "app output 0 = input 0 x 2"
    );
    assert_eq!(
        i16s(&out[1]),
        vec![-300, 0, 300],
        "app output 1 = input 1 x 3"
    );
    assert_eq!(i16s(&out[0]), vec![0, 0, 0], "an unmapped output is silent");
    assert_eq!(i16s(&out[2]), vec![0, 0, 0]);
    let mut none = vec![0xAAu8; 6];
    e.encode_output(9, &mut none);
    assert_eq!(none, vec![0; 6], "a channel past the driver's is silent");
    assert_eq!(
        (
            blocks.load(Ordering::SeqCst),
            restarts.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
    // A missing input buffer is silence; the next block is no restart.
    e.decode_input(0, &[]);
    e.run();
    e.encode_output(3, &mut out[3]);
    assert_eq!(i16s(&out[3]), vec![0, 0, 0]);
    assert_eq!(
        (
            blocks.load(Ordering::SeqCst),
            restarts.load(Ordering::SeqCst)
        ),
        (2, 1)
    );
    let c = e.counters();
    assert_eq!((c.callbacks(), c.consumed(), c.faults()), (2, 6, 0));
}

#[test]
fn a_driver_with_no_outputs_or_no_inputs_writes_silence_and_reads_nothing() {
    let mut o = opened(2, 2);
    o.output_sample = None;
    o.input_sample = None;
    let channels = Channels {
        inputs: vec![0],
        outputs: vec![0],
    };
    let (p, _, _) = gain(None);
    let mut e = Engine::new(p, &o, &channels);
    e.decode_input(0, &[0xFF; 4]);
    e.run();
    let mut buf = vec![0xAAu8; 4];
    e.encode_output(0, &mut buf);
    assert_eq!(buf, vec![0; 4]);
}

#[test]
fn a_panicking_app_plays_silence_from_then_on_and_is_counted() {
    let channels = Channels {
        inputs: vec![],
        outputs: vec![0],
    };
    let (p, blocks, _) = gain(Some(1));
    let mut e = Engine::new(p, &opened(2, 1), &channels);
    let mut buf = vec![0u8; 4];
    e.run();
    e.encode_output(0, &mut buf);
    assert_eq!(i16s(&buf), vec![8192, 8192], "2 / 8 with no input");
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    e.run();
    std::panic::set_hook(hook);
    e.encode_output(0, &mut buf);
    assert_eq!(i16s(&buf), vec![0, 0], "the panicking block is silence");
    e.run();
    e.encode_output(0, &mut buf);
    assert_eq!(i16s(&buf), vec![0, 0], "and every later one");
    assert_eq!(blocks.load(Ordering::SeqCst), 2, "never called again");
    let c = e.counters();
    assert_eq!((c.callbacks(), c.faults()), (3, 2));
}

#[test]
fn each_callback_is_timed_against_the_period() {
    // 48 frames at 48 kHz: a period of 1 ms.
    let (p, _, _) = gain(None);
    let e = Engine::new(p, &opened(48, 1), &Channels::default());
    let c = e.counters();
    e.note_callback(10_000_000);
    assert_eq!(c.take_gap_max_ns(), 0, "the first switch has no gap");
    e.note_callback(11_000_000);
    e.note_callback(13_000_001); // 2 ms + 1 ns: late
    e.note_callback(13_249_999); // under a quarter: fast
    e.note_callback(13_499_999); // exactly a quarter: normal
    e.note_callback(15_499_999); // exactly two periods: normal
    assert_eq!((c.late(), c.fast()), (1, 1));
    assert_eq!(c.take_gap_max_ns(), 2_000_001);
    assert_eq!(c.take_gap_max_ns(), 0, "a new interval");
    e.note_callback(15_999_999);
    assert_eq!(c.take_gap_max_ns(), 500_000);
}
