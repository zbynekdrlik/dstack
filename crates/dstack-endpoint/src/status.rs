// SPDX-License-Identifier: MIT OR Apache-2.0

//! The status parts dlisten and dsend share (dstack #139, #164): the
//! [`Player`](crate::player::Player)'s reading, its log line, dtray's
//! `state`, the endpoint list in the app status (cut to the status's bound)
//! and the `app output <n> [<name>]` command dtray's Output menu sends.

use crate::fifo::Fifo;
use crate::player::Stats;

/// One reading of the player's side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    /// The FIFO's depth, ms of its producer's rate.
    pub depth_ms: f64,
    /// The converter's correction, ppm.
    pub ppm: f64,
    pub underruns: u64,
    /// Frames the player gave its consumer.
    pub played: u64,
    /// The consumer's rate, Hz.
    pub out_rate: u32,
    /// Producer frames dropped on a full FIFO.
    pub overflows: u64,
    pub recentres: u64,
    /// The consumer waits for the FIFO to fill (silence: no audio yet, or a
    /// refill after an underrun).
    pub priming: bool,
    /// Every channel summed to both speakers (`--mono`).
    pub mono: bool,
    /// The largest magnitude the FIFO's producer stored since the last
    /// log line ([`Fifo::take_peak`], set by [`Snapshot::with_peak`]);
    /// `None` in a reading that leaves it to the log line.
    pub input_peak: Option<f32>,
}

impl Snapshot {
    /// The reading of `fifo` (at its producer's `fifo_rate`) and the
    /// player's `stats`.
    pub fn read(fifo: &Fifo, stats: &Stats, fifo_rate: u32) -> Self {
        Snapshot {
            depth_ms: fifo.len() as f64 * 1000.0 / f64::from(fifo_rate.max(1)),
            ppm: stats.ppm(),
            underruns: stats.underruns(),
            played: stats.played(),
            out_rate: stats.out_rate(),
            overflows: fifo.overflows(),
            recentres: stats.recentres(),
            priming: stats.priming(),
            mono: stats.mono(),
            input_peak: None,
        }
    }

    /// This reading with the FIFO's input peak taken (the log line's: one
    /// taker, so the peak covers the time since the last line).
    #[must_use]
    pub fn with_peak(self, fifo: &Fifo) -> Self {
        Snapshot {
            input_peak: Some(fifo.take_peak()),
            ..self
        }
    }
}

/// `, input peak -20.0 dBFS`, or `, input silent` for a peak of 0.
fn peak_text(peak: f32) -> String {
    if peak > 0.0 {
        format!(", input peak {:.1} dBFS", 20.0 * f64::from(peak).log10())
    } else {
        ", input silent".to_string()
    }
}

/// The status line: `depth 40.1 ms, +3.2 ppm, 0 underruns, 480000 frames
/// played at 48000 Hz`, then `, mono` under `--mono`, `, waiting for audio` while priming and the
/// overflows and re-centres when there are any.
pub fn line(s: &Snapshot) -> String {
    let mut text = format!(
        "depth {:.1} ms, {:+.1} ppm, {} underruns, {} frames played at {} Hz",
        s.depth_ms, s.ppm, s.underruns, s.played, s.out_rate
    );
    if s.mono {
        text.push_str(", mono");
    }
    if s.priming {
        text.push_str(", waiting for audio");
    }
    if s.overflows > 0 {
        text.push_str(&format!(", {} frames overflowed", s.overflows));
    }
    if s.recentres > 0 {
        text.push_str(&format!(", {} re-centres", s.recentres));
    }
    if let Some(peak) = s.input_peak {
        text.push_str(&peak_text(peak));
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

/// The most bytes of an endpoint's name in the status (and in dtray's
/// request back): a longer one is cut at a character's edge.
pub const NAME_MAX: usize = 96;

/// An endpoint's name as the status carries it: each control character a
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

/// The app status's text: the object `base` with the endpoints dtray's
/// Output menu offers, `outputs` (their [`status_name`]s) and `output` (the
/// index `current` of the one in use; the index is what `app output <n>`
/// names). Within [`APP_MAX`]: endpoints past it are left off the list
/// (`outputs_cut`: true), the one in use kept while it fits.
pub fn with_outputs(base: &serde_json::Value, outputs: &[String], current: usize) -> String {
    let names: Vec<String> = outputs.iter().map(|o| status_name(o)).collect();
    let mut shown = names.len();
    // The list is cut from its end, down to the one in use; past that (it
    // alone does not fit), to none.
    let keep = (current + 1).min(shown);
    loop {
        let mut obj = base.clone();
        obj["output"] = current.into();
        obj["outputs"] = names[..shown].into();
        if shown < names.len() {
            obj["outputs_cut"] = serde_json::Value::Bool(true);
        }
        let text = obj.to_string();
        if text.len() <= APP_MAX || shown == 0 {
            return text;
        }
        shown = if shown > keep { shown - 1 } else { 0 };
    }
}

/// The words of a name, single-spaced (a request's words lose their
/// spacing).
fn spaced(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// The app command `app` takes (`app output <n> [<name>]`, from dtray's
/// menu): endpoint `n` of the `outputs` it offers (their status names).
/// With the name (its [`status_name`]), the list must still have it at `n`:
/// a list that changed since dtray read it is refused, never another
/// endpoint chosen. An error says what it takes.
pub fn parse_command(app: &str, words: &str, outputs: &[String]) -> Result<usize, String> {
    let usage = || format!("{app} takes: output <n> [<name>]");
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
            mono: false,
            input_peak: None,
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
            mono: true,
            priming: true,
            overflows: 128,
            recentres: 1,
            ..snap()
        };
        assert_eq!(
            line(&all),
            "depth 40.1 ms, -12.0 ppm, 2 underruns, 480000 frames played at 44100 Hz, \
             mono, waiting for audio, 128 frames overflowed, 1 re-centres"
        );
    }

    /// dstack #184: the log line says what the producer hands over, so a
    /// capture of silence shows in dsend's own log.
    #[test]
    fn the_line_carries_the_input_peak_in_dbfs_or_silence() {
        let at = |p: f32| {
            line(&Snapshot {
                input_peak: Some(p),
                ..snap()
            })
        };
        let base = line(&snap());
        assert_eq!(at(0.1), format!("{base}, input peak -20.0 dBFS"));
        assert_eq!(at(1.0), format!("{base}, input peak 0.0 dBFS"));
        assert_eq!(at(0.0), format!("{base}, input silent"));
        assert!(!base.contains("input"));
        let fifo = Fifo::new(2, 64);
        fifo.push_with(4, |f, c| if f == 2 && c == 1 { -0.5 } else { 0.25 });
        let s = Snapshot::read(&fifo, &Stats::default(), 48_000);
        assert_eq!(s.input_peak, None, "a plain reading leaves the peak");
        assert_eq!(s.with_peak(&fifo).input_peak, Some(0.5));
        assert_eq!(s.with_peak(&fifo).input_peak, Some(0.0), "taken once");
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

    #[test]
    fn the_outputs_join_the_base_object() {
        let base = serde_json::json!({"line": "x"});
        let outs = vec!["A".to_string(), "B\nC".to_string()];
        let text = with_outputs(&base, &outs, 1);
        assert_eq!(dstack::ctl::app_object(&text), Some(text.clone()));
        let j: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(j["line"], "x");
        assert_eq!(j["output"], 1);
        assert_eq!(j["outputs"], serde_json::json!(["A", "B C"]));
        assert_eq!(j.get("outputs_cut"), None);
    }

    #[test]
    fn a_long_list_is_cut_down_to_the_one_in_use_or_dropped() {
        assert_eq!(APP_MAX, dstack::ctl::APP_MAX);
        let base = serde_json::json!({"line": "x"});
        let outs: Vec<String> = (0..200)
            .map(|i| format!("{i:03} {}", "x".repeat(40)))
            .collect();
        let text = with_outputs(&base, &outs, 0);
        assert!(text.len() <= APP_MAX, "{}", text.len());
        let j: serde_json::Value = serde_json::from_str(&text).unwrap();
        let kept = j["outputs"].as_array().unwrap().len();
        assert!(kept > 50 && kept < 200, "{kept}");
        assert_eq!(j["outputs_cut"], true);
        assert!(text.len() + outs[kept].len() + 3 > APP_MAX);
        let j: serde_json::Value = serde_json::from_str(&with_outputs(&base, &outs, 150)).unwrap();
        assert_eq!(j["outputs"], serde_json::json!([]));
        assert_eq!(j["output"], 150);
    }

    #[test]
    fn the_command_is_output_and_an_index_it_offers() {
        let outs = vec!["Speakers (USB  Audio)".to_string(), "HDMI".to_string()];
        assert_eq!(parse_command("dsend", "output 0", &outs), Ok(0));
        assert_eq!(parse_command("dsend", " output  1 ", &outs), Ok(1));
        assert_eq!(
            parse_command("dsend", "output 2", &outs),
            Err("no output 2 (there are 2)".into())
        );
        assert_eq!(
            parse_command("dsend", "output x", &outs),
            Err("output needs a number, not \"x\"".into())
        );
        for bad in ["", "output", "volume 3", "outputs 1"] {
            assert_eq!(
                parse_command("dsend", bad, &outs),
                Err("dsend takes: output <n> [<name>]".into()),
                "{bad}"
            );
        }
        assert_eq!(
            parse_command("dsend", "output 0 Speakers (USB Audio)", &outs),
            Ok(0)
        );
        assert_eq!(
            parse_command("dsend", "output 1 USB DAC", &outs),
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
                mono: false,
                input_peak: None,
            }
        );
        let player = player.with_mono(true);
        assert!(Snapshot::read(&fifo, &player.stats(), 48_000).mono);
        fifo.push_with(24_000, |_, _| 0.0);
        let full = Snapshot::read(&fifo, &player.stats(), 48_000);
        assert_eq!(full.overflows, 480);
        assert_eq!(full.depth_ms, 500.0);
    }
}
