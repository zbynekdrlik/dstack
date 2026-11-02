// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! `card`: a device's channel labels and name, read and written by its IP
//! on the ARC port ([`crate::arc::control`]), with iemmixer's
//! `dante_card.py` contract (its netaudio helper, which this replaces):
//!
//! ```text
//! card read <ip> <expected-name>            stdout: {name, rx_count, tx_count, tx: {"<n>": label}, rx: {...}}
//! card write <ip> <expected-name> <flag>    stdin: [{type: "tx"|"rx", channel, label}];
//!                                           stdout: {sent, failed, error, read_back, mismatches, tx, rx}
//! card rename <ip> <expected-name> <new>    stdout: {renamed, name, error}
//! ```
//!
//! Before anything is written, the device at `<ip>` must name itself
//! `<expected-name>` and read completely (a name, and every TX and RX
//! channel its counts promise). A label equal to the channel's number
//! resets it to the default. Once writing starts the verb always ends with
//! one JSON line and exit 0: `<flag>` (an absolute path) appearing stops
//! the writes; the first write the device refuses or does not answer stops
//! them (`failed`; stricter than netaudio, which counted an unanswered
//! write as sent); then a fresh read says what the device holds. Exit 1: no answer, another device, an incomplete
//! read or a channel it lacks (nothing written); 2: usage. Nothing but the
//! labels and (rename) the device's own name of that one device is ever
//! changed.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::arc::control::{self, Call, Dir, Labels};
use crate::device::{valid_channel_name, valid_device_name};

/// The highest channel number a change may name (dante_card.py's).
pub const MAX_CHANNEL: u16 = 255;

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    Read,
    /// The flag file whose appearance stops the writes.
    Write(std::path::PathBuf),
    /// The new name.
    Rename(String),
}

/// A parsed `card` command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub ip: Ipv4Addr,
    pub expected: String,
    pub verb: Verb,
}

/// One change of the write verb's stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub channel: u16,
    pub label: String,
}

impl Change {
    fn dir(&self) -> Dir {
        if self.kind == "tx" { Dir::Tx } else { Dir::Rx }
    }
}

/// The usage line.
pub const USAGE: &str = "usage: card read <ip> <expected-name> | write <ip> <expected-name> <flag> | rename <ip> <expected-name> <new-name>";

/// One IPv4 address of one host: not multicast, loopback, unspecified or
/// reserved (240/4, the broadcast address); a link-local address stays
/// allowed (a Dante card takes one when no DHCP answers).
fn unicast(text: &str) -> Option<Ipv4Addr> {
    let ip: Ipv4Addr = text.parse().ok()?;
    let reserved = ip.octets()[0] >= 240;
    (!(ip.is_multicast() || ip.is_loopback() || ip.is_unspecified() || reserved)).then_some(ip)
}

/// The `card` arguments (after the word `card`).
pub fn parse(args: &[String]) -> Result<Card, String> {
    let verb = args.first().map(String::as_str);
    let ok_len = matches!(
        (verb, args.len()),
        (Some("read"), 3) | (Some("write" | "rename"), 4)
    );
    if !ok_len {
        return Err(USAGE.to_string());
    }
    let ip = unicast(&args[1]).ok_or("<ip> is not a unicast IPv4 address")?;
    let expected = args[2].clone();
    // Python's isprintable(): no control or format character, no
    // whitespace but the space.
    let printable = |c: char| {
        !c.is_control()
            && (c == ' ' || !c.is_whitespace())
            && !matches!(c, '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{feff}')
    };
    if expected.is_empty() || !expected.chars().all(printable) {
        return Err("<expected-name> is empty or not printable".into());
    }
    let verb = match verb {
        Some("read") => Verb::Read,
        Some("write") => {
            let flag = std::path::PathBuf::from(&args[3]);
            if !flag.is_absolute() {
                return Err("<flag> is not an absolute path".into());
            }
            Verb::Write(flag)
        }
        _ => {
            let new = args[3].clone();
            if !valid_device_name(&new) || new == expected {
                return Err("<new-name> is not a Dante device name (1 to 31 letters, digits and hyphens, no hyphen at either end) other than <expected-name>".into());
            }
            Verb::Rename(new)
        }
    };
    Ok(Card { ip, expected, verb })
}

/// The write verb's stdin: a non-empty list of `{type, channel, label}`,
/// each a label Dante holds on a channel it numbers, no channel twice.
pub fn parse_changes(text: &str) -> Result<Vec<Change>, String> {
    let doc: Value = serde_json::from_str(text).map_err(|_| "stdin is not JSON")?;
    let list = doc
        .as_array()
        .filter(|l| !l.is_empty())
        .ok_or("stdin must be a non-empty list of {type, channel, label}")?;
    let mut out = Vec::with_capacity(list.len());
    for c in list {
        let obj = c
            .as_object()
            .filter(|o| {
                o.len() == 3
                    && ["type", "channel", "label"]
                        .iter()
                        .all(|k| o.contains_key(*k))
            })
            .ok_or("each change is exactly {type, channel, label}")?;
        let kind = match obj["type"].as_str() {
            Some("tx") => "tx",
            Some("rx") => "rx",
            _ => {
                return Err(format!(
                    "a change names no tx/rx channel (1 to {MAX_CHANNEL})"
                ));
            }
        };
        let channel = obj["channel"]
            .as_u64()
            .filter(|&n| (1..=u64::from(MAX_CHANNEL)).contains(&n))
            .and_then(|n| u16::try_from(n).ok())
            .ok_or(format!(
                "a change names no tx/rx channel (1 to {MAX_CHANNEL})"
            ))?;
        let label = obj["label"]
            .as_str()
            .filter(|l| valid_channel_name(l))
            .ok_or(format!(
                "{kind} {channel}: not a label Dante holds (1 to 31 printable ASCII characters, none of = . @)"
            ))?;
        out.push(Change {
            kind,
            channel,
            label: label.to_string(),
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    if !out.iter().all(|c| seen.insert((c.kind, c.channel))) {
        return Err("a channel is named twice".into());
    }
    Ok(out)
}

/// Why nothing was written: exit 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

/// The device's name, after it named itself `expected` and read
/// completely, and its labels.
fn connect(c: &mut impl Call, expected: &str) -> Result<Labels, Refused> {
    let name =
        control::device_name(c).map_err(|e| Refused(format!("the card did not answer ({e})")))?;
    if name != expected {
        return Err(Refused(
            "the device answering at the configured host is not the expected one".into(),
        ));
    }
    let labels =
        control::labels(c).map_err(|e| Refused(format!("incomplete read of the card ({e})")))?;
    if !labels.complete() {
        let kind = if labels.tx_count == 0 || labels.tx.len() != labels.tx_count {
            "tx"
        } else {
            "rx"
        };
        return Err(Refused(format!(
            "incomplete read of the card's {kind} channels"
        )));
    }
    Ok(labels)
}

/// `card read`'s line (keys in dante_card.py's order, labels by number).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadOut {
    pub name: String,
    pub rx_count: usize,
    pub tx_count: usize,
    pub tx: BTreeMap<u16, String>,
    pub rx: BTreeMap<u16, String>,
}

/// A sent or failed change whose label did not read back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mismatch {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub channel: u16,
    pub want: String,
    pub got: Option<String>,
}

/// `card write`'s line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WriteOut {
    /// The changes the device accepted (status OK), in order.
    pub sent: Vec<Change>,
    /// The change that was refused or got no answer, which ended the writes.
    pub failed: Option<Change>,
    pub error: Option<String>,
    pub read_back: bool,
    pub mismatches: Vec<Mismatch>,
    pub tx: Option<BTreeMap<u16, String>>,
    pub rx: Option<BTreeMap<u16, String>>,
}

/// `card rename`'s line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenameOut {
    pub renamed: bool,
    pub name: Option<String>,
    pub error: Option<String>,
}

/// `card read`.
pub fn read(c: &mut impl Call, expected: &str) -> Result<ReadOut, Refused> {
    let labels = connect(c, expected)?;
    Ok(ReadOut {
        name: expected.to_string(),
        rx_count: labels.rx_count,
        tx_count: labels.tx_count,
        tx: labels.tx,
        rx: labels.rx,
    })
}

fn joined(errors: Vec<String>) -> Option<String> {
    (!errors.is_empty()).then(|| errors.join("; "))
}

/// `card write`: the changes in order until `flag` appears or one is
/// refused (or unanswered), then the read back.
pub fn write(
    c: &mut impl Call,
    expected: &str,
    flag: &Path,
    changes: &[Change],
) -> Result<WriteOut, Refused> {
    let held =
        connect(c, expected).map_err(|e| Refused(format!("{}: nothing was written", e.0)))?;
    if let Some(m) = changes
        .iter()
        .find(|ch| held.get(ch.dir(), ch.channel).is_none())
    {
        return Err(Refused(format!(
            "the card has no {} channel {}: nothing was written",
            m.kind, m.channel
        )));
    }
    let mut sent: Vec<Change> = Vec::new();
    let mut failed: Option<Change> = None;
    let mut errors = Vec::new();
    for ch in changes {
        if flag.exists() {
            errors.push(format!(
                "{} appeared: writing stopped after {} of {} labels",
                flag.display(),
                sent.len(),
                changes.len()
            ));
            break;
        }
        let label = (ch.label != ch.channel.to_string()).then_some(ch.label.as_str());
        match control::rename_channel(c, ch.dir(), ch.channel, label) {
            Ok(()) => sent.push(ch.clone()),
            Err(e) => {
                errors.push(format!(
                    "writing {} {} failed ({e}): writing stopped after {} of {} labels",
                    ch.kind,
                    ch.channel,
                    sent.len(),
                    changes.len()
                ));
                failed = Some(ch.clone());
                break;
            }
        }
    }
    let after = match connect(c, expected) {
        Ok(l) => Some(l),
        Err(e) => {
            errors.push(format!("not read back: {}", e.0));
            None
        }
    };
    let mut mismatches = Vec::new();
    if let Some(after) = &after {
        for ch in sent.iter().chain(&failed) {
            // dante_card.py's rule: what reads back must be the label sent
            // (a card's default label is its number, so a reset reads as
            // the number too).
            let got = after.get(ch.dir(), ch.channel);
            if got != Some(ch.label.as_str()) {
                mismatches.push(Mismatch {
                    kind: ch.kind,
                    channel: ch.channel,
                    want: ch.label.clone(),
                    got: got.map(str::to_string),
                });
            }
        }
    }
    let (tx, rx) = match after {
        Some(a) => (Some(a.tx), Some(a.rx)),
        None => (None, None),
    };
    Ok(WriteOut {
        sent,
        failed,
        error: joined(errors),
        read_back: tx.is_some(),
        mismatches,
        tx,
        rx,
    })
}

/// `card rename`: the new name, then the name the device holds.
pub fn rename(c: &mut impl Call, expected: &str, new: &str) -> Result<RenameOut, Refused> {
    connect(c, expected).map_err(|e| Refused(format!("{}: nothing was renamed", e.0)))?;
    let mut errors = Vec::new();
    if let Err(e) = control::rename_device(c, new) {
        errors.push(format!("renaming failed ({e})"));
    }
    let name = control::device_name(c).ok();
    match &name {
        None => errors.push("not read back: the card did not answer".into()),
        Some(n) if n != new && errors.is_empty() => {
            errors.push("the card does not hold the new name".into())
        }
        Some(_) => {}
    }
    Ok(RenameOut {
        renamed: name.as_deref() == Some(new),
        name,
        error: joined(errors),
    })
}

#[cfg(test)]
mod tests;
