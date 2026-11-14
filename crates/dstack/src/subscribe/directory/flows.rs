// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The directory's TX flow lists (dstack #148, #271): which multicast flow
//! carries a channel, and the reader's end of a flow list read.

use std::time::Instant;

use tracing::{debug, info};

use super::{Job, Shared, lock};
use crate::arc::client::ReadError;
use crate::arc::flows::{Flows, ListForm};
use crate::subscribe::resolve::{Advertised, Multicast};

/// The multicast flow in `list` that carries `a`'s TX channel at its bits
/// (a flow with no multicast destination, no channel or more channel
/// slots than one of our flows takes carries nothing), with the
/// transmitter's free TX flow slots: its slots less its flows, unknown for
/// a list that names no slot count.
pub fn carried(list: &Flows, a: &Advertised) -> Option<Multicast> {
    let free_slots = (list.slots > 0).then(|| {
        let used = u16::try_from(list.flows.len()).unwrap_or(u16::MAX);
        list.slots.saturating_sub(used)
    });
    list.flows.iter().find_map(|f| {
        let group = f.destinations.iter().find(|d| d.ip().is_multicast())?;
        (f.multicast()
            && u32::from(f.bits) == a.bits
            && f.channels.contains(&a.tx_channel_id)
            && f.channels.len() <= crate::media::tx::MAX_CHANNELS)
            .then(|| Multicast {
                group: *group,
                channels: f.channels.clone(),
                fpp: f.fpp,
                bits: a.bits,
                free_slots,
            })
    })
}

/// A flow list read `job` ended with `result`: logged and kept. A
/// device's first refusal is logged at info (dstack #271: a refused list
/// every minute was visible only as `refused=1` in the minute's report).
pub(super) fn flows_read(
    shared: &Shared,
    job: &Job,
    result: Result<(Flows, ListForm), ReadError>,
    sent: usize,
    peer: usize,
    started: Instant,
) {
    let ms = started.elapsed().as_millis();
    let error = match &result {
        Ok((list, form)) => {
            info!(
                peer,
                op = format_args!("{:#06x}", form.op()),
                flows = list.flows.len(),
                multicast = list.flows.iter().filter(|f| f.multicast()).count(),
                unread = list.unread,
                requests = sent,
                ms,
                "a transmitter's TX flows read through its ARC"
            );
            None
        }
        Err(e) => Some(e.to_string()),
    };
    let now = shared.now();
    let tell = lock(&shared.book).finished_flows(job, result, sent, now);
    if let Some(error) = error {
        if tell {
            info!(
                peer,
                requests = sent,
                ms,
                error,
                "a transmitter refused our TX flow list read (told once per device)"
            );
        } else {
            debug!(
                peer,
                requests = sent,
                ms,
                error,
                "an ARC read of a transmitter's TX flows failed"
            );
        }
    }
}
