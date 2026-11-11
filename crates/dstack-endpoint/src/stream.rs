// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a running endpoint stream's error means (dstack #164): cpal's error
//! callback reports glitches and fatal faults alike. A glitch (an xrun:
//! WASAPI's data-discontinuity flag, seen on the first packets of a fresh
//! loopback stream) leaves the stream running; opening it again for one
//! starts a fresh stream that reports another, every retry (seen live on a
//! Windows PC, 10.11.2026). Only a fault opens the endpoint again.

use cpal::ErrorKind;

/// Whether a stream error of `kind` leaves the stream dead, so the app
/// opens the endpoint again: every kind but a glitch (an xrun), a reroute
/// the stream follows by itself, and a refused real-time promotion.
pub fn needs_reopen(kind: ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied
    )
}

/// Whether a stream error of `kind` is a data discontinuity (dstack #198:
/// WASAPI's flag, cpal's xrun): audio was lost between two packets, which
/// dsend measures on the next packet.
pub fn is_discontinuity(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::Xrun)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glitch_keeps_the_stream_and_a_fault_opens_it_again() {
        for kind in [
            ErrorKind::Xrun,
            ErrorKind::DeviceChanged,
            ErrorKind::RealtimeDenied,
        ] {
            assert!(!needs_reopen(kind), "{kind:?}");
        }
        for kind in [
            ErrorKind::DeviceNotAvailable,
            ErrorKind::StreamInvalidated,
            ErrorKind::HostUnavailable,
            ErrorKind::DeviceBusy,
            ErrorKind::BackendError,
        ] {
            assert!(needs_reopen(kind), "{kind:?}");
        }
    }

    #[test]
    fn only_an_xrun_is_a_discontinuity() {
        assert!(is_discontinuity(ErrorKind::Xrun));
        for kind in [
            ErrorKind::DeviceChanged,
            ErrorKind::RealtimeDenied,
            ErrorKind::DeviceNotAvailable,
            ErrorKind::StreamInvalidated,
            ErrorKind::BackendError,
        ] {
            assert!(!is_discontinuity(kind), "{kind:?}");
        }
    }
}
