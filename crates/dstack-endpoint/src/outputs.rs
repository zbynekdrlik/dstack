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

/// The output a binary takes when neither `--output` nor a saved choice
/// names one ([`choose`]). Each binary states its own: dlisten plays where
/// Windows plays ([`Fallback::Default`]), dsend captures its own output
/// ([`DSEND`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// The system's default output.
    Default,
    /// The first output whose name holds this (any case), else the
    /// system's default.
    Prefer(&'static str),
}

/// dsend's fallback: its own render endpoint (`drivers/dsend-audio`, dstack
/// #184), which Windows lists as "Speakers (dsend)", over the default, so
/// dsend never captures the PC's real speakers while its own output exists.
pub const DSEND: Fallback = Fallback::Prefer("dsend");

/// The output [`Fallback`] gives, and how a note names it.
fn fall_back(outputs: &[Output], fallback: Fallback) -> Result<(usize, String), String> {
    if let Fallback::Prefer(part) = fallback {
        let part = part.to_lowercase();
        if let Some(i) = outputs
            .iter()
            .position(|o| o.name.to_lowercase().contains(&part))
        {
            return Ok((i, format!("the output holding {part:?}")));
        }
    }
    pick(outputs, None).map(|i| (i, "the default".to_string()))
}

/// The output to play on at the start: `--output` (`wanted`) first, then
/// the one the tray's menu saved (`saved`, by its exact name), then the
/// binary's [`Fallback`]. A saved output that is gone falls back to the
/// same, with the note to log as the second value.
pub fn choose(
    outputs: &[Output],
    wanted: Option<&str>,
    saved: Option<&str>,
    fallback: Fallback,
) -> Result<(usize, Option<String>), String> {
    if wanted.is_some() {
        return pick(outputs, wanted).map(|i| (i, None));
    }
    let Some(name) = saved else {
        return fall_back(outputs, fallback).map(|(i, _)| (i, None));
    };
    match outputs.iter().position(|o| o.name == name) {
        Some(i) => Ok((i, None)),
        None => fall_back(outputs, fallback).map(|(i, which)| {
            let note = format!(
                "the saved output {name:?} is not there: playing on {which}, {:?}",
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
            choose(
                &outputs(),
                Some("phones"),
                Some("HDMI Monitor"),
                Fallback::Default
            ),
            Ok((2, None))
        );
        assert!(
            choose(
                &outputs(),
                Some("USB"),
                Some("HDMI Monitor"),
                Fallback::Default
            )
            .is_err()
        );
    }

    #[test]
    fn the_saved_output_is_found_by_its_exact_name() {
        assert_eq!(
            choose(&outputs(), None, Some("Headphones"), Fallback::Default),
            Ok((2, None))
        );
        assert_eq!(
            choose(&outputs(), None, None, Fallback::Default),
            Ok((1, None))
        );
    }

    #[test]
    fn a_saved_output_that_is_gone_falls_back_to_the_default_with_a_note() {
        assert_eq!(
            choose(&outputs(), None, Some("USB DAC"), Fallback::Default),
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
        assert_eq!(
            choose(&outputs(), None, Some("Head"), Fallback::Default)
                .unwrap()
                .0,
            1
        );
        assert_eq!(
            choose(&[], None, Some("x"), Fallback::Default),
            Err(NONE.to_string())
        );
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

    /// The list with dsend's own output (drivers/dsend-audio) last, not the
    /// default: Windows still plays on the HDMI monitor.
    fn with_dsend() -> Vec<Output> {
        let mut list = outputs();
        list.push(Output {
            name: "Speakers (DSEND)".into(),
            default: false,
            config: "48000 Hz, 2 ch, f32".into(),
        });
        list
    }

    #[test]
    fn dsend_takes_its_own_output_over_the_default_unless_one_is_named() {
        // Nothing named or saved: its own output (any case), not the default.
        assert_eq!(choose(&with_dsend(), None, None, DSEND), Ok((3, None)));
        // --output and the saved choice still win.
        assert_eq!(
            choose(&with_dsend(), Some("phones"), None, DSEND),
            Ok((2, None))
        );
        assert_eq!(
            choose(&with_dsend(), None, Some("HDMI Monitor"), DSEND),
            Ok((1, None))
        );
        // A saved output that is gone falls back to dsend's own output too.
        assert_eq!(
            choose(&with_dsend(), None, Some("USB DAC"), DSEND),
            Ok((
                3,
                Some(
                    "the saved output \"USB DAC\" is not there: playing on the output \
                     holding \"dsend\", \"Speakers (DSEND)\""
                        .into()
                )
            ))
        );
    }

    #[test]
    fn without_its_own_output_dsend_takes_the_default() {
        assert_eq!(choose(&outputs(), None, None, DSEND), Ok((1, None)));
        assert_eq!(
            choose(&outputs(), None, Some("USB DAC"), DSEND)
                .unwrap()
                .1
                .unwrap(),
            "the saved output \"USB DAC\" is not there: playing on the default, \
             \"HDMI Monitor\""
        );
        assert_eq!(choose(&[], None, None, DSEND), Err(NONE.to_string()));
    }

    #[test]
    fn dlisten_never_takes_the_dsend_output_by_default() {
        // dlisten's fallback is the system default, even when the dsend
        // output exists (it would play into nothing).
        assert_eq!(
            choose(&with_dsend(), None, None, Fallback::Default),
            Ok((1, None))
        );
        assert_eq!(
            choose(&with_dsend(), None, Some("USB DAC"), Fallback::Default)
                .unwrap()
                .0,
            1
        );
    }
}
