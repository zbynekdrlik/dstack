// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the settings window checks before Apply (dstack #512): the least
//! app reserve dstack takes for the chosen buffer, the reserve kept when
//! the buffer changes, and what dstack's own config check would refuse in
//! the file an apply writes. Pure, every platform.

use dstack::clock::latency_samples;
use dstack::device::NORMAL_MIN_WORK_MARGIN_NS;

use super::{Fields, changes, edit, restarts};

/// The least app reserve dstack takes for `f`, samples: one buffer, and
/// 2 ms under the `normal` timing profile (`DeviceConfig::problems`).
pub fn least_work_margin(f: &Fields) -> u64 {
    let normal = if f.normal {
        latency_samples(NORMAL_MIN_WORK_MARGIN_NS, f.sample_rate)
    } else {
        0
    };
    f.block.max(normal)
}

/// The app reserve after a new buffer pick in `f`: the one chosen while
/// dstack still takes it, else the least it takes ([`least_work_margin`]),
/// so the reserve list never sits on a value the config refuses.
pub fn reserve_for(f: &Fields) -> u64 {
    f.work_margin.max(least_work_margin(f))
}

/// Why the file an apply of `new` over `old` writes would be refused by
/// the config check (an app reserve under the buffer, say), as [`super::plan`]
/// writes it (the file keeps its own RX latency: that one is the live
/// write's); `None` when no field that needs a restart changed, or the
/// file is fine. `text` is the instance's config as read.
pub fn file_problem(text: &str, old: &Fields, new: &Fields) -> Option<String> {
    if restarts(&changes(old, new)) == 0 {
        return None;
    }
    let mut file = new.clone();
    file.latency_ns = match Fields::read(text) {
        Ok(f) => f.latency_ns,
        Err(e) => return Some(e),
    };
    edit(text, &file).err()
}

#[cfg(test)]
mod tests;
