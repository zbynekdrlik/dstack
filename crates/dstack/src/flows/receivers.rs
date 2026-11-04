// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Each TX flow's receiver latency, as the receiver states it (dstack #1):
//! every Dante device sets its own receive latency, and a packet is late
//! only against the latency its receiver plays at (the owner's ruling;
//! DVS at 10 ms was warned late for packets of 1.15 ms against our
//! advertised latency).
//!
//! The read is the standard one, dcontroller's too: the settings
//! ([`crate::arc::control::settings`], ARC 0x1100, start 0x2809) of the device that
//! asked for the flow, at the ARC its `_netaudio-arc` record announces
//! ([`ArcRecords`], the device's own browse; several devices on one PC each
//! have their own port), only when that record's address is the flow's
//! destination. Read-only: nothing is written to the receiver.
//!
//! - [`Reads`] (pure, times in ns): which flow is due. A flow is read when
//!   it starts and again every [`REREAD_NS`] (Dante Controller may change a
//!   receiver's latency); a failed read is asked again after
//!   [`retry_after`]`(failures)`, a device whose record is not in yet at
//!   the next look.
//! - [`spawn`]: one thread, one read at a time (each waits at most the ARC
//!   client's reply timeout), off the TX thread's path. It hands each
//!   latency to [`TxShared::set_receiver_latency`]; a failed read leaves
//!   the last one (keyed by destination: a reused slot starts unknown).
//!
//! Logs name slots and latencies, never a device name or an address.

use std::io;
use std::net::SocketAddrV4;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{debug, info};

use crate::arc::client::ReadError;
use crate::arc::control::Settings;
use crate::media::tx::{MAX_FLOWS, TxShared};
use crate::subscribe::directory::ArcRecords;
use crate::subscribe::retry_after;

/// How often a flow's receiver latency is read again, ns.
pub const REREAD_NS: u64 = 30_000_000_000;
/// The reader's wait between looks.
pub const IDLE: Duration = Duration::from_secs(1);

/// A read to make: the flow's slot, its receiver and the receiving
/// device's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub slot: usize,
    pub dest: SocketAddrV4,
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    job: Job,
    due_ns: u64,
    failures: u32,
}

/// The flows' reads (see the module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reads {
    entries: Vec<Option<Entry>>,
}

impl Default for Reads {
    fn default() -> Self {
        Self::new()
    }
}

impl Reads {
    pub fn new() -> Self {
        Reads {
            entries: vec![None; MAX_FLOWS],
        }
    }

    /// Follows the flows sending at `now` (slot, receiver, device name): a
    /// new flow, or a slot with another receiver, is due now; a slot no
    /// longer sending is forgotten.
    pub fn follow(&mut self, flows: &[(usize, SocketAddrV4, String)], now: u64) {
        for (slot, entry) in self.entries.iter_mut().enumerate() {
            let flow = flows.iter().find(|(s, _, _)| *s == slot);
            let job = flow.map(|(slot, dest, device)| Job {
                slot: *slot,
                dest: *dest,
                device: device.clone(),
            });
            if entry.as_ref().map(|e| &e.job) != job.as_ref() {
                *entry = job.map(|job| Entry {
                    job,
                    due_ns: now,
                    failures: 0,
                });
            }
        }
    }

    /// The first read due at `now`.
    pub fn next(&self, now: u64) -> Option<Job> {
        self.entries
            .iter()
            .flatten()
            .find(|e| e.due_ns <= now)
            .map(|e| e.job.clone())
    }

    /// `job` ended at `now` with `outcome`.
    pub fn finished(&mut self, job: &Job, outcome: Outcome, now: u64) {
        let Some(Some(e)) = self.entries.get_mut(job.slot) else {
            return;
        };
        if e.job != *job {
            return;
        }
        match outcome {
            Outcome::Read => {
                e.failures = 0;
                e.due_ns = now.saturating_add(REREAD_NS);
            }
            Outcome::Failed => {
                e.failures = e.failures.saturating_add(1);
                e.due_ns = now.saturating_add(retry_after(e.failures));
            }
            Outcome::NotYet => e.due_ns = now.saturating_add(1),
        }
    }
}

/// How a read ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Read: again in [`REREAD_NS`].
    Read,
    /// Asked and failed: again after [`retry_after`] its failures.
    Failed,
    /// Nothing asked (the device's ARC record is not in yet): again at the
    /// next look, no failure counted.
    NotYet,
}

/// A device's latency from its settings, ns: in effect, else as
/// configured; 0 when it states neither.
pub fn latency_of(s: &Settings) -> u64 {
    s.latency_active_ns
        .or(s.latency_configured_ns)
        .map_or(0, u64::from)
}

/// Why a receiver's latency was not read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unread {
    #[error("no ARC record of the receiving device")]
    NoRecord,
    #[error("the device's ARC record is at another address than the flow")]
    OtherAddress,
    #[error("the device states no latency")]
    NoLatency,
    #[error(transparent)]
    Read(#[from] ReadError),
}

/// `job`'s receiver latency, ns: the settings `read` gives from the ARC at
/// `arc` (the device's record), when that is at the flow's address.
pub fn read_latency(
    job: &Job,
    arc: Option<SocketAddrV4>,
    read: impl FnOnce(SocketAddrV4) -> Result<Settings, ReadError>,
) -> Result<u64, Unread> {
    let arc = arc.ok_or(Unread::NoRecord)?;
    if arc.ip() != job.dest.ip() {
        return Err(Unread::OtherAddress);
    }
    match latency_of(&read(arc)?) {
        0 => Err(Unread::NoLatency),
        ns => Ok(ns),
    }
}

/// One look at `now`: follows `tx`'s flows, then makes every read due
/// (each receiver's ARC from `records`, its settings from `read`) and hands
/// each latency read to `tx`.
pub fn look(
    reads: &mut Reads,
    tx: &TxShared,
    records: &ArcRecords,
    now: u64,
    mut read: impl FnMut(SocketAddrV4) -> Result<Settings, ReadError>,
) {
    reads.follow(&tx.receiver_flows(), now);
    while let Some(job) = reads.next(now) {
        let result = read_latency(&job, records.arc_of(&job.device), &mut read);
        match &result {
            Ok(ns) => {
                if tx.receiver_latency_ns(job.slot, job.dest) != *ns {
                    info!(
                        slot = job.slot,
                        latency_us = ns / 1_000,
                        "a TX flow's receiver states its latency"
                    );
                }
                tx.set_receiver_latency(job.slot, job.dest, *ns);
            }
            Err(e) => debug!(slot = job.slot, error = %e, "a TX flow's receiver latency not read"),
        }
        let outcome = match result {
            Ok(_) => Outcome::Read,
            Err(Unread::NoRecord) => Outcome::NotYet,
            Err(_) => Outcome::Failed,
        };
        reads.finished(&job, outcome, now);
    }
}

/// The reader thread ([`spawn`]): it ends when this is dropped (after the
/// read it waits on, at most [`IDLE`] later otherwise).
#[derive(Debug)]
pub struct Reader {
    stop: Arc<AtomicBool>,
    _thread: JoinHandle<()>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts the reader: a [`look`] every `idle` with `read` (the daemon's
/// reads [`crate::arc::control::settings`] over UDP from our address).
pub fn spawn(
    tx: Arc<TxShared>,
    records: ArcRecords,
    idle: Duration,
    mut read: impl FnMut(SocketAddrV4) -> Result<Settings, ReadError> + Send + 'static,
) -> io::Result<Reader> {
    let stop = Arc::new(AtomicBool::new(false));
    let ends = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("dstack-rx-latency".to_string())
        .spawn(move || {
            let origin = Instant::now();
            let mut reads = Reads::new();
            while !ends.load(Ordering::Relaxed) {
                let now = u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
                look(&mut reads, &tx, &records, now, &mut read);
                std::thread::sleep(idle);
            }
        })?;
    Ok(Reader {
        stop,
        _thread: thread,
    })
}

#[cfg(test)]
mod tests;
