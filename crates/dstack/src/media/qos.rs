//! The CPU wake latency the media threads need (dstack #1, gen3 at 250 µs
//! on the N100 box, 31.10.2026). With the deep idle states (C6, C8, C10)
//! allowed, the package sleeps between packets 83 µs apart, and the NIC's
//! DMA and the wake wait for it: the IEM card read peak 490 µs and late
//! packets at 250 µs; with them disabled, peak 240 µs and none in 300 s.
//! No counter of ours sees that time: it comes after `send_to`.
//!
//! Linux takes a PM QoS request through `/dev/cpu_dma_latency`: a 32-bit
//! latency in µs written to it holds while the file stays open. The TX
//! thread holds 0 (no idle state that costs a wake) while it runs, in the
//! low-latency profile. Refused (the device is root's by default), it logs
//! the reason and the device runs without it: provisioning then grants
//! the device or disables the idle states.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use tracing::{info, warn};

/// Linux's PM QoS device for the CPU wake latency.
pub const CPU_DMA_LATENCY: &str = "/dev/cpu_dma_latency";

/// The wake latency held, µs: no idle state that costs a wake.
pub const HOLD_US: i32 = 0;

/// Opens `path` and writes `us` as the kernel reads it (a native-endian
/// 32-bit value); the request holds while the returned file stays open.
pub fn request(path: &Path, us: i32) -> io::Result<File> {
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.write_all(&us.to_ne_bytes())?;
    Ok(f)
}

/// [`request`] of [`HOLD_US`] at `path`, logged; `None` when refused.
pub fn hold(path: &Path) -> Option<File> {
    match request(path, HOLD_US) {
        Ok(f) => {
            info!(path = %path.display(), us = HOLD_US, "CPU wake latency held while TX runs");
            Some(f)
        }
        Err(e) => {
            warn!(
                path = %path.display(),
                kind = ?e.kind(),
                "CPU wake latency not held: deep idle states can delay every packet (grant the device or disable them)"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests;
