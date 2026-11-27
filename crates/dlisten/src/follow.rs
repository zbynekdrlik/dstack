// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which output a run plays on, read after read of the output list (dstack
//! #506). Pure: the session reads the devices from cpal and passes their
//! names in; every decision here is tested with made-up lists.
//!
//! A run that names its output (`--output <text>`, [`Want::Named`]) plays
//! on that output only. While no output matches it plays nothing and
//! waits ([`Step::Wait`]); it opens the output when it appears
//! ([`Step::Open`]); it never falls back to another output (on 26.11.2026
//! the headphones of the sound engineer's SOLO listen were pulled out for
//! three minutes, the run fell to the notebook's speakers and stayed there
//! for the rest of the service). A run without `--output` ([`Want::Free`])
//! keeps what it did before: the saved choice or the default at the start,
//! the output playing found again by name, the default when it goes.

use crate::outputs::{self, Output, Refind};

/// What the run plays on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// `--output <text>`: only an output whose name is the text or holds
    /// it (any case); none other, ever.
    Named(String),
    /// No `--output`: the saved choice or the default at the start, then
    /// the output playing, the default when it goes.
    Free,
}

impl Want {
    /// The want of a run's `--output`.
    pub fn of(output: Option<&str>) -> Want {
        output.map_or(Want::Free, |t| Want::Named(t.to_string()))
    }
}

/// What the session does with a newly read output list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The output playing is still there, at `index`; `reopen` when its
    /// config (rate, channels, format) changed: its stream is opened again.
    Keep { index: usize, reopen: bool },
    /// Play on the output at `index` (it appeared, or it takes over).
    Open(usize),
    /// Play nothing: no output there may play.
    Wait,
}

/// The output to play on at the start, and a note to log: `Some(index)`,
/// or `None` to wait (nothing is there that may play). An error only when
/// a [`Want::Free`] run finds no output at all.
pub fn start(
    list: &[Output],
    want: &Want,
    saved: Option<&str>,
) -> Result<(Option<usize>, Option<String>), String> {
    match want {
        Want::Named(text) => Ok((matching(list, text), None)),
        Want::Free => outputs::choose(list, None, saved, outputs::Fallback::Default)
            .map(|(i, note)| (Some(i), note)),
    }
}

/// The output a [`Want::Named`] run may play on in `list`: the first whose
/// name is `text` (any case), else the first whose name holds it.
pub fn matching(list: &[Output], text: &str) -> Option<usize> {
    let text = text.to_lowercase();
    list.iter()
        .position(|o| o.name.to_lowercase() == text)
        .or_else(|| {
            list.iter()
                .position(|o| o.name.to_lowercase().contains(&text))
        })
}

/// What to do with the new list `now`, `playing` the output that plays
/// (the last one played, for a [`Want::Free`] run).
pub fn next(want: &Want, playing: Option<&Output>, now: &[Output]) -> Step {
    if let Want::Named(text) = want {
        // The output playing, while it is listed by its exact name (a run
        // re-named by dtray's menu names it whole, so it still matches).
        if let Some(p) = playing
            && let Some(index) = now.iter().position(|o| o.name == p.name)
        {
            return Step::Keep {
                index,
                reopen: now[index].config != p.config,
            };
        }
        return matching(now, text).map_or(Step::Wait, Step::Open);
    }
    match playing {
        Some(p) => match outputs::refind(p, now) {
            Refind::Same { index, changed } => Step::Keep {
                index,
                reopen: changed,
            },
            Refind::Default(index) => Step::Open(index),
            Refind::Gone => Step::Wait,
        },
        None if now.is_empty() => Step::Wait,
        None => Step::Open(now.iter().position(|o| o.default).unwrap_or(0)),
    }
}

/// A line the session logs about a missing named output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Warn(String),
    Info(String),
}

/// The log of a named output that is missing: a WARN at once, then a line
/// a minute while it stays missing ([`MissingLog::due`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingLog {
    text: String,
    lines: u64,
}

impl MissingLog {
    /// The log of a wait for the output named `text`.
    pub fn new(text: &str) -> MissingLog {
        MissingLog {
            text: text.to_string(),
            lines: 0,
        }
    }

    /// The line due `missing_s` seconds after the output went missing (the
    /// session asks at each read of the list, every 5 s): the WARN at the
    /// first ask, then one INFO line at each new whole minute (a late ask
    /// gives one line, never one per minute missed).
    pub fn due(&mut self, missing_s: u64) -> Option<Line> {
        let minutes = missing_s / 60;
        if self.lines == 0 {
            self.lines = minutes + 1;
            return Some(Line::Warn(format!(
                "dlisten: no output holding {:?} is there (--output): playing nothing until it \
                 comes, never on another output",
                self.text
            )));
        }
        if minutes < self.lines {
            return None;
        }
        self.lines = minutes + 1;
        Some(Line::Info(format!(
            "dlisten: still waiting for an output holding {:?} (missing {minutes} min): playing \
             nothing",
            self.text
        )))
    }

    /// The line when the output `name` is there again after `missing_s`
    /// seconds.
    pub fn found(&self, name: &str, missing_s: u64) -> String {
        let (m, s) = (missing_s / 60, missing_s % 60);
        let after = if m == 0 {
            format!("{s} s")
        } else {
            format!("{m} min {s} s")
        };
        format!("dlisten: the output {name:?} is there again after {after}: playing on it")
    }
}

#[cfg(test)]
mod tests;
