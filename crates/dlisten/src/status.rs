// SPDX-License-Identifier: MIT OR Apache-2.0

//! dlisten's status (dstack #139): the log line every 10 s and the app
//! part of the device's status, which dtray shows after the device's own
//! line (`Link::set_app_status`; `line`, and `state` "ok" or "warn").

use dlink::{RxChannel, SubState};

pub use dstack_endpoint::status::{
    APP_MAX, NAME_MAX, Snapshot, line, state, status_name, with_outputs, with_outputs_of,
};

/// Whether audio flows: a channel whose subscription is receiving
/// (connected) wrote samples since the last reading (`before`, each
/// channel's `written`, [`written`]). The output's RX pull writes silence into
/// the FIFO either way; a lost flow shows as a count that stops.
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
    /// The output devices' names, in the order `output <n>` counts them
    /// (the status carries each as [`status_name`] gives it).
    pub outputs: &'a [String],
    /// The one playing: `None` while none plays (a named output missing,
    /// dstack #506).
    pub current: Option<usize>,
    /// The `--output` text a run waits for while no output matches it
    /// (dstack #506): the line says so and the state is "warn".
    pub waiting_for: Option<&'a str>,
    /// The command line's `--output` text (dstack #507), kept after a
    /// choice in dtray named the run after another output: dtray warns
    /// when the output playing is not the one it names (the next start
    /// plays on that one again).
    pub pinned: Option<&'a str>,
}

/// dtray's line for dlisten: `<output> (48000 Hz): audio flowing, depth
/// 40.1 ms, 2 underruns`; while it waits for its named output (dstack
/// #506): `waiting for an output holding "Headphones" (--output): nothing
/// plays, audio flowing`.
pub fn short_line(v: &AppView<'_>) -> String {
    let audio = if v.flowing {
        "audio flowing"
    } else {
        "no audio"
    };
    if let Some(text) = v.waiting_for {
        return format!(
            "waiting for an output holding {:?} (--output): nothing plays, {audio}",
            status_name(text)
        );
    }
    let output = v
        .current
        .and_then(|c| v.outputs.get(c))
        .map_or_else(|| "no output".to_string(), |o| status_name(o));
    format!(
        "{output} ({} Hz): {audio}, depth {:.1} ms, {} underruns",
        v.snap.out_rate, v.snap.depth_ms, v.snap.underruns
    )
}

/// The app part of the device's status for dtray (`Link::set_app_status`):
/// one JSON object with dtray's `line` ([`short_line`]) and `state`
/// ([`state`]), the numbers, and the outputs dtray's Output menu offers:
/// `outputs` (their names) and `output` (the index of the one playing; the
/// index is what `app output <n>` names; left out while none plays, and
/// then `waiting_for` names the `--output` text waited for), and `pinned`,
/// the command line's `--output` text (dstack #507; left out without one).
/// Within [`APP_MAX`]: outputs past
/// it are left off the list (`outputs_cut`: true), the one playing kept
/// while it fits.
pub fn app_status(v: &AppView<'_>) -> String {
    let s = v.snap;
    let state = if v.waiting_for.is_some() {
        "warn"
    } else {
        state(s, v.underruns_before)
    };
    let mut base = serde_json::json!({
        "line": short_line(v),
        "state": state,
        "flowing": v.flowing,
        "depth_ms": (s.depth_ms * 10.0).round() / 10.0,
        "ppm": (s.ppm * 10.0).round() / 10.0,
        "underruns": s.underruns,
        "convert_errors": s.convert_errors,
        "output_gaps": s.output_gaps,
        "output_lost_ms": (s.output_lost_ms * 10.0).round() / 10.0,
        "played": s.played,
        "out_rate": s.out_rate,
    });
    if let Some(text) = v.waiting_for {
        base["waiting_for"] = status_name(text).into();
    }
    if let Some(text) = v.pinned {
        base["pinned"] = status_name(text).into();
    }
    with_outputs_of(&base, v.outputs, v.current)
}

/// The app command dlisten takes (`app output <n> [<name>]`, from dtray's
/// menu): [`dstack_endpoint::status::parse_command`] for dlisten.
pub fn parse_command(words: &str, outputs: &[String]) -> Result<usize, String> {
    dstack_endpoint::status::parse_command("dlisten", words, outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    use dlink::SubStatus;
    use dstack::device::Subscription;

    fn snap() -> Snapshot {
        Snapshot {
            depth_ms: 40.06,
            target_ms: 11.333333,
            ppm: 3.24,
            underruns: 2,
            played: 480_000,
            out_rate: 44_100,
            overflows: 0,
            recentres: 0,
            resyncs: 0,
            convert_errors: 3,
            output_gaps: 4,
            output_lost_ms: 61.5,
            priming: false,
            mono: false,
            input_peak: None,
        }
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
            current: Some(1),
            waiting_for: None,
            pinned: None,
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
        assert_eq!(j["convert_errors"], 3, "dstack #303");
        assert_eq!(j["output_gaps"], 4, "dstack #307");
        assert_eq!(j["output_lost_ms"], 61.5, "dstack #307");
        assert_eq!(j["played"], 480_000);
        assert_eq!(j["out_rate"], 44_100);
        assert_eq!(j["output"], 1);
        assert_eq!(j["outputs"], serde_json::json!(["Output 0", "Output 1"]));
        assert_eq!(j.get("outputs_cut"), None);
        assert_eq!(j.get("pinned"), None, "no --output, no key");
        // A run started with --output keeps its text after another choice
        // (dstack #507): dtray tells the two apart.
        let chosen = AppView {
            pinned: Some("Output 0"),
            ..v
        };
        let j: serde_json::Value = serde_json::from_str(&app_status(&chosen)).unwrap();
        assert_eq!(j["pinned"], "Output 0");
        assert_eq!(j["output"], 1);
        let quiet = AppView {
            flowing: false,
            current: Some(5),
            ..v
        };
        assert_eq!(
            short_line(&quiet),
            "no output (44100 Hz): no audio, depth 40.1 ms, 2 underruns"
        );
    }

    #[test]
    fn a_run_waiting_for_its_named_output_says_so_and_offers_the_list() {
        let outs = vec!["Speakers (Realtek(R) Audio)".to_string()];
        // No new underrun: the state would be "ok" while playing.
        let v = AppView {
            snap: &snap(),
            underruns_before: 2,
            flowing: true,
            outputs: &outs,
            current: None,
            waiting_for: Some("Headphones"),
            pinned: Some("Headphones"),
        };
        let j: serde_json::Value = serde_json::from_str(&app_status(&v)).unwrap();
        assert_eq!(
            j["line"],
            "waiting for an output holding \"Headphones\" (--output): nothing plays, audio \
             flowing"
        );
        assert_eq!(j["state"], "warn");
        assert_eq!(j["waiting_for"], "Headphones");
        assert_eq!(j["pinned"], "Headphones", "dstack #507: the --output text");
        assert_eq!(j.get("output"), None, "no menu item checked");
        assert_eq!(
            j["outputs"],
            serde_json::json!(["Speakers (Realtek(R) Audio)"])
        );
        let quiet = AppView {
            flowing: false,
            ..v
        };
        assert!(short_line(&quiet).ends_with("nothing plays, no audio"));
        // Playing again: no waiting key, the state from the counts.
        let playing = AppView {
            current: Some(0),
            waiting_for: None,
            ..v
        };
        let j: serde_json::Value = serde_json::from_str(&app_status(&playing)).unwrap();
        assert_eq!(j["state"], "ok");
        assert_eq!(j.get("waiting_for"), None);
        assert_eq!(j["output"], 0);
        // Nothing playing and nothing waited for: "no output".
        let none = AppView {
            current: None,
            waiting_for: None,
            ..v
        };
        assert!(short_line(&none).starts_with("no output (44100 Hz)"));
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
            current: Some(0),
            waiting_for: None,
            pinned: None,
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
            current: Some(150),
            waiting_for: None,
            pinned: None,
        };
        let j: serde_json::Value = serde_json::from_str(&app_status(&v)).unwrap();
        // 151 names of 44 bytes do not fit 4096 bytes: the list cannot be
        // cut short of the playing one, so it is dropped whole.
        assert_eq!(j["outputs"], serde_json::json!([]));
        assert_eq!(j["output"], 150);
        let few = AppView {
            current: Some(20),
            ..v
        };
        let j: serde_json::Value = serde_json::from_str(&app_status(&few)).unwrap();
        let kept = j["outputs"].as_array().unwrap();
        assert!(kept.len() > 21, "{}", kept.len());
        assert_eq!(kept[20], outs[20].as_str());
        assert_eq!(j["outputs_cut"], true);
        // At the edge: the last playing index the list still reaches keeps
        // exactly up to it; one more and the list is dropped.
        let len = |current: usize| {
            let j: serde_json::Value = serde_json::from_str(&app_status(&AppView {
                current: Some(current),
                ..v
            }))
            .unwrap();
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
            "written but not connected: the pull's silence"
        );
        assert!(flowing(&[], &[ch(rx, 1)]), "a first reading");
        assert!(!flowing(&[], &[]));
        assert_eq!(written(&[ch(None, 4), ch(None, 9)]), [4, 9]);
    }
}
