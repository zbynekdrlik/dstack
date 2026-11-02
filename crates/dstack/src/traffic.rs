//! Other traffic on the Dante NIC, watched while the device runs (dstack
//! #1, the FOH PC, 1.11.2026). Every UDP sender there saw 1 to 2.5 ms gaps
//! every ~285 ms, the period of another program's TCP bursts on the same
//! NIC: one NIC queue has no priority between packets (no DCB on consumer
//! NICs), so our audio waits behind whatever the NIC holds.
//!
//! Once a minute the watch compares the NIC's own sent-bytes counter with
//! our audio's wire bytes ([`TxShared::bytes_sent`]) and warns when the
//! rest averages over [`OTHER_BYTES_PER_S`]. Our own control traffic
//! (heartbeats, mDNS, ARC answers) is a few kB/s, well under it.

use std::net::Ipv4Addr;

use tracing::warn;

use crate::media::tx::TxShared;

/// How often the watch looks, ns.
pub const EVERY_NS: u64 = 60_000_000_000;
/// Other traffic past this average, bytes per second, is warned about.
pub const OTHER_BYTES_PER_S: u64 = 100_000;

/// The watch's memory: the last look's time and counters.
#[derive(Debug, Default)]
pub struct TrafficWatch {
    last: Option<(u64, u64, u64)>,
}

impl TrafficWatch {
    pub const fn new() -> Self {
        TrafficWatch { last: None }
    }

    /// At `now_ns`, with the NIC's sent bytes `nic` (none when unread) and
    /// ours `ours`: once [`EVERY_NS`] has passed since the last look, the
    /// other traffic's average in bytes per second; `None` between looks,
    /// at the first look and when the NIC is unread. A counter that went
    /// back (a NIC reset) starts the window again.
    pub fn look(&mut self, now_ns: u64, nic: Option<u64>, ours: u64) -> Option<u64> {
        let nic = nic?;
        match self.last {
            Some((at, _, _)) if now_ns.saturating_sub(at) < EVERY_NS => None,
            Some((at, nic0, ours0)) => {
                self.last = Some((now_ns, nic, ours));
                if nic < nic0 || ours < ours0 {
                    return None;
                }
                let other = (nic - nic0).saturating_sub(ours - ours0);
                let secs = (now_ns - at) / 1_000_000_000;
                Some(other / secs.max(1))
            }
            None => {
                self.last = Some((now_ns, nic, ours));
                None
            }
        }
    }

    /// [`TrafficWatch::look`] on the NIC holding `ip` and `tx`'s audio, a
    /// rate over [`OTHER_BYTES_PER_S`] logged as a warning.
    pub fn watch(&mut self, now_ns: u64, ip: Ipv4Addr, tx: &TxShared) {
        if let Some(rate) = self.look(now_ns, nic_sent_bytes(ip), tx.bytes_sent())
            && rate > OTHER_BYTES_PER_S
        {
            warn!(
                other_bytes_per_s = rate,
                "the Dante NIC carries other traffic: our audio waits behind it in the NIC's queue (late packets at a receiver); move that traffic to another NIC (docs/windows-box.md, docs/linux-box.md)"
            );
        }
    }
}

/// The bytes the NIC holding `ip` has sent, its own counter; `None` when
/// no interface holds `ip` or the counter cannot be read.
#[cfg(target_os = "linux")]
pub fn nic_sent_bytes(ip: Ipv4Addr) -> Option<u64> {
    let name = if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .find(|i| i.ip() == std::net::IpAddr::V4(ip))?
        .name;
    std::fs::read_to_string(format!("/sys/class/net/{name}/statistics/tx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::nic_sent_bytes;

#[cfg(not(any(target_os = "linux", windows)))]
pub fn nic_sent_bytes(_ip: Ipv4Addr) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests;
