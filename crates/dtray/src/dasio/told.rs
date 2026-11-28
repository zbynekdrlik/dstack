// SPDX-License-Identifier: MIT OR Apache-2.0

//! The latency an instance tells the DAW (dstack #512), as the settings
//! window shows it: computed from the values chosen in the window by
//! dasio's own formula (dasio `crates/dasio/src/lib.rs`, `latencies`),
//! with every term named, and the running instance's figures from its
//! status beside it. Pure, every platform.
//!
//! With B the DAW buffer (`block`), P the Dante receive latency, W the
//! app reserve (`work_margin`), S the spare buffers (the service's
//! `--spare-blocks`, `app.spare_blocks` of the status) and T the Dante
//! send latency (`tx_latency_ns`, else the receive latency), P and T in
//! whole samples as the RX plays them (`dstack::clock::latency_samples`):
//!
//! - input = B + P
//! - output = B + W + S x B + T
//!
//! A bridge instance hosts a local ASIO driver and adds its own
//! conversion terms, so no sum is shown for it: only what its status
//! reports.

use dstack::clock::latency_samples;

use super::choices::ms;
use super::{Field, Fields, app_field};

/// The terms of one instance's latency, samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terms {
    /// B: the DAW buffer.
    pub block: u64,
    /// P: the Dante receive latency.
    pub receive: u64,
    /// W: the app reserve.
    pub reserve: u64,
    /// S: the spare buffers, each one more B on the output.
    pub spare: u64,
    /// T: the Dante send latency.
    pub send: u64,
}

impl Terms {
    /// The terms of `f` with `spare` spare buffers: T is the receive
    /// latency when `f` sets no send latency (dasio's rule).
    pub fn of(f: &Fields, spare: u64) -> Terms {
        let rate = f.sample_rate;
        Terms {
            block: f.block,
            receive: latency_samples(f.latency_ns, rate),
            reserve: f.work_margin,
            spare,
            send: latency_samples(f.tx_latency_ns.unwrap_or(f.latency_ns), rate),
        }
    }

    /// The input latency the DAW is told: B + P.
    pub fn input(&self) -> u64 {
        self.block + self.receive
    }

    /// The output latency the DAW is told: B + W + S x B + T.
    pub fn output(&self) -> u64 {
        self.block + self.reserve + self.spare * self.block + self.send
    }
}

/// The input and output latency a running instance reports to the DAW,
/// samples, and its rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Running {
    pub input: u64,
    pub output: u64,
    pub rate: u32,
}

/// What a status line's app part reports: `app.latency_in`,
/// `app.latency_out` and `app.sample_rate` (`rate` when it names none);
/// `None` when either latency is missing (a bridge instance's status, an
/// older dasio).
pub fn running(status: &str, rate: u32) -> Option<Running> {
    let n = |key: &str| app_field(status, key)?.as_u64();
    Some(Running {
        input: n("latency_in")?,
        output: n("latency_out")?,
        rate: n("sample_rate")
            .and_then(|r| u32::try_from(r).ok())
            .unwrap_or(rate),
    })
}

/// The spare buffers a status line's app part reports
/// (`app.spare_blocks`).
pub fn spare(status: &str) -> Option<u64> {
    app_field(status, "spare_blocks")?.as_u64()
}

/// The grey hint beside a field: what it adds to the latency the DAW is
/// told (empty for a field that adds nothing).
pub fn hint(field: Field) -> &'static str {
    match field {
        Field::Iface => "the network adapter Dante runs on; adds no latency",
        Field::Block => "the buffer Ableton works in; adds to the input and the output",
        Field::WorkMargin => "extra time Ableton gets per buffer; adds to the output",
        Field::RxLatency => "Dante's delay to us; adds to the input",
        Field::TxLatency => {
            "our delay to Dante; adds to the output (when not set: the receive latency)"
        }
        _ => "",
    }
}

/// The hint beside the spare buffers.
pub const SPARE_HINT: &str =
    "each adds one DAW buffer to the output (dasio's service setting, read only)";

/// The spare buffers' value as the window shows it, from the instance's
/// status (`None`: not running): `1 (adds 0.67 ms)`, or why it is not
/// known.
pub fn spare_text(f: &Fields, status: Option<&str>) -> String {
    match status.map(spare) {
        Some(Some(n)) => format!("{n} (adds {} ms)", ms(n * f.block, f.sample_rate)),
        Some(None) => "not known (the status does not say)".to_string(),
        None => "not known (dasio is not running)".to_string(),
    }
}

/// The input line: `Input 2.33 ms = DAW buffer 0.67 ms + Dante receive
/// 1.67 ms`.
pub fn input_line(t: &Terms, rate: u32) -> String {
    format!(
        "Input {} ms = DAW buffer {} ms + Dante receive {} ms",
        ms(t.input(), rate),
        ms(t.block, rate),
        ms(t.receive, rate),
    )
}

/// The output line: `Output 7.00 ms = DAW buffer 0.67 ms + app reserve
/// 2.67 ms + spare buffers 0 x 0.67 ms + Dante send 3.67 ms`.
pub fn output_line(t: &Terms, rate: u32) -> String {
    format!(
        "Output {} ms = DAW buffer {} ms + app reserve {} ms + spare buffers {} x {} ms \
         + Dante send {} ms",
        ms(t.output(), rate),
        ms(t.block, rate),
        ms(t.reserve, rate),
        t.spare,
        ms(t.block, rate),
        ms(t.send, rate),
    )
}

/// `in 2.33 / out 7.00 ms` of a running instance.
fn running_text(r: &Running) -> String {
    format!(
        "in {} / out {} ms",
        ms(r.input, r.rate),
        ms(r.output, r.rate)
    )
}

/// The "Latency Ableton is told" group's text for `f` (the values chosen
/// in the window) and the instance's status line (`None`: not running).
/// A DAW instance: the input and output lines, a note when the spare
/// buffers are not known (counted as 0), and the running instance's
/// figures: the same, or different (the chosen values apply at the next
/// restart). A bridge instance: only what its status reports.
pub fn told_text(f: &Fields, status: Option<&str>) -> String {
    let now = status.and_then(|s| running(s, f.sample_rate));
    if f.bridge.is_some() {
        let reported = match &now {
            Some(r) => format!("dasio reports {} now.", running_text(r)),
            None if status.is_some() => {
                "dasio's status does not report it for a bridge instance.".to_string()
            }
            None => "dasio is not running.".to_string(),
        };
        return format!(
            "A bridge instance adds its own conversion to the ASIO driver, so no sum is \
             shown here. {reported}"
        );
    }
    let spare = status.and_then(spare);
    let t = Terms::of(f, spare.unwrap_or(0));
    let mut lines = vec![
        input_line(&t, f.sample_rate),
        output_line(&t, f.sample_rate),
    ];
    if spare.is_none() {
        lines.push("Spare buffers not known: counted as 0.".to_string());
    }
    lines.push(match &now {
        Some(r) if r.input == t.input() && r.output == t.output() => {
            format!("dasio tells Ableton the same now ({}).", running_text(r))
        }
        Some(r) => format!(
            "dasio tells Ableton {} now; the values above apply at its next restart.",
            running_text(r)
        ),
        None if status.is_some() => "dasio's status does not say what it tells Ableton.".into(),
        None => "dasio is not running: nothing to compare.".into(),
    });
    lines.join("\r\n")
}

#[cfg(test)]
mod tests;
