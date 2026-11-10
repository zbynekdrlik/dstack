// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output devices as dlisten offers them (dstack #139): the list for
//! `--list-outputs` and the choice for `--output`. Pure: the binary reads
//! the devices from cpal and passes their names in.

/// One output device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub name: String,
    /// The system's default output.
    pub default: bool,
    /// Its default config (rate, channels, format), as the line shows it.
    pub config: String,
}

/// The error for a PC with no output device (a CI runner, a server).
pub const NONE: &str = "no audio output device found (none is connected, or the audio \
                        service does not run)";

/// `--list-outputs`' text: one line per output, `*` before the default.
/// An error when there is none.
pub fn list_text(outputs: &[Output]) -> Result<String, String> {
    if outputs.is_empty() {
        return Err(NONE.to_string());
    }
    Ok(outputs
        .iter()
        .map(|o| {
            let mark = if o.default { '*' } else { ' ' };
            format!("{mark} {} ({})", o.name, o.config)
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The output to play on: the first whose name holds `wanted` (any case),
/// else (no `wanted`) the default one. An error names what is missing.
pub fn pick(outputs: &[Output], wanted: Option<&str>) -> Result<usize, String> {
    if outputs.is_empty() {
        return Err(NONE.to_string());
    }
    match wanted {
        Some(w) => {
            let w = w.to_lowercase();
            outputs
                .iter()
                .position(|o| o.name.to_lowercase().contains(&w))
                .ok_or_else(|| format!("no output's name holds {w:?} (see --list-outputs)"))
        }
        None => outputs
            .iter()
            .position(|o| o.default)
            .ok_or_else(|| "no default output (name one with --output)".to_string()),
    }
}

/// The output to play on at the start: `--output` (`wanted`) first, then
/// the one the tray's menu saved (`saved`, by its exact name), then the
/// default. A saved output that is gone falls back to the default, with
/// the note to log as the second value.
pub fn choose(
    outputs: &[Output],
    wanted: Option<&str>,
    saved: Option<&str>,
) -> Result<(usize, Option<String>), String> {
    if wanted.is_some() {
        return pick(outputs, wanted).map(|i| (i, None));
    }
    let Some(name) = saved else {
        return pick(outputs, None).map(|i| (i, None));
    };
    match outputs.iter().position(|o| o.name == name) {
        Some(i) => Ok((i, None)),
        None => pick(outputs, None).map(|i| {
            let note = format!(
                "the saved output {name:?} is not there: playing on the default, {:?}",
                outputs[i].name
            );
            (i, Some(note))
        }),
    }
}

/// Where the output in use is found in a newly read list ([`refind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refind {
    /// Still there, at `index`; `changed` when its config (rate, channels,
    /// format) is not what it was: its stream must be opened again.
    Same { index: usize, changed: bool },
    /// Gone: the default output (the first when none is the default), at
    /// this index, takes over.
    Default(usize),
    /// Gone, and the list is empty.
    Gone,
}

/// The output `old` in the new list `now`: by its exact name, else the
/// default, else the first ([`Refind`]).
pub fn refind(old: &Output, now: &[Output]) -> Refind {
    match now.iter().position(|o| o.name == old.name) {
        Some(index) => Refind::Same {
            index,
            changed: now[index].config != old.config,
        },
        None if now.is_empty() => Refind::Gone,
        None => Refind::Default(now.iter().position(|o| o.default).unwrap_or(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_output_in_use_is_found_again_by_name_or_falls_back_to_the_default() {
        let list = outputs();
        let phones = list[2].clone();
        // Moved in the list: found by name, unchanged.
        let moved = vec![list[2].clone(), list[1].clone()];
        assert_eq!(
            refind(&phones, &moved),
            Refind::Same {
                index: 0,
                changed: false
            }
        );
        // Its config changed (another mix format): to be opened again.
        let mut other = list.clone();
        other[2].config = "44100 Hz, 2 ch, f32".into();
        assert_eq!(
            refind(&phones, &other),
            Refind::Same {
                index: 2,
                changed: true
            }
        );
        // Unplugged: the default takes over.
        assert_eq!(refind(&phones, &list[..2]), Refind::Default(1));
        // No default: the first. Nothing at all: gone.
        let mut none = list[..2].to_vec();
        none[1].default = false;
        assert_eq!(refind(&phones, &none), Refind::Default(0));
        assert_eq!(refind(&phones, &[]), Refind::Gone);
    }

    fn outputs() -> Vec<Output> {
        let o = |name: &str, default| Output {
            name: name.into(),
            default,
            config: "48000 Hz, 2 ch, f32".into(),
        };
        vec![
            o("Speakers (Realtek)", false),
            o("HDMI Monitor", true),
            o("Headphones", false),
        ]
    }

    #[test]
    fn no_output_is_a_clear_error_never_a_panic() {
        assert_eq!(list_text(&[]), Err(NONE.to_string()));
        assert_eq!(pick(&[], None), Err(NONE.to_string()));
        assert_eq!(pick(&[], Some("x")), Err(NONE.to_string()));
        assert!(NONE.starts_with("no audio output device found"));
    }

    #[test]
    fn the_list_marks_the_default() {
        assert_eq!(
            list_text(&outputs()).unwrap(),
            "  Speakers (Realtek) (48000 Hz, 2 ch, f32)\n\
             * HDMI Monitor (48000 Hz, 2 ch, f32)\n  \
             Headphones (48000 Hz, 2 ch, f32)"
        );
    }

    #[test]
    fn an_output_is_picked_by_part_of_its_name_in_any_case() {
        assert_eq!(pick(&outputs(), Some("realtek")), Ok(0));
        assert_eq!(pick(&outputs(), Some("PHONES")), Ok(2));
        assert_eq!(
            pick(&outputs(), Some("o")),
            Ok(1),
            "the first that holds it"
        );
        assert_eq!(
            pick(&outputs(), Some("USB")),
            Err("no output's name holds \"usb\" (see --list-outputs)".into())
        );
    }

    #[test]
    fn the_command_line_wins_over_the_saved_output() {
        assert_eq!(
            choose(&outputs(), Some("phones"), Some("HDMI Monitor")),
            Ok((2, None))
        );
        assert!(choose(&outputs(), Some("USB"), Some("HDMI Monitor")).is_err());
    }

    #[test]
    fn the_saved_output_is_found_by_its_exact_name() {
        assert_eq!(choose(&outputs(), None, Some("Headphones")), Ok((2, None)));
        assert_eq!(choose(&outputs(), None, None), Ok((1, None)));
    }

    #[test]
    fn a_saved_output_that_is_gone_falls_back_to_the_default_with_a_note() {
        assert_eq!(
            choose(&outputs(), None, Some("USB DAC")),
            Ok((
                1,
                Some(
                    "the saved output \"USB DAC\" is not there: playing on the default, \
                     \"HDMI Monitor\""
                        .into()
                )
            ))
        );
        // Not by part of its name: the saved name is the whole name.
        assert_eq!(choose(&outputs(), None, Some("Head")).unwrap().0, 1);
        assert_eq!(choose(&[], None, Some("x")), Err(NONE.to_string()));
    }

    #[test]
    fn without_a_name_the_default_is_picked() {
        assert_eq!(pick(&outputs(), None), Ok(1));
        let mut none = outputs();
        none[1].default = false;
        assert_eq!(
            pick(&none, None),
            Err("no default output (name one with --output)".into())
        );
    }
}
