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

/// The most bytes of an output's name in the status (and in dtray's
/// request back): a longer one is cut at a character's edge.
pub const NAME_MAX: usize = 96;

/// An output's name as the status carries it: each control character a
/// space, cut to [`NAME_MAX`] bytes at a character's edge.
pub fn status_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if out.len() + c.len_utf8() > NAME_MAX {
            break;
        }
        out.push(if c.is_control() { ' ' } else { c });
    }
    out
}

/// What dtray shows of dlisten.
#[derive(Debug, Clone, Copy)]
pub struct AppView<'a> {
    pub snap: &'a Snapshot,
    /// The underruns of the last reading.
    pub underruns_before: u64,
    pub flowing: bool,
    /// The output devices' names, in the order `output <n>` counts them
    /// (the status carries each as [`status_name`] gives it).
    pub outputs: &'a [String],
    /// The one playing.
    pub current: usize,
}

/// dtray's line for dlisten: `<output> (48000 Hz): audio flowing, depth
/// 40.1 ms, 2 underruns`.
pub fn short_line(v: &AppView<'_>) -> String {
    let output = v
        .outputs
        .get(v.current)
        .map_or_else(|| "no output".to_string(), |o| status_name(o));
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
/// it are left off the list (`outputs_cut`: true), the one playing kept
/// while it fits.
pub fn app_status(v: &AppView<'_>) -> String {
    let s = v.snap;
    let mut outputs = v.outputs.len();
    let names: Vec<String> = v.outputs.iter().map(|o| status_name(o)).collect();
    // The list is cut from its end, down to the one playing; past that
    // (it alone does not fit), to none.
    let keep = (v.current + 1).min(outputs);
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
            "outputs": names[..outputs],
        });
        if outputs < v.outputs.len() {
            obj["outputs_cut"] = serde_json::Value::Bool(true);
        }
        let text = obj.to_string();
        if text.len() <= APP_MAX || outputs == 0 {
            return text;
        }
        outputs = if outputs > keep { outputs - 1 } else { 0 };
    }
}

/// The words of a name, single-spaced (a request's words lose their
/// spacing).
fn spaced(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// The app command dlisten takes (`app output <n> [<name>]`, from dtray's
/// menu): output `n` of the `outputs` it offers (their status names).
/// With the name (its [`status_name`]), the list must still have it at `n`:
/// a list that changed
/// since dtray read it is refused, never another device chosen. An error
/// says what it takes.
pub fn parse_command(words: &str, outputs: &[String]) -> Result<usize, String> {
    let usage = || "dlisten takes: output <n> [<name>]".to_string();
    let mut parts = words.split_ascii_whitespace();
    if parts.next() != Some("output") {
        return Err(usage());
    }
    let n = parts.next().ok_or_else(usage)?;
    let n: usize = n
        .parse()
        .map_err(|_| format!("output needs a number, not {n:?}"))?;
    let name = parts.collect::<Vec<_>>().join(" ");
    let Some(have) = outputs.get(n) else {
        return Err(format!("no output {n} (there are {})", outputs.len()));
    };
    if !name.is_empty() && spaced(&status_name(have)) != name {
        return Err(format!(
            "output {n} is now {have:?}, not {name:?}: the list changed, choose again"
        ));
    }
    Ok(n)
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
        // A long name is cut ([`status_name`]) in the list and the line:
        // one object within the bound.
        let huge = vec!["y".repeat(5000)];
        let text = app_status(&AppView {
            outputs: &huge,
            ..v
        });
        assert!(text.len() <= APP_MAX, "{}", text.len());
        assert!(dstack::ctl::app_object(&text).is_some());
        let j: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(j["outputs"], serde_json::json!(["y".repeat(NAME_MAX)]));
        let line = j["line"].as_str().unwrap();
        assert!(
            line.starts_with(&format!("{} (", "y".repeat(NAME_MAX))),
            "{line}"
        );
    }

    #[test]
    fn a_cut_list_keeps_the_one_playing() {
        let outs: Vec<String> = (0..200)
            .map(|i| format!("{i:03} {}", "x".repeat(40)))
            .collect();
        let v = AppView {
            snap: &snap(),
            underruns_before: 2,
            flowing: true,
            outputs: &outs,
            current: 150,
        };
        let j: serde_json::Value = serde_json::from_str(&app_status(&v)).unwrap();
        // 151 names of 44 bytes do not fit 4096 bytes: the list cannot be
        // cut short of the playing one, so it is dropped whole.
        assert_eq!(j["outputs"], serde_json::json!([]));
        assert_eq!(j["output"], 150);
        let few = AppView { current: 20, ..v };
        let j: serde_json::Value = serde_json::from_str(&app_status(&few)).unwrap();
        let kept = j["outputs"].as_array().unwrap();
        assert!(kept.len() > 21, "{}", kept.len());
        assert_eq!(kept[20], outs[20].as_str());
        assert_eq!(j["outputs_cut"], true);
        // At the edge: the last playing index the list still reaches keeps
        // exactly up to it; one more and the list is dropped.
        let len = |current: usize| {
            let j: serde_json::Value =
                serde_json::from_str(&app_status(&AppView { current, ..v })).unwrap();
            j["outputs"].as_array().unwrap().len()
        };
        let edge = (0..200).find(|&c| len(c) == 0).unwrap();
        assert!(edge > 50, "{edge}");
        assert_eq!(len(edge - 1), edge, "cut down to the playing one");
    }

    #[test]
    fn the_command_is_output_and_an_index_it_offers() {
        let outs = vec!["Speakers (USB  Audio)".to_string(), "HDMI".to_string()];
        assert_eq!(parse_command("output 0", &outs), Ok(0));
        assert_eq!(parse_command(" output  1 ", &outs), Ok(1));
        assert_eq!(
            parse_command("output 2", &outs),
            Err("no output 2 (there are 2)".into())
        );
        assert_eq!(
            parse_command("output x", &outs),
            Err("output needs a number, not \"x\"".into())
        );
        for bad in ["", "output", "volume 3", "outputs 1"] {
            assert_eq!(
                parse_command(bad, &outs),
                Err("dlisten takes: output <n> [<name>]".into()),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_named_choice_must_still_be_at_its_index() {
        let outs = vec!["Speakers (USB  Audio)".to_string(), "HDMI".to_string()];
        // The request's words lose the double space; the name still fits.
        assert_eq!(parse_command("output 0 Speakers (USB Audio)", &outs), Ok(0));
        assert_eq!(parse_command("output 1 HDMI", &outs), Ok(1));
        assert_eq!(
            parse_command("output 1 USB DAC", &outs),
            Err("output 1 is now \"HDMI\", not \"USB DAC\": the list changed, choose again".into())
        );
    }

    #[test]
    fn a_status_name_is_one_line_within_its_bound() {
        assert_eq!(status_name("Line\nOut\t2"), "Line Out 2");
        assert_eq!(status_name(&"x".repeat(200)).len(), NAME_MAX);
        let mut edge = "x".repeat(NAME_MAX - 1);
        edge.push('é');
        assert_eq!(
            status_name(&edge).len(),
            NAME_MAX - 1,
            "a character is never split"
        );
        let mut fits = "x".repeat(NAME_MAX - 2);
        fits.push('é');
        assert_eq!(status_name(&fits).len(), NAME_MAX);
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
