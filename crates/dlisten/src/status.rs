// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlisten's status (dstack #139): the log line every 10 s and the app
//! part of the device's status, which dtray shows after the device's own
//! line (`Link::set_app_status`; `line`, and `state` "ok" or "warn").

use dlink::{RxChannel, SubState};

use crate::fifo::Fifo;
use crate::player::Stats;

/// One reading of the output side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    /// The FIFO's depth, ms of the network's rate.
    pub depth_ms: f64,
    /// The converter's correction, ppm.
    pub ppm: f64,
    pub underruns: u64,
    /// Output frames played.
    pub played: u64,
    /// The output's rate, Hz.
    pub out_rate: u32,
    /// Network frames dropped on a full FIFO.
    pub overflows: u64,
    pub recentres: u64,
    /// The output waits for the FIFO to fill (silence: no Dante audio yet,
    /// or a refill after an underrun).
    pub priming: bool,
}

impl Snapshot {
    /// The reading of `fifo` (at `net_rate`) and the player's `stats`.
    pub fn read(fifo: &Fifo, stats: &Stats, net_rate: u32) -> Self {
        Snapshot {
            depth_ms: fifo.len() as f64 * 1000.0 / f64::from(net_rate.max(1)),
            ppm: stats.ppm(),
            underruns: stats.underruns(),
            played: stats.played(),
            out_rate: stats.out_rate(),
            overflows: fifo.overflows(),
            recentres: stats.recentres(),
            priming: stats.priming(),
        }
    }
}

/// The status line: `depth 40.1 ms, +3.2 ppm, 0 underruns, 480000 frames
/// played at 48000 Hz`, then `, waiting for audio` while priming and the
/// overflows and re-centres when there are any.
pub fn line(s: &Snapshot) -> String {
    let mut text = format!(
        "depth {:.1} ms, {:+.1} ppm, {} underruns, {} frames played at {} Hz",
        s.depth_ms, s.ppm, s.underruns, s.played, s.out_rate
    );
    if s.priming {
        text.push_str(", waiting for audio");
    }
    if s.overflows > 0 {
        text.push_str(&format!(", {} frames overflowed", s.overflows));
    }
    if s.recentres > 0 {
        text.push_str(&format!(", {} re-centres", s.recentres));
    }
    text
}

/// "warn" while priming or when the underruns grew past `underruns_before`
/// (the last reading's), else "ok".
pub fn state(s: &Snapshot, underruns_before: u64) -> &'static str {
    if s.priming || s.underruns > underruns_before {
        "warn"
    } else {
        "ok"
    }
}

/// The most bytes of the app part the device's status carries
/// (`dstack::ctl::APP_MAX`): a longer one is left out whole.
pub const APP_MAX: usize = 4096;

/// Whether audio flows: a channel whose subscription is receiving
/// (connected) wrote samples since the last reading (`before`, each
/// channel's `written`, [`written`]). The pump writes silence into the FIFO
/// either way; a lost flow shows as a count that stops.
pub fn flowing(before: &[u64], now: &[RxChannel]) -> bool {
    now.iter().enumerate().any(|(i, ch)| {
        ch.status.is_some_and(|s| s.state() == SubState::Connected)
            && ch.written > before.get(i).copied().unwrap_or(0)
    })
}

/// The channels' `written` counts, for the next [`flowing`].
pub fn written(now: &[RxChannel]) -> Vec<u64> {
    now.iter().map(|ch| ch.written).collect()
}

/// What dtray shows of dlisten.
#[derive(Debug, Clone, Copy)]
pub struct AppView<'a> {
    pub snap: &'a Snapshot,
    /// The underruns of the last reading.
    pub underruns_before: u64,
    pub flowing: bool,
    /// The output devices' names, in the order `output <n>` counts them.
    pub outputs: &'a [String],
    /// The one playing.
    pub current: usize,
}

/// dtray's line for dlisten: `<output> (48000 Hz): audio flowing, depth
/// 40.1 ms, 2 underruns`.
pub fn short_line(v: &AppView<'_>) -> String {
    let output = v.outputs.get(v.current).map_or("no output", String::as_str);
    let audio = if v.flowing {
        "audio flowing"
    } else {
        "no audio"
    };
    format!(
        "{output} ({} Hz): {audio}, depth {:.1} ms, {} underruns",
        v.snap.out_rate, v.snap.depth_ms, v.snap.underruns
    )
}

/// The app part of the device's status for dtray (`Link::set_app_status`):
/// one JSON object with dtray's `line` ([`short_line`]) and `state`
/// ([`state`]), the numbers, and the outputs dtray's Output menu offers:
/// `outputs` (their names) and `output` (the index of the one playing; the
/// index is what `app output <n>` names). Within [`APP_MAX`]: outputs past
/// it are left off the list (`outputs_cut`: true).
pub fn app_status(v: &AppView<'_>) -> String {
    let s = v.snap;
    let mut outputs = v.outputs.len();
    loop {
        let mut obj = serde_json::json!({
            "line": short_line(v),
            "state": state(s, v.underruns_before),
            "flowing": v.flowing,
            "depth_ms": (s.depth_ms * 10.0).round() / 10.0,
            "ppm": (s.ppm * 10.0).round() / 10.0,
            "underruns": s.underruns,
            "played": s.played,
            "out_rate": s.out_rate,
            "output": v.current,
            "outputs": &v.outputs[..outputs],
        });
        if outputs < v.outputs.len() {
            obj["outputs_cut"] = serde_json::Value::Bool(true);
        }
        let text = obj.to_string();
        if text.len() <= APP_MAX || outputs == 0 {
            return text;
        }
        outputs -= 1;
    }
}

/// The app command dlisten takes (`app output <n>`, from dtray's menu):
/// output `n` of the `count` it offers. An error says what it takes.
pub fn parse_command(words: &str, count: usize) -> Result<usize, String> {
    match words.split_ascii_whitespace().collect::<Vec<_>>()[..] {
        ["output", n] => {
            let n: usize = n
                .parse()
                .map_err(|_| format!("output needs a number, not {n:?}"))?;
            if n < count {
                Ok(n)
            } else {
                Err(format!("no output {n} (there are {count})"))
            }
        }
        _ => Err("dlisten takes: output <n>".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use dlink::SubStatus;
    use dstack::device::Subscription;

    use crate::player::{Player, fifo_for};

    fn snap() -> Snapshot {
        Snapshot {
            depth_ms: 40.06,
            ppm: 3.24,
            underruns: 2,
            played: 480_000,
            out_rate: 44_100,
            overflows: 0,
            recentres: 0,
            priming: false,
        }
    }

    #[test]
    fn the_line_carries_depth_ppm_underruns_frames_and_rate() {
        assert_eq!(
            line(&snap()),
            "depth 40.1 ms, +3.2 ppm, 2 underruns, 480000 frames played at 44100 Hz"
        );
        let all = Snapshot {
            ppm: -12.0,
            priming: true,
            overflows: 128,
            recentres: 1,
            ..snap()
        };
        assert_eq!(
            line(&all),
            "depth 40.1 ms, -12.0 ppm, 2 underruns, 480000 frames played at 44100 Hz, \
             waiting for audio, 128 frames overflowed, 1 re-centres"
        );
    }

    #[test]
    fn the_state_warns_while_priming_or_on_new_underruns() {
        assert_eq!(state(&snap(), 2), "ok");
        assert_eq!(state(&snap(), 1), "warn");
        assert_eq!(state(&snap(), 3), "ok");
        let priming = Snapshot {
            priming: true,
            ..snap()
        };
        assert_eq!(state(&priming, 2), "warn");
    }

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("Output {i}")).collect()
    }

    #[test]
    fn the_app_status_is_one_json_object_dtray_reads() {
        let outs = names(2);
        let v = AppView {
            snap: &snap(),
            underruns_before: 1,
            flowing: true,
            outputs: &outs,
            current: 1,
        };
        let text = app_status(&v);
        assert_eq!(
            dstack::ctl::app_object(&text),
            Some(text.clone()),
            "dstack takes it"
        );
        let j: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            j["line"],
            "Output 1 (44100 Hz): audio flowing, depth 40.1 ms, 2 underruns"
        );
        assert_eq!(j["state"], "warn");
        assert_eq!(j["flowing"], true);
        assert_eq!(j["depth_ms"], 40.1);
        assert_eq!(j["ppm"], 3.2);
        assert_eq!(j["underruns"], 2);
        assert_eq!(j["played"], 480_000);
        assert_eq!(j["out_rate"], 44_100);
        assert_eq!(j["output"], 1);
        assert_eq!(j["outputs"], serde_json::json!(["Output 0", "Output 1"]));
        assert_eq!(j.get("outputs_cut"), None);
        let quiet = AppView {
            flowing: false,
            current: 5,
            ..v
        };
        assert_eq!(
            short_line(&quiet),
            "no output (44100 Hz): no audio, depth 40.1 ms, 2 underruns"
        );
    }

    #[test]
    fn a_long_output_list_is_cut_to_fit_the_status() {
        assert_eq!(APP_MAX, dstack::ctl::APP_MAX);
        let outs: Vec<String> = (0..200)
            .map(|i| format!("{i:03} {}", "x".repeat(40)))
            .collect();
        let v = AppView {
            snap: &snap(),
            underruns_before: 2,
            flowing: true,
            outputs: &outs,
            current: 0,
        };
        let text = app_status(&v);
        assert!(text.len() <= APP_MAX, "{}", text.len());
        assert_eq!(dstack::ctl::app_object(&text), Some(text.clone()));
        let j: serde_json::Value = serde_json::from_str(&text).unwrap();
        let kept = j["outputs"].as_array().unwrap().len();
        assert!(kept > 50 && kept < 200, "{kept}");
        assert_eq!(j["outputs_cut"], true);
        // As many as fit: the next name (in quotes, after a comma) would not.
        assert!(
            text.len() + outs[kept].len() + 3 > APP_MAX,
            "{}",
            text.len()
        );
        // Nothing fits: no outputs at all, still one object.
        let huge = vec!["y".repeat(5000)];
        let text = app_status(&AppView {
            outputs: &huge,
            ..v
        });
        let j: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(j["outputs"], serde_json::json!([]));
    }

    #[test]
    fn the_command_is_output_and_an_index_it_offers() {
        assert_eq!(parse_command("output 0", 2), Ok(0));
        assert_eq!(parse_command(" output  1 ", 2), Ok(1));
        assert_eq!(
            parse_command("output 2", 2),
            Err("no output 2 (there are 2)".into())
        );
        assert_eq!(
            parse_command("output x", 2),
            Err("output needs a number, not \"x\"".into())
        );
        for bad in ["", "output", "volume 3", "output 1 2"] {
            assert_eq!(
                parse_command(bad, 2),
                Err("dlisten takes: output <n>".into()),
                "{bad}"
            );
        }
    }

    fn ch(status: Option<SubStatus>, written: u64) -> RxChannel {
        RxChannel {
            rx: 1,
            subscription: Some(Subscription {
                tx_channel: "Out".into(),
                tx_device: "tx".into(),
            }),
            status,
            flow: None,
            packets: 0,
            written,
        }
    }

    #[test]
    fn audio_flows_when_a_receiving_channel_wrote_since_the_last_reading() {
        let rx = Some(SubStatus::Receiving);
        assert!(flowing(&[10, 0], &[ch(rx, 11), ch(None, 0)]));
        assert!(flowing(&[10, 0], &[ch(None, 0), ch(rx, 1)]), "any channel");
        assert!(!flowing(&[10], &[ch(rx, 10)]), "stopped");
        assert!(
            !flowing(&[0], &[ch(Some(SubStatus::Unresolved), 50)]),
            "written but not connected: the pump's silence"
        );
        assert!(flowing(&[], &[ch(rx, 1)]), "a first reading");
        assert!(!flowing(&[], &[]));
        assert_eq!(written(&[ch(None, 4), ch(None, 9)]), [4, 9]);
    }

    #[test]
    fn a_reading_takes_the_fifo_and_the_player_s_counts() {
        let fifo = Arc::new(fifo_for(2, 48_000));
        fifo.push_with(480, |_, _| 0.0);
        let player = Player::new(Arc::clone(&fifo), 48_000, 44_100, Arc::default()).unwrap();
        let s = Snapshot::read(&fifo, &player.stats(), 48_000);
        assert_eq!(
            s,
            Snapshot {
                depth_ms: 10.0,
                ppm: 0.0,
                underruns: 0,
                played: 0,
                out_rate: 44_100,
                overflows: 0,
                recentres: 0,
                priming: true,
            }
        );
        fifo.push_with(24_000, |_, _| 0.0);
        let full = Snapshot::read(&fifo, &player.stats(), 48_000);
        assert_eq!(full.overflows, 480);
        assert_eq!(full.depth_ms, 500.0);
    }
}
