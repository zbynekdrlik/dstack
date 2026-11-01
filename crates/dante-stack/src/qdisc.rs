//! The Dante NIC's queueing discipline, checked at start (dstack #1, gen3
//! on the N100 box, 31.10.2026). With fq_codel, the kernel's default, a
//! low-priority sender held the qdisc while preempted and our packets
//! queued behind it (85113 requeues): the IEM card read peak 812 µs and
//! late packets at 250 µs while our own counters were clean. With
//! `noqueue`, peak 229 µs and none late in 300 s.
//!
//! When audio TX cannot bypass the qdisc (no `CAP_NET_RAW`,
//! [`crate::media::bypass`]), the device reads the root qdisc with
//! `tc -j qdisc show dev <nic>` (Linux) and warns when it is one that queues
//! behind a lock. With the bypass the qdisc carries only other traffic, and
//! shaping it (prio + tbf) is right. No root needed; a missing `tc` leaves it
//! unknown.

use std::process::Command;

use tracing::{info, warn};

/// What the start line says about the root qdisc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// No queue, or a lockless one: our packets go to the driver at once.
    Direct(String),
    /// A qdisc that can hold our packets behind another sender.
    Queues(String),
    /// Not read (no `tc`, an answer we do not understand).
    Unknown,
}

/// Root qdiscs that never hold our packets behind a preempted sender:
/// none, or lockless (`TCQ_F_NOLOCK`).
const DIRECT: [&str; 2] = ["noqueue", "pfifo_fast"];

/// The root qdisc's kind in `tc -j qdisc show`'s answer.
pub fn root_kind(json: &str) -> Option<String> {
    let all: serde_json::Value = serde_json::from_str(json).ok()?;
    all.as_array()?
        .iter()
        .find(|q| q.get("root").and_then(serde_json::Value::as_bool) == Some(true))?
        .get("kind")?
        .as_str()
        .map(str::to_string)
}

/// The verdict on a root qdisc of `kind`.
pub fn verdict(kind: Option<String>) -> Verdict {
    match kind {
        Some(k) if DIRECT.contains(&k.as_str()) => Verdict::Direct(k),
        Some(k) => Verdict::Queues(k),
        None => Verdict::Unknown,
    }
}

/// The verdict on the interface `name`'s root qdisc, read with `tc`.
pub fn check(name: &str) -> Verdict {
    let answer = Command::new("tc")
        .args(["-j", "qdisc", "show", "dev", name])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok());
    verdict(answer.as_deref().and_then(root_kind))
}

/// [`check`] of `name`, logged: the start line's companion (log-only glue,
/// left out of the mutation gate).
pub fn report(name: &str) {
    match check(name) {
        Verdict::Direct(kind) => info!(qdisc = %kind, "the Dante NIC sends without a queue"),
        Verdict::Queues(kind) => warn!(
            qdisc = %kind,
            "the Dante NIC's qdisc can hold our packets behind another sender (late packets at a receiver): give the service CAP_NET_RAW so audio bypasses it, or set noqueue: tc qdisc replace dev <nic> root noqueue"
        ),
        Verdict::Unknown => info!("the Dante NIC's qdisc was not read (no tc)"),
    }
}

#[cfg(test)]
mod tests;
