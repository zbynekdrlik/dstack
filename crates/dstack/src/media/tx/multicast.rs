// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Our multicast TX flows (ARC 2.8.9's create and delete, dstack #193): a
//! [`TxTable`] entry sent to a group ([`multicast_group`]) rather than to
//! the receiver that asked for it. No receiver keeps one alive, so it
//! never expires; the TX thread sends it as any other flow, from the
//! slot's socket ([`multicast_out`]).

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use super::{MAX_FLOWS, TxFlow, TxTable};
use crate::arc::flows::MAX_FLOW_CHANNELS;
use crate::device::DeviceConfig;

/// The UDP port of our multicast flows' groups, Dante's.
pub const MULTICAST_PORT: u16 = 4321;
/// The TTL of our multicast flows' packets: what Audinate's own audio
/// packets carry (a pktmon capture, dstack #141). The OS default of 1 would
/// do on one subnet; this keeps them as far as Dante's.
pub const MULTICAST_TTL: u32 = 32;

/// The group of our multicast flow in `slot` (from 0, under [`MAX_FLOWS`])
/// of the device at `ip` with process id `process_id` (`cmc::device_id`'s
/// parts), port [`MULTICAST_PORT`]: `239.255.<ip's last byte>.<process mod
/// 8, slot>` in the administratively scoped range Dante's own groups use.
/// The same every time for the same flow, and no two of our flows share a
/// group on one /24 while no two devices of one address have process ids
/// 8 apart.
pub fn multicast_group(ip: Ipv4Addr, process_id: u16, slot: usize) -> SocketAddrV4 {
    let [.., host] = ip.octets();
    let flow = (process_id as u8)
        .wrapping_mul(MAX_FLOWS as u8)
        .wrapping_add(slot as u8);
    SocketAddrV4::new(Ipv4Addr::new(239, 255, host, flow), MULTICAST_PORT)
}

/// A flow socket's multicast sends (dstack #193): out of the device's
/// address `ip` (the interface a group is sent on is otherwise the
/// routing table's), with [`MULTICAST_TTL`]. Looped back to this host's
/// members, as by default (another device on the same PC may receive the
/// flow).
pub(super) fn multicast_out(s: &UdpSocket, ip: Ipv4Addr) -> io::Result<()> {
    let r = socket2::SockRef::from(s);
    r.set_multicast_if_v4(&ip)?;
    r.set_multicast_ttl_v4(MULTICAST_TTL)
}

/// Whether `channels` can make a flow of a device with `tx` TX channels:
/// 1 to [`MAX_FLOW_CHANNELS`] of them, each one of ours (from 1) and each
/// once.
fn flow_channels(channels: &[u16], tx: usize) -> bool {
    (1..=MAX_FLOW_CHANNELS).contains(&channels.len())
        && channels.iter().all(|&c| (1..=tx).contains(&usize::from(c)))
        && (1..channels.len()).all(|i| !channels[..i].contains(&channels[i]))
}

impl TxTable {
    /// The device `cfg` at `ip` makes its own multicast flow of TX
    /// `channels` (from 1) in `slot` (from 0): sent to [`multicast_group`],
    /// with the config's `tx_fpp_max` frames a packet (the most its
    /// advertised latency covers) and its encoding. Returns the group.
    /// Refused, with the reason, for channels that cannot make a flow
    /// ([`flow_channels`]) and as [`TxTable::add_multicast`] refuses.
    pub fn add_own_multicast(
        &mut self,
        cfg: &DeviceConfig,
        ip: Ipv4Addr,
        slot: usize,
        channels: &[u16],
    ) -> Result<SocketAddrV4, &'static str> {
        if !flow_channels(channels, cfg.tx.len()) {
            return Err("channels the device does not have");
        }
        let group = multicast_group(ip, cfg.process_id, slot);
        self.add_multicast(slot, group, channels, cfg.tx_fpp_max, cfg.encoding / 8)?;
        Ok(group)
    }

    /// A multicast flow in `slot` (ARC 2.8.9's create, dstack #193) of
    /// our TX `channels` (from 1), sent to `group` with `fpp` frames a
    /// packet and `bps` bytes a sample. Refused, with the reason, for a
    /// slot past the device's flow slots or one that holds a flow (expired
    /// or not, as the ARC lists show it).
    pub fn add_multicast(
        &mut self,
        slot: usize,
        group: SocketAddrV4,
        channels: &[u16],
        fpp: u16,
        bps: u8,
    ) -> Result<(), &'static str> {
        if slot >= self.slots {
            return Err("a slot past the device's flow slots");
        }
        if self.flows[slot].is_some() {
            return Err("a slot that holds a flow");
        }
        let cookie = self.cookie();
        self.flows[slot] = Some(TxFlow {
            dest: group,
            rx_host: String::new(),
            rx_flow_name: String::new(),
            channels: channels.iter().copied().map(Some).collect(),
            fpp,
            bps,
            cookie,
            heard_ns: 0,
            expired: false,
            multicast: true,
        });
        Ok(())
    }

    /// Removes the multicast flow in `slot` (ARC 2.8.9's delete); `false`
    /// when the slot holds none (empty, or a receiver's flow).
    pub fn remove_multicast(&mut self, slot: usize) -> bool {
        let found = self.flow(slot).is_some_and(|f| f.multicast);
        if found {
            self.flows[slot] = None;
        }
        found
    }
}

#[cfg(test)]
mod tests;
