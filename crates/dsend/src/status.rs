// SPDX-License-Identifier: MIT OR Apache-2.0

//! dsend's status (dstack #164): the log line every 10 s and the app part
//! of the device's status, which dtray shows after the device's own line
//! (`Link::set_app_status`; `line`, and `state` "ok" or "warn"), with the
//! outputs its Output menu offers.

use dstack_endpoint::status::{Snapshot, state, status_name, with_outputs};

pub use dstack_endpoint::status::line;

/// What dtray shows of dsend.
#[derive(Debug, Clone, Copy)]
pub struct AppView<'a> {
    pub snap: &'a Snapshot,
    /// The underruns of the last reading.
    pub underruns_before: u64,
    /// The capture handed over frames since the last reading ([`capturing`]).
    pub capturing: bool,
    /// The captured output's rate, Hz.
    pub in_rate: u32,
    /// The output devices' names, in the order `output <n>` counts them.
    pub outputs: &'a [String],
    /// The one captured.
    pub current: usize,
}

/// Whether the capture runs: its FIFO's frames grew since the last
/// reading (`before`, [`Fifo::arrived`](dstack_endpoint::fifo::Fifo::arrived)).
/// With the keep-alive stream it runs while nothing plays too (silence);
/// a stop means the output was lost or its driver failed.
pub fn capturing(before: u64, now: u64) -> bool {
    now > before
}

/// Wakes (seconds) with no captured frame before the output is opened
/// again: with the keep-alive the capture never pauses, so a stop means a
/// dead stream (the output re-enumerated, its driver reset).
pub const STALL_WAKES: u32 = 3;
/// The fewest wakes between two openings of the output: a stream that
/// fails again at once is retried every 5 s, never every wake.
pub const RETRY_WAKES: u32 = 5;

/// The wakes the capture has been stalled for after this one: 0 when its
/// frames grew (`before` to `now`, [`Fifo::arrived`](dstack_endpoint::fifo::Fifo::arrived)),
/// else one more than `stalled`.
pub fn stalled_after(stalled: u32, before: u64, now: u64) -> u32 {
    if now > before {
        0
    } else {
        stalled.saturating_add(1)
    }
}

/// Whether the main thread opens the captured output again: a stream
/// reported an error (`failed`) or the capture has been stalled for
/// [`STALL_WAKES`] wakes (`stalled`), and the last opening is at least
/// [`RETRY_WAKES`] wakes ago (`since`).
pub fn reopen_due(failed: bool, stalled: u32, since: u32) -> bool {
    since >= RETRY_WAKES && (failed || stalled >= STALL_WAKES)
}

/// dtray's line for dsend: `<output> (48000 Hz): capturing, depth 40.1
/// ms, 2 underruns`.
pub fn short_line(v: &AppView<'_>) -> String {
    let output = v
        .outputs
        .get(v.current)
        .map_or_else(|| "no output".to_string(), |o| status_name(o));
    let capture = if v.capturing {
        "capturing"
    } else {
        "no capture"
    };
    format!(
        "{output} ({} Hz): {capture}, depth {:.1} ms, {} underruns",
        v.in_rate, v.snap.depth_ms, v.snap.underruns
    )
}

/// The app part of the device's status for dtray: one JSON object with
/// dtray's `line` ([`short_line`]) and `state` (warn while priming, on new
/// underruns, or while the capture stopped), the numbers (`in_rate` the
/// captured output's, `out_rate` the network's), and the outputs dtray's
/// Output menu offers ([`with_outputs`]: `outputs`, `output`).
pub fn app_status(v: &AppView<'_>) -> String {
    let s = v.snap;
    let state = if v.capturing {
        state(s, v.underruns_before)
    } else {
        "warn"
    };
    let base = serde_json::json!({
        "line": short_line(v),
        "state": state,
        "capturing": v.capturing,
        "depth_ms": (s.depth_ms * 10.0).round() / 10.0,
        "ppm": (s.ppm * 10.0).round() / 10.0,
        "underruns": s.underruns,
        "played": s.played,
        "in_rate": v.in_rate,
        "out_rate": s.out_rate,
    });
    with_outputs(&base, v.outputs, v.current)
}

/// The app command dsend takes (`app output <n> [<name>]`, from dtray's
/// menu): the output to capture
/// ([`dstack_endpoint::status::parse_command`]).
pub fn parse_command(words: &str, outputs: &[String]) -> Result<usize, String> {
    dstack_endpoint::status::parse_command(crate::cli::APP, words, outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> Snapshot {
        Snapshot {
            depth_ms: 40.06,
            ppm: -3.24,
            underruns: 2,
            played: 480_000,
            out_rate: 48_000,
            overflows: 0,
            recentres: 0,
            priming: false,
            mono: false,
        }
    }

    fn view<'a>(s: &'a Snapshot, outputs: &'a [String]) -> AppView<'a> {
        AppView {
            snap: s,
            underruns_before: 2,
            capturing: true,
            in_rate: 44_100,
            outputs,
            current: 1,
        }
    }

    #[test]
    fn the_app_status_is_one_json_object_dtray_reads() {
        let outs = vec!["Speakers".to_string(), "HDMI\n2".to_string()];
        let s = snap();
        let text = app_status(&view(&s, &outs));
        assert_eq!(dstack::ctl::app_object(&text), Some(text.clone()));
        let j: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            j["line"],
            "HDMI 2 (44100 Hz): capturing, depth 40.1 ms, 2 underruns"
        );
        assert_eq!(j["state"], "ok");
        assert_eq!(j["capturing"], true);
        assert_eq!(j["depth_ms"], 40.1);
        assert_eq!(j["ppm"], -3.2);
        assert_eq!(j["underruns"], 2);
        assert_eq!(j["played"], 480_000);
        assert_eq!(j["in_rate"], 44_100);
        assert_eq!(j["out_rate"], 48_000);
        assert_eq!(j["output"], 1);
        assert_eq!(j["outputs"], serde_json::json!(["Speakers", "HDMI 2"]));
    }

    #[test]
    fn the_state_warns_on_new_underruns_priming_or_a_stopped_capture() {
        let outs = vec!["A".to_string()];
        let s = snap();
        let state_of = |v: AppView<'_>| {
            let j: serde_json::Value = serde_json::from_str(&app_status(&v)).unwrap();
            j["state"].as_str().unwrap().to_string()
        };
        let v = AppView {
            current: 0,
            ..view(&s, &outs)
        };
        assert_eq!(state_of(v), "ok");
        assert_eq!(
            state_of(AppView {
                underruns_before: 1,
                ..v
            }),
            "warn"
        );
        let stopped = AppView {
            capturing: false,
            ..v
        };
        assert_eq!(state_of(stopped), "warn");
        assert_eq!(
            short_line(&stopped),
            "A (44100 Hz): no capture, depth 40.1 ms, 2 underruns"
        );
        let priming = Snapshot { priming: true, ..s };
        assert_eq!(
            state_of(AppView {
                snap: &priming,
                ..v
            }),
            "warn"
        );
        assert_eq!(
            short_line(&AppView { current: 3, ..v }),
            "no output (44100 Hz): capturing, depth 40.1 ms, 2 underruns"
        );
    }

    #[test]
    fn the_capture_runs_while_its_frames_grow() {
        assert!(capturing(10, 11));
        assert!(!capturing(10, 10));
        assert!(!capturing(0, 0));
    }

    #[test]
    fn a_stream_error_or_a_stalled_capture_opens_the_output_again_at_most_every_5_s() {
        assert!(reopen_due(true, 0, RETRY_WAKES), "an error");
        assert!(reopen_due(false, STALL_WAKES, RETRY_WAKES), "a stall");
        assert!(
            !reopen_due(false, STALL_WAKES - 1, u32::MAX),
            "not stalled yet"
        );
        assert!(
            !reopen_due(true, 9, RETRY_WAKES - 1),
            "too soon after the last"
        );
        assert!(!reopen_due(false, 0, u32::MAX), "running");
        const _: () = assert!(STALL_WAKES == 3 && RETRY_WAKES == 5);
    }

    #[test]
    fn the_stall_counts_wakes_with_no_new_frame() {
        assert_eq!(stalled_after(4, 10, 11), 0);
        assert_eq!(stalled_after(4, 10, 10), 5);
        assert_eq!(stalled_after(0, 0, 0), 1);
        assert_eq!(stalled_after(u32::MAX, 3, 3), u32::MAX);
    }

    #[test]
    fn the_command_chooses_an_output_and_names_dsend() {
        let outs = vec!["Speakers".to_string(), "HDMI".to_string()];
        assert_eq!(parse_command("output 1 HDMI", &outs), Ok(1));
        assert_eq!(
            parse_command("volume 1", &outs),
            Err("dsend takes: output <n> [<name>]".into())
        );
    }

    #[test]
    fn the_log_line_is_the_shared_one() {
        assert!(line(&snap()).starts_with("depth 40.1 ms, -3.2 ppm, 2 underruns"));
    }
}
