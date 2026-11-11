// SPDX-License-Identifier: MIT OR Apache-2.0

//! The capture side (dstack #164): the loopback callback hands an
//! interleaved packet of the endpoint's channels; it goes into the FIFO
//! frame by frame. It allocates nothing and never waits (the driver's
//! thread); a full FIFO drops the packet's tail and counts it.
//!
//! A busy desktop must not starve the capture (dstack #198: on the FOH PC
//! dsend ran at priority class BelowNormal while a browser started; the
//! capture thread missed its loopback reads, WASAPI flagged a data
//! discontinuity and the FIFO ran dry, 0.5 s dropouts). So:
//!
//! - The capture thread (cpal's) joins MMCSS "Pro Audio" from its first
//!   callback ([`ThreadJoin`]), as the dlink device's own media threads
//!   (the pump among them) do at their start, whatever the process's
//!   class. Its log line is [`mmcss_line`].
//! - The loopback buffer holds [`CAPTURE_BUFFER_MS`]
//!   ([`capture_buffer_frames`]): a stall of [`STALL_MS`] loses nothing
//!   at WASAPI, the backlog comes in the next callbacks.
//! - A discontinuity WASAPI still flags is measured on the packets'
//!   capture times ([`GapWatch`]) and logged in ms ([`Gap::line`]).

use dstack_endpoint::fifo::Fifo;

/// The capture thread's stall that loses nothing at WASAPI, ms (dstack
/// #198): the loopback buffer keeps that much audio while the thread is
/// off the CPU.
pub const STALL_MS: u32 = 200;
/// The loopback buffer's length, ms: [`STALL_MS`] and five of WASAPI's
/// default 10 ms periods (the packet pending when the stall began and the
/// engine's own scheduling).
pub const CAPTURE_BUFFER_MS: u32 = STALL_MS + 50;

/// The loopback buffer at the endpoint's mix `rate`, frames:
/// [`CAPTURE_BUFFER_MS`] rounded up (cpal's `BufferSize::Fixed`, the
/// shared-mode `Initialize`'s buffer duration; the callback period stays
/// the engine's).
pub fn capture_buffer_frames(rate: u32) -> u32 {
    // 0.6.86 asked no size (cpal's BufferSize::Default: the engine's least).
    let _ = rate;
    0
}

/// The capture thread's one-time setup (dstack #198): cpal makes the
/// thread, so dsend's data callback joins MMCSS on its first call, never
/// on the ones after. Each stream (a reopen too) has its own thread and
/// its own `ThreadJoin`.
#[derive(Debug, Default)]
pub struct ThreadJoin {
    joined: bool,
}

impl ThreadJoin {
    /// On every callback: runs `join` on the first call only and returns
    /// its result then (for the log); `None` on the calls after.
    pub fn first<T>(&mut self, join: impl FnOnce() -> T) -> Option<T> {
        // 0.6.86: the capture thread joined nothing.
        let _ = (&mut self.joined, join);
        None
    }
}

/// The log line of the capture thread's MMCSS join: MMCSS's task index,
/// or the OS error (the thread then runs at its process's priority).
pub fn mmcss_line(joined: &std::io::Result<u32>) -> String {
    let _ = joined;
    String::new()
}

/// A loopback discontinuity measured (dstack #198): between the last good
/// packet and this one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gap {
    /// The endpoint's mix rate, Hz.
    pub rate: u32,
    /// The time from the last good callback to this one, ms (the thread's
    /// own clock: how long the capture was away).
    pub since_callback_ms: f64,
    /// The frames the capture times say came between the two packets'
    /// starts.
    pub expected: u64,
    /// The frames the last good packet held.
    pub got: u64,
}

impl Gap {
    /// The frames lost: expected but never delivered.
    pub fn missing(&self) -> u64 {
        0
    }

    /// The audio lost, ms.
    pub fn missing_ms(&self) -> f64 {
        0.0
    }

    /// The log line.
    pub fn line(&self) -> String {
        String::new()
    }
}

/// The last good packet: its capture time, frames and callback time.
#[derive(Debug, Clone, Copy)]
struct Packet {
    capture_ns: u64,
    frames: u64,
    wall_ns: u64,
}

/// Measures the loopback's discontinuities from the packets' capture
/// times (WASAPI's QPC position of each packet's first frame): the
/// capture callback hands every packet, with whether WASAPI flagged a
/// discontinuity before it (the error callback, on the same thread).
#[derive(Debug)]
pub struct GapWatch {
    rate: u32,
    last: Option<Packet>,
}

impl GapWatch {
    /// A watch for an endpoint at mix `rate`.
    pub fn new(rate: u32) -> Self {
        GapWatch { rate, last: None }
    }

    /// A packet of `frames` captured at `capture_ns`, handed at `wall_ns`
    /// (both on their own monotonic clocks), after a discontinuity when
    /// `discontinuity`: the gap since the last packet, when there was
    /// one; `None` for a packet with no flag and for the stream's first.
    pub fn packet(
        &mut self,
        capture_ns: u64,
        frames: usize,
        wall_ns: u64,
        discontinuity: bool,
    ) -> Option<Gap> {
        // 0.6.86 measured nothing.
        let _ = (capture_ns, frames, wall_ns, discontinuity, self.rate);
        let _ = self.last.map(|p| (p.capture_ns, p.frames, p.wall_ns));
        None
    }
}

/// Writes the interleaved `data` of `channels` channels into `fifo`, each
/// sample made an `f64` by `to` (the driver's sample format). A FIFO
/// channel the packet lacks reads as silence; a partial last frame is
/// left out. Returns the frames written.
pub fn push_interleaved<T: Copy>(
    fifo: &Fifo,
    data: &[T],
    channels: usize,
    to: impl Fn(T) -> f64,
) -> usize {
    let Some(frames) = data.len().checked_div(channels) else {
        return 0;
    };
    fifo.push_with(frames, |f, c| {
        if c < channels {
            to(data[f * channels + c])
        } else {
            0.0
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(fifo: &Fifo) -> Vec<f64> {
        let n = fifo.len();
        let mut out = vec![0.0; n * fifo.channels()];
        assert!(fifo.pop_planar(&mut out, n));
        out
    }

    #[test]
    fn an_interleaved_packet_goes_in_frame_by_frame() {
        let fifo = Fifo::new(2, 16);
        // L = 0.1 0.2 0.3, R = -0.1 -0.2 -0.3, interleaved.
        let data = [0.1f32, -0.1, 0.2, -0.2, 0.3, -0.3];
        assert_eq!(push_interleaved(&fifo, &data, 2, f64::from), 3);
        let want = [0.1f32, 0.2, 0.3, -0.1, -0.2, -0.3].map(f64::from);
        assert_eq!(all(&fifo), want);
    }

    #[test]
    fn the_sample_format_is_converted_by_the_caller_s_function() {
        let fifo = Fifo::new(1, 8);
        let data = [16_384i16, -32_768];
        push_interleaved(&fifo, &data, 1, |x| f64::from(x) / 32_768.0);
        assert_eq!(all(&fifo), [0.5, -1.0]);
    }

    #[test]
    fn a_fifo_wider_than_the_packet_fills_silence_and_a_partial_frame_is_dropped() {
        let fifo = Fifo::new(2, 8);
        assert_eq!(
            push_interleaved(&fifo, &[0.5f32, 0.25, 0.75], 1, f64::from),
            3
        );
        assert_eq!(all(&fifo), [0.5, 0.25, 0.75, 0.0, 0.0, 0.0]);
        // 5 samples of 2 channels: 2 whole frames.
        let wide = Fifo::new(2, 8);
        assert_eq!(push_interleaved(&wide, &[1.0f32; 5], 2, f64::from), 2);
        assert_eq!(wide.arrived(), 2);
    }

    #[test]
    fn zero_channels_write_nothing() {
        let fifo = Fifo::new(2, 8);
        assert_eq!(push_interleaved(&fifo, &[1.0f32; 4], 0, f64::from), 0);
        assert_eq!(fifo.arrived(), 0);
    }

    /// dstack #198: 250 ms of the mix rate, rounded up, so a 200 ms stall
    /// loses nothing.
    #[test]
    fn the_loopback_buffer_holds_a_200_ms_stall() {
        assert_eq!(CAPTURE_BUFFER_MS, 250);
        assert_eq!(capture_buffer_frames(48_000), 12_000);
        assert_eq!(capture_buffer_frames(44_100), 11_025);
        assert_eq!(capture_buffer_frames(96_000), 24_000);
        // 1001 Hz: 250.25 frames, rounded up.
        assert_eq!(capture_buffer_frames(1_001), 251);
        // 1003 Hz: 250.75 frames, rounded up (not to the nearest).
        assert_eq!(capture_buffer_frames(1_003), 251);
        assert_eq!(capture_buffer_frames(4), 1);
        for rate in [44_100, 48_000, 88_200, 96_000, 192_000] {
            let frames = u64::from(capture_buffer_frames(rate));
            assert!(frames * 1000 >= u64::from(STALL_MS) * u64::from(rate));
        }
    }

    /// dstack #198: the capture thread joins MMCSS on the first callback
    /// and never again (three calls, one join).
    #[test]
    fn the_capture_thread_joins_once_on_its_first_callback() {
        let mut join = ThreadJoin::default();
        let mut calls = 0;
        let mut joins = Vec::new();
        for _ in 0..3 {
            joins.push(join.first(|| {
                calls += 1;
                7u32
            }));
        }
        assert_eq!(calls, 1);
        assert_eq!(joins, [Some(7), None, None]);
        // Another stream's thread joins again.
        let mut other = ThreadJoin::default();
        assert_eq!(other.first(|| 3u32), Some(3));
    }

    #[test]
    fn the_mmcss_line_names_the_task_index_or_the_error() {
        assert_eq!(
            mmcss_line(&Ok(2)),
            "the loopback capture thread runs in MMCSS Pro Audio (task index 2)"
        );
        let refused = std::io::Error::other("the service is stopped");
        assert_eq!(
            mmcss_line(&Err(refused)),
            "MMCSS refused the loopback capture thread (the service is stopped): \
             it runs at its process's priority"
        );
    }

    /// One ms in ns (the tests' capture and callback times).
    const MS: u64 = 1_000_000;

    #[test]
    fn a_packet_without_a_discontinuity_measures_nothing() {
        let mut w = GapWatch::new(48_000);
        assert_eq!(w.packet(0, 480, 0, false), None);
        assert_eq!(w.packet(10 * MS, 480, 10 * MS, false), None);
        // The stream's first packet, flagged: nothing to measure against.
        let mut fresh = GapWatch::new(48_000);
        assert_eq!(fresh.packet(5 * MS, 480, 0, true), None);
        assert_eq!(fresh.packet(15 * MS, 480, 10 * MS, false), None);
    }

    /// dstack #198: a stall of the capture thread; the packet after it
    /// starts 250 ms after the last one, which held 10 ms (48 kHz).
    #[test]
    fn a_discontinuity_is_measured_on_the_capture_times() {
        let mut w = GapWatch::new(48_000);
        assert_eq!(w.packet(1_000 * MS, 480, 3 * MS, false), None);
        let gap = w.packet(1_250 * MS, 480, 263 * MS, true).unwrap();
        assert_eq!(
            gap,
            Gap {
                rate: 48_000,
                since_callback_ms: 260.0,
                expected: 12_000,
                got: 480,
            }
        );
        assert_eq!(gap.missing(), 11_520);
        assert!((gap.missing_ms() - 240.0).abs() < 1e-9);
        assert_eq!(
            gap.line(),
            "a loopback discontinuity: 260.0 ms since the last good callback, \
             12000 frames expected since the last packet, 480 got: 240.0 ms lost"
        );
        // The next measures from the packet just handed (480 frames).
        let next = w.packet(1_300 * MS, 960, 300 * MS, true).unwrap();
        assert_eq!(next.expected, 2_400);
        assert_eq!(next.got, 480);
        assert!((next.since_callback_ms - 37.0).abs() < 1e-9);
        // An unflagged packet moves the reference on too.
        assert_eq!(w.packet(1_320 * MS, 480, 320 * MS, false), None);
        let after = w.packet(1_330 * MS, 480, 330 * MS, true).unwrap();
        assert_eq!((after.expected, after.got), (480, 480));
        assert_eq!(after.missing(), 0);
    }

    /// The expected frames round to the nearest (a capture time is in
    /// 100 ns steps); times that go backwards measure no loss.
    #[test]
    fn a_gap_rounds_and_never_goes_negative() {
        let mut w = GapWatch::new(44_100);
        w.packet(0, 441, 0, false);
        // 10.011 ms at 44.1 kHz: 441.49 frames.
        let g = w.packet(10_011_000, 441, 10 * MS, true).unwrap();
        assert_eq!(g.expected, 441);
        // 10.012 ms: 441.53 frames.
        let mut w = GapWatch::new(44_100);
        w.packet(0, 441, 0, false);
        let g = w.packet(10_012_000, 441, 10 * MS, true).unwrap();
        assert_eq!(g.expected, 442);
        assert_eq!(g.missing(), 1);
        assert!((g.missing_ms() - 1000.0 / 44_100.0).abs() < 1e-9);
        // Backwards (a reset stream position): nothing expected, none lost.
        let mut w = GapWatch::new(48_000);
        w.packet(50 * MS, 480, 50 * MS, false);
        let g = w.packet(40 * MS, 480, 40 * MS, true).unwrap();
        assert_eq!((g.expected, g.missing(), g.since_callback_ms), (0, 0, 0.0));
    }
}
