// SPDX-License-Identifier: MIT OR Apache-2.0

//! The traffic recorder (dstack #195): the last [`SECONDS`] seconds of the
//! Dante NIC's traffic and the CPU, dumped once when the NIC refuses our
//! sends. On 15.11.2026 the FOH PC's NIC refused every TX slot's sends for
//! about 100 ms (WSAENOBUFS) during a service, and nothing recorded what
//! filled its send path at that second: the [`super::TrafficWatch`] looks
//! once a minute.
//!
//! The control tick feeds [`Recorder::record`] once a second (the NIC's
//! sent-bytes counter, this device's audio bytes, the CPU times), with no
//! thread of its own and nothing on the media path. The TX thread only
//! raises a slot's flag on a refusal ([`crate::media::tx::nic_refusal`]);
//! the control tick takes the flags and, at the first refusal of a burst
//! ([`Recorder::refused`]: none in the [`BURST_QUIET_NS`] before), logs one
//! WARN `NIC refused sends: the last 30 s of traffic` with
//! [`Recorder::report`]: per second, the NIC's bytes, ours, the other
//! traffic and the CPU. So the next refusal names its cause: our traffic,
//! other traffic or a CPU stall. A dump every second would bury the real
//! lines (86 400 a day per device, cf. dstack #273), so the ring is only
//! written out at a refusal.
//!
//! "Other" is the NIC's bytes less our devices' audio: this device's, read
//! each second, and the other dstack devices' on the PC as the traffic
//! watch last measured them (a minute's average, [`Recorder::set_neighbours`]):
//! reading them each second could hold the control tick for
//! [`super::PEERS_WAIT`].

use std::collections::VecDeque;

/// How many seconds the recorder keeps.
pub const SECONDS: usize = 30;
/// How far apart its samples are, ns: the control tick's sample is taken
/// once this much has passed since the last one.
pub const SAMPLE_NS: u64 = 1_000_000_000;
/// A refusal after this long without one starts a new burst, which is
/// dumped; ns.
pub const BURST_QUIET_NS: u64 = 10_000_000_000;

/// The CPU times at a moment, all in one unit (Linux: clock ticks; Windows:
/// 100 ns): this process's (user + kernel), the whole machine's busy time
/// and its total (busy + idle), both summed over every CPU.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub process: u64,
    pub busy: u64,
    pub total: u64,
}

/// One second of the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Second {
    /// When the second ended (the control side's clock), ns.
    pub at_ns: u64,
    /// The NIC's sent bytes per second; `None`: unread, or the counter
    /// went back (a NIC reset).
    pub nic: Option<u64>,
    /// This device's audio, bytes per second.
    pub ours: u64,
    /// The NIC's bytes less our devices' audio ([`other_of`]).
    pub other: Option<u64>,
    /// This process's CPU, % of the whole machine; `None`: unread.
    pub process_cpu: Option<u64>,
    /// The machine's CPU, %; `None`: unread.
    pub system_cpu: Option<u64>,
}

/// A sample's counters, kept for the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    at: u64,
    nic: Option<u64>,
    ours: u64,
    cpu: Option<CpuTimes>,
}

/// The recorder (see the module).
#[derive(Debug, Default)]
pub struct Recorder {
    last: Option<Mark>,
    seconds: VecDeque<Second>,
    neighbours: u64,
    last_refusal: Option<u64>,
}

impl Recorder {
    pub fn new() -> Self {
        Recorder::default()
    }

    /// Whether a sample at `now_ns` is taken: the first one, or
    /// [`SAMPLE_NS`] after the last.
    pub fn due(&self, now_ns: u64) -> bool {
        self.last
            .is_none_or(|m| now_ns.saturating_sub(m.at) >= SAMPLE_NS)
    }

    /// The other dstack devices' audio on the PC, bytes per second, as the
    /// traffic watch last measured it: taken off the NIC's bytes with ours.
    pub fn set_neighbours(&mut self, bytes_per_s: u64) {
        self.neighbours = bytes_per_s;
    }

    /// After the traffic watch's step: a closed `window` sets the
    /// neighbours' audio ([`Recorder::set_neighbours`]: our devices' less
    /// this device's); `None` (no window closed) keeps the last.
    pub fn after_window(&mut self, window: Option<&super::Traffic>) {
        if let Some(t) = window {
            self.set_neighbours(t.our_devices().saturating_sub(t.ours));
        }
    }

    /// The other dstack devices' audio as last set, bytes per second.
    pub fn neighbours(&self) -> u64 {
        self.neighbours
    }

    /// At `now_ns`, once a sample is [`Recorder::due`]: the NIC's sent
    /// bytes from `nic`, this device's `ours` and the CPU times from `cpu`
    /// (each read only then); the second since the last sample added to
    /// the ring, the oldest dropped past [`SECONDS`]. The first sample
    /// only starts the record.
    pub fn record(
        &mut self,
        now_ns: u64,
        nic: impl FnOnce() -> Option<u64>,
        ours: u64,
        cpu: impl FnOnce() -> Option<CpuTimes>,
    ) {
        if !self.due(now_ns) {
            return;
        }
        let mark = Mark {
            at: now_ns,
            nic: nic(),
            ours,
            cpu: cpu(),
        };
        let Some(last) = self.last.replace(mark) else {
            return;
        };
        let span = now_ns - last.at;
        let nic = mark
            .nic
            .zip(last.nic)
            .and_then(|(b, a)| b.checked_sub(a))
            .map(|d| per_second(d, span));
        let ours = per_second(mark.ours.saturating_sub(last.ours), span);
        let (process_cpu, system_cpu) = match (last.cpu, mark.cpu) {
            (Some(a), Some(b)) => cpu_percent(a, b),
            _ => (None, None),
        };
        self.seconds.push_back(Second {
            at_ns: now_ns,
            nic,
            ours,
            other: nic.map(|n| other_of(n, ours, self.neighbours)),
            process_cpu,
            system_cpu,
        });
        if self.seconds.len() > SECONDS {
            self.seconds.pop_front();
        }
    }

    /// The seconds kept, oldest first.
    pub fn seconds(&self) -> impl Iterator<Item = &Second> {
        self.seconds.iter()
    }

    /// At `now_ns`, with a refusal seen since the last tick or not: whether
    /// to dump, i.e. it is the first refusal of a burst (none in the
    /// [`BURST_QUIET_NS`] before it). A burst that goes on over many ticks
    /// is dumped once.
    pub fn refused(&mut self, now_ns: u64, refused: bool) -> bool {
        if !refused {
            return false;
        }
        let new_burst = self
            .last_refusal
            .is_none_or(|at| now_ns.saturating_sub(at) >= BURST_QUIET_NS);
        self.last_refusal = Some(now_ns);
        new_burst
    }

    /// The record as the dump shows it, oldest first, `; ` between seconds:
    /// `-<age>s nic=<B/s> ours=<B/s> other=<B/s> cpu=<process>%/<system>%`,
    /// the age in whole seconds before the newest, `?` for an unread value;
    /// `none recorded yet` while the ring is empty.
    pub fn report(&self) -> String {
        let Some(newest) = self.seconds.back().map(|s| s.at_ns) else {
            return String::from("none recorded yet");
        };
        self.seconds
            .iter()
            .map(|s| {
                format!(
                    "-{}s nic={} ours={} other={} cpu={}%/{}%",
                    (newest - s.at_ns) / 1_000_000_000,
                    or_unread(s.nic),
                    s.ours,
                    or_unread(s.other),
                    or_unread(s.process_cpu),
                    or_unread(s.system_cpu),
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// `bytes` over `span_ns` as bytes per second (saturating; a zero span
/// gives `bytes`).
pub fn per_second(bytes: u64, span_ns: u64) -> u64 {
    let rate = u128::from(bytes) * 1_000_000_000 / u128::from(span_ns.max(1));
    u64::try_from(rate).unwrap_or(u64::MAX)
}

/// The other traffic: the NIC's `nic` bytes less `ours` and the
/// neighbours' `neighbours`, saturating (the counters are read at slightly
/// different moments, and the neighbours' is a minute's average).
pub fn other_of(nic: u64, ours: u64, neighbours: u64) -> u64 {
    nic.saturating_sub(ours.saturating_add(neighbours))
}

/// This process's and the machine's CPU between `a` and `b`, % of the
/// whole machine; `None` for both when no time passed or the total went
/// back, `None` for one whose own counter went back.
pub fn cpu_percent(a: CpuTimes, b: CpuTimes) -> (Option<u64>, Option<u64>) {
    let Some(total) = b.total.checked_sub(a.total).filter(|&t| t > 0) else {
        return (None, None);
    };
    let percent = |x: u64, y: u64| y.checked_sub(x).map(|d| per_cent_of(d, total));
    (percent(a.process, b.process), percent(a.busy, b.busy))
}

/// `part` as a whole percent of `total` (`total` > 0), saturating.
fn per_cent_of(part: u64, total: u64) -> u64 {
    u64::try_from(u128::from(part) * 100 / u128::from(total)).unwrap_or(u64::MAX)
}

/// `v` as text, `?` when unread.
fn or_unread(v: Option<u64>) -> String {
    v.map_or_else(|| String::from("?"), |v| v.to_string())
}

#[cfg(test)]
mod tests;
