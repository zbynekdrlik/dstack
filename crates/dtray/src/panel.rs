// SPDX-License-Identifier: MIT OR Apache-2.0

//! dtray's panel (dstack #507): the window a left click on the tray icon
//! opens on Windows. Pure, every platform; the binary's `panel_win.rs` is
//! the Win32 glue that only calls it.
//!
//! - [`place`]: where the window goes: next to the tray icon, always inside
//!   the work area of the monitor the icon is on (the screen less the
//!   taskbar), its height capped to it, and a scroll bar when the content
//!   is taller. Before, a click opened the native menu, which Windows
//!   places against the whole monitor: on the FOH notebook (1920x1200 at
//!   125 %) dlisten's Output submenu ran under the taskbar and its checked
//!   output was cut off.
//! - [`rows`]: what the panel shows: a row per device (its name, colour
//!   and line), and for an app that offers outputs (dlisten) the output it
//!   actually plays on, a warning when it plays nothing or another output
//!   than its command line's `--output` names, and its output list for the
//!   one picker ([`crate::output`]: a pick sends `app output <n> <name>`,
//!   which the app saves).

use serde_json::Value;

use crate::discover::Reading;
use crate::model::{Colour, View, app_line};
use crate::output::{self, OutputChoice};

/// A rectangle in a monitor's physical pixels, `right` and `bottom`
/// excluded (as Windows' `RECT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// Where the panel goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The window's rectangle, inside the work area.
    pub rect: Rect,
    /// The content is taller than the window: it scrolls.
    pub scroll: bool,
}

/// The panel's window for content of `want_w` x `want_h` pixels, next to
/// the tray icon at `anchor`, inside `work` (the work area of the icon's
/// monitor), `gap` pixels from the icon. It is never wider or taller than
/// the work area; taller content scrolls. Beside a taskbar at the side
/// (the icon left or right of the work area) it sits next to that
/// taskbar, its bottom at the icon's; otherwise above the icon when the
/// room above is the larger, else below, its right edge at the icon's.
/// Then it is moved inside the work area.
pub fn place(work: Rect, anchor: Rect, want_w: i32, want_h: i32, gap: i32) -> Placement {
    let w = want_w.clamp(1, work.width().max(1));
    let h = want_h.clamp(1, work.height().max(1));
    let (left, top) = if anchor.left >= work.right {
        // A taskbar on the right.
        (anchor.left - gap - w, anchor.bottom - h)
    } else if anchor.right <= work.left {
        // A taskbar on the left.
        (anchor.right + gap, anchor.bottom - h)
    } else {
        let above = anchor.top - work.top;
        let below = work.bottom - anchor.bottom;
        let top = if above >= below {
            anchor.top - gap - h
        } else {
            anchor.bottom + gap
        };
        (anchor.right - w, top)
    };
    // Never `clamp`: a work area under one pixel would make its bounds
    // cross (the window then keeps the work area's corner).
    let left = left.min(work.right - w).max(work.left);
    let top = top.min(work.bottom - h).max(work.top);
    Placement {
        rect: Rect {
            left,
            top,
            right: left + w,
            bottom: top + h,
        },
        scroll: want_h > h,
    }
}

/// The width the panel's text wraps in: the window's width as [`place`]
/// gives it for a window `want_w` wide inside `work` (never wider than the
/// work area), less `chrome` (the scroll bar and the borders) and a
/// `margin` on each side; at least 1. On a work area narrower than the
/// panel the text wraps narrower too, so no line runs past the window
/// (dstack #507).
pub fn text_width(work: Rect, want_w: i32, chrome: i32, margin: i32) -> i32 {
    let w = want_w.clamp(1, work.width().max(1));
    w.saturating_sub(chrome)
        .saturating_sub(margin.saturating_mul(2))
        .max(1)
}

/// `dips` (device-independent pixels, 96 a logical inch) in the pixels of
/// a monitor at `dpi` (96 = 100 %, 120 = 125 %), rounded.
pub fn scaled(dips: i32, dpi: u32) -> i32 {
    let px = (i64::from(dips) * i64::from(dpi) + 48).div_euclid(96);
    i32::try_from(px).unwrap_or(i32::MAX)
}

/// The scroll position after moving `by` pixels from `pos`, for content
/// `content` pixels high in a window `page` pixels high: from 0 to the
/// content's height less the page, never past it, and 0 when it fits.
pub fn scroll_to(pos: i32, by: i32, content: i32, page: i32) -> i32 {
    let max = content.saturating_sub(page).max(0);
    pos.saturating_add(by).clamp(0, max)
}

/// One device's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub colour: Colour,
    /// Its line without the name in front.
    pub line: String,
    /// An app that offers outputs: what it plays on and its picker.
    pub output: Option<OutputRow>,
}

/// An app's output part of its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRow {
    /// `Plays on: <the output>`, or `Plays on: nothing`.
    pub plays: String,
    /// Why the operator should look: it plays nothing, or not on the
    /// output its `--output` names.
    pub warning: Option<String>,
    /// The picker's items ([`output::choices`]).
    pub choices: Vec<OutputChoice>,
    /// The item of the output playing, an index into `choices`.
    pub selected: Option<usize>,
}

/// The panel's rows: one per reading (`view` is [`crate::model::Tracker`]'s
/// view of `readings`, its lines and colours in their order), then one
/// per expected device not running (the view's lines after them). A row's
/// line is the view's `whole` one (the app's part never cut, dstack #507;
/// the panel wraps it), else its menu line.
pub fn rows(readings: &[Reading], view: &View) -> Vec<Row> {
    let mut out = Vec::with_capacity(view.lines.len());
    for (i, menu) in view.lines.iter().enumerate() {
        let line = view.whole.get(i).unwrap_or(menu);
        let colour = view.colours.get(i).copied().unwrap_or(Colour::Red);
        let (name, output) = match readings.get(i) {
            Some((name, reading)) => (
                name.clone(),
                reading.as_ref().ok().and_then(|s| output_row(s)),
            ),
            None => (
                line.split_once(": ")
                    .map_or(line.as_str(), |(n, _)| n)
                    .to_string(),
                None,
            ),
        };
        let line = line
            .strip_prefix(&format!("{name}: "))
            .unwrap_or(line)
            .to_string();
        out.push(Row {
            name,
            colour,
            line,
            output,
        });
    }
    out
}

/// The output part of a status line: `None` unless its app part has an
/// `outputs` list.
pub fn output_row(status: &str) -> Option<OutputRow> {
    let v: Value = serde_json::from_str(status).ok()?;
    let app = v.get("app")?;
    let list = app.get("outputs")?.as_array()?;
    // The raw texts decide (dlisten matches whole names); the shown ones
    // are cut for display ([`app_line`]).
    let raw = |key: &str| {
        app.get(key)
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty())
    };
    let playing = app
        .get("output")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .and_then(|n| list.get(n))
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty());
    let choices = output::choices(status);
    let selected = choices.iter().position(|c| c.checked);
    let waiting = raw("waiting_for");
    let pinned = raw("pinned");
    let warning = match (playing, waiting, pinned) {
        // Waiting for the command line's --output.
        (_, Some(t), Some(p)) if t == p => Some(format!(
            "Plays nothing: no output holding \"{}\" (its --output) is there; \
             it plays as soon as one comes",
            app_line(t)
        )),
        // Waiting for an output picked in dtray (the run is named after
        // it until the next start).
        (_, Some(t), p) => Some(format!(
            "Plays nothing: the output \"{}\" chosen in dtray is not there; \
             it plays as soon as it comes{}",
            app_line(t),
            p.map(|p| format!(" (a restart plays on \"{}\")", app_line(p)))
                .unwrap_or_default()
        )),
        (None, None, _) => Some("Plays nothing: no output is open".to_string()),
        (Some(name), None, Some(p)) if !holds(name, p) => {
            let p = app_line(p);
            Some(format!(
                "Not the --output \"{p}\" output: a restart plays on \"{p}\" again"
            ))
        }
        _ => None,
    };
    let playing = playing.map(app_line);
    let plays = format!("Plays on: {}", playing.as_deref().unwrap_or("nothing"));
    Some(OutputRow {
        plays,
        warning,
        choices,
        selected,
    })
}

/// Whether the output `name` is one a run's `--output <text>` may play
/// on: its name is the text or holds it, any case (dlisten's own rule,
/// `follow::matching`).
fn holds(name: &str, text: &str) -> bool {
    name.to_lowercase().contains(&text.to_lowercase())
}

/// What a row's controls are built from: its name and, for an app with
/// outputs, the picker's labels. The panel builds its controls again only
/// when this changes; otherwise it sets their texts.
pub fn shape(rows: &[Row]) -> Vec<(String, Option<Vec<String>>)> {
    rows.iter()
        .map(|r| {
            let picker = r
                .output
                .as_ref()
                .map(|o| o.choices.iter().map(|c| c.label.clone()).collect());
            (r.name.clone(), picker)
        })
        .collect()
}

/// A colour's word in the row's title.
pub fn colour_word(c: Colour) -> &'static str {
    match c {
        Colour::Grey => "no device",
        Colour::Green => "ok",
        Colour::Orange => "warning",
        Colour::Red => "error",
    }
}

#[cfg(test)]
mod tests;
