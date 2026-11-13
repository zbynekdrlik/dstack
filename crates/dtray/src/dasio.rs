// SPDX-License-Identifier: MIT OR Apache-2.0

//! dasio's instance settings (dstack #246; the window approved as mockup
//! v1, `docs/mockups/dtray-dasio-settings-v1.html`). Pure, every platform:
//! the Windows window (`settings_win.rs` in the binary) and the elevated
//! helper (`dtray-cli --dasio-apply`) only call these.
//!
//! An instance is a folder `instance-N` under `%ProgramData%\dasio`
//! ([`root`]) holding its `device.toml`; its Windows service is `dasio-N`
//! ([`Instance::service`]). dasio reads the file as dstack's
//! [`DeviceConfig`] after taking out its own `iface` key (the Windows
//! interface alias the device binds to, dasio#101), so [`Fields::read`]
//! does the same.
//!
//! - [`Fields`]: what the window shows and edits; [`edit`] writes a change
//!   into the file's text with `toml_edit`, so the admin's comments, the
//!   key order and every other key stay.
//! - [`changes`] and [`Field::kind`]: which fields changed and whether each
//!   applies at once (the RX latency, through the device's ARC as
//!   `dtray --set-latency` writes it) or needs a restart of the service
//!   (everything the file holds).
//! - [`busy_cpus`] and [`cpu_problems`]: the other instances' CPUs are
//!   greyed and refused.
//! - [`resize`]: a channel list at a new count, the names kept.
//! - [`plan`]: what an apply does (the live write, the new file, whether
//!   the DAW must release the driver first).
//! - [`banner`], [`status_line`], [`daw_attached`], [`verify`]: the
//!   window's texts and the apply's checks against the device's status;
//!   [`Restart`] and [`shown_status`]: "Restarting…" while an apply's
//!   restart keeps the device away.
//! - [`install`]: the elevated helper's write of a new file into an
//!   instance's folder.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::ops::Sub;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dstack::device::DeviceConfig;
use serde_json::Value;
use toml_edit::{Array, DocumentMut, Item, Value as TomlValue};

use crate::latency;
use crate::model;

/// dasio's folder under `%ProgramData%`.
pub const DIR: &str = "dasio";
/// An instance folder's name before its number.
pub const INSTANCE_PREFIX: &str = "instance-";
/// An instance's config file.
pub const CONFIG: &str = "device.toml";
/// dasio's own key: the Windows interface alias (dasio#101).
pub const IFACE_KEY: &str = "iface";
/// The fewest channels each way the window offers.
pub const MIN_CHANNELS: usize = 2;
/// The most channels each way dasio carries.
pub const MAX_CHANNELS: usize = 64;
/// The file the helper writes before it renames it over [`CONFIG`].
pub const STAGED: &str = "device.toml.dtray-new";

/// dasio's folder: `<program_data>\dasio`, `C:\ProgramData` when Windows
/// names none.
pub fn root_for(program_data: Option<OsString>) -> PathBuf {
    program_data
        .filter(|d| !d.is_empty())
        .map_or_else(
            || PathBuf::from(crate::expected::PROGRAM_DATA),
            PathBuf::from,
        )
        .join(DIR)
}

/// This PC's dasio folder ([`root_for`] with the ProgramData folder: on
/// Windows the system's known folder, not the environment, which a
/// program of the user could point elsewhere for the elevated helper).
pub fn root() -> PathBuf {
    #[cfg(windows)]
    let program_data = system::program_data();
    #[cfg(not(windows))]
    let program_data = std::env::var_os("ProgramData");
    root_for(program_data)
}

/// One dasio instance on this PC.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instance {
    pub number: u32,
    pub dir: PathBuf,
}

impl Instance {
    /// Its Windows service: `dasio-N`.
    pub fn service(&self) -> String {
        service_name(self.number)
    }

    /// Its config file.
    pub fn config(&self) -> PathBuf {
        self.dir.join(CONFIG)
    }
}

/// The service of instance `n`: `dasio-N`.
pub fn service_name(n: u32) -> String {
    format!("dasio-{n}")
}

/// The instance number of a folder name: `instance-N` with N plain ASCII
/// digits (no sign, no space); `None` for anything else.
pub fn instance_number(name: &str) -> Option<u32> {
    let digits = name.strip_prefix(INSTANCE_PREFIX)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The instances under `root`: each `instance-N` folder that holds a
/// [`CONFIG`], by number. A missing or unreadable `root` has none.
pub fn instances(root: &Path) -> Vec<Instance> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<Instance> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let number = instance_number(e.file_name().to_str()?)?;
            let dir = e.path();
            dir.join(CONFIG)
                .is_file()
                .then_some(Instance { number, dir })
        })
        .collect();
    out.sort();
    out
}

/// The three CPU pickers: the audio pump, the send (TX) thread and the
/// receive (RX) thread; `None` is "any" (the config leaves it out).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cpus {
    pub pump: Option<usize>,
    pub io: Option<usize>,
    pub rx: Option<usize>,
}

impl Cpus {
    /// The CPUs named, each once.
    pub fn named(&self) -> BTreeSet<usize> {
        [self.pump, self.io, self.rx]
            .into_iter()
            .flatten()
            .collect()
    }
}

/// What the window shows of one instance's config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fields {
    /// The device's name (read only here).
    pub name: String,
    /// The interface alias, when the file names one.
    pub iface: Option<String>,
    /// The buffer: the pump's block in frames.
    pub block: u64,
    /// The RX (device) latency, ns.
    pub latency_ns: u64,
    /// The TX latency, ns; `None` follows the RX latency.
    pub tx_latency_ns: Option<u64>,
    /// Hz (read only: the network's).
    pub sample_rate: u32,
    /// The RX and TX channel names.
    pub rx: Vec<String>,
    pub tx: Vec<String>,
    pub cpus: Cpus,
}

impl Fields {
    /// The fields of a config's text, checked as dasio reads it: the
    /// `iface` key (a string) taken out, the rest a valid
    /// [`DeviceConfig`]; the reason otherwise.
    pub fn read(text: &str) -> Result<Fields, String> {
        let mut doc = parse_doc(text)?;
        let iface = match doc.remove(IFACE_KEY) {
            None => None,
            Some(item) => Some(
                item.as_str()
                    .ok_or_else(|| format!("{IFACE_KEY}: not a string"))?
                    .to_string(),
            ),
        };
        let cfg = DeviceConfig::from_toml(&doc.to_string()).map_err(|e| e.problems.join("; "))?;
        Ok(Fields {
            name: cfg.name,
            iface,
            block: cfg.block,
            latency_ns: cfg.latency_ns,
            tx_latency_ns: cfg.tx_latency_ns,
            sample_rate: cfg.sample_rate,
            rx: cfg.rx,
            tx: cfg.tx,
            cpus: Cpus {
                pump: cfg.pump_cpu,
                io: cfg.io_cpu,
                rx: cfg.rx_cpu,
            },
        })
    }
}

/// `text` as an editable TOML document; the reason (its first line)
/// otherwise.
fn parse_doc(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>()
        .map_err(|e| format!("not TOML: {}", one_line(&e.to_string())))
}

/// The first line of a message (a TOML error spans several).
fn one_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().trim().to_string()
}

/// `key` set to `v` in `doc`, the value's own decor (a comment after it on
/// its line) kept when the key was there.
fn set_value(doc: &mut DocumentMut, key: &str, v: TomlValue) {
    let mut v = v;
    if let Some(old) = doc.get(key).and_then(Item::as_value) {
        *v.decor_mut() = old.decor().clone();
    }
    doc[key] = Item::Value(v);
}

/// `key` set to `n` (an integer) in `doc`.
fn set_int(doc: &mut DocumentMut, key: &str, n: u64) -> Result<(), String> {
    let n = i64::try_from(n).map_err(|_| format!("{key}: {n} is too large"))?;
    set_value(doc, key, TomlValue::from(n));
    Ok(())
}

/// `key` set to `n`, or taken out for `None` (the config's default).
fn set_opt(doc: &mut DocumentMut, key: &str, n: Option<u64>) -> Result<(), String> {
    match n {
        Some(n) => set_int(doc, key, n),
        None => {
            doc.remove(key);
            Ok(())
        }
    }
}

/// `names` as a TOML array, one name a line (as dasio's configs list
/// them).
fn name_array(names: &[String]) -> Array {
    let mut a: Array = names.iter().map(String::as_str).collect();
    for v in a.iter_mut() {
        v.decor_mut().set_prefix("\n  ");
    }
    a.set_trailing_comma(true);
    a.set_trailing("\n");
    a
}

/// The config `text` with the fields that differ between `text`'s own and
/// `new` written (every other key, the comments and the order kept); the
/// result is read again and must give `new` exactly. The name and the
/// sample rate are not the window's to change.
pub fn edit(text: &str, new: &Fields) -> Result<String, String> {
    let old = Fields::read(text)?;
    if new.name != old.name {
        return Err("the device's name is not changed here".into());
    }
    if new.sample_rate != old.sample_rate {
        return Err("the sample rate is the network's, not changed here".into());
    }
    let mut doc = parse_doc(text)?;
    if new.iface != old.iface {
        match &new.iface {
            Some(alias) => set_value(&mut doc, IFACE_KEY, TomlValue::from(alias.as_str())),
            None => {
                doc.remove(IFACE_KEY);
            }
        }
    }
    if new.block != old.block {
        set_int(&mut doc, "block", new.block)?;
    }
    if new.latency_ns != old.latency_ns {
        set_int(&mut doc, "latency_ns", new.latency_ns)?;
    }
    if new.tx_latency_ns != old.tx_latency_ns {
        set_opt(&mut doc, "tx_latency_ns", new.tx_latency_ns)?;
    }
    if new.rx != old.rx {
        set_value(&mut doc, "rx", TomlValue::Array(name_array(&new.rx)));
    }
    if new.tx != old.tx {
        set_value(&mut doc, "tx", TomlValue::Array(name_array(&new.tx)));
    }
    let to_u64 = |c: Option<usize>| c.map(|c| c as u64);
    if new.cpus.pump != old.cpus.pump {
        set_opt(&mut doc, "pump_cpu", to_u64(new.cpus.pump))?;
    }
    if new.cpus.io != old.cpus.io {
        set_opt(&mut doc, "io_cpu", to_u64(new.cpus.io))?;
    }
    if new.cpus.rx != old.cpus.rx {
        set_opt(&mut doc, "rx_cpu", to_u64(new.cpus.rx))?;
    }
    let out = doc.to_string();
    let back = Fields::read(&out).map_err(|e| format!("the new settings are refused: {e}"))?;
    if back != *new {
        return Err("the new settings did not read back as chosen".into());
    }
    Ok(out)
}

/// `names` at `n` channels: the first names kept; a new channel is named
/// by its number ("17"), or, when another channel already has that name
/// (case aside, as Dante compares them), its number and the first free
/// suffix ("17-2").
pub fn resize(names: &[String], n: usize) -> Vec<String> {
    let mut out: Vec<String> = names.iter().take(n).cloned().collect();
    while out.len() < n {
        let number = out.len() + 1;
        let taken = |s: &str| out.iter().any(|o| o.eq_ignore_ascii_case(s));
        let mut name = number.to_string();
        let mut k = 2;
        while taken(&name) {
            name = format!("{number}-{k}");
            k += 1;
        }
        out.push(name);
    }
    out
}

/// One field of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    Iface,
    Block,
    RxLatency,
    TxLatency,
    Inputs,
    Outputs,
    PumpCpu,
    IoCpu,
    RxCpu,
}

/// When a change takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// At once, without a restart (●).
    Live,
    /// After a restart of the instance's service (↻).
    Restart,
}

impl Field {
    /// When a change of this field takes effect: the RX latency at once
    /// (the device's ARC latency write; the device keeps it in its state
    /// file), everything else at the service's restart. dstack has no live
    /// path for a fixed TX latency: receivers read it when they subscribe,
    /// and dasio reports it to the DAW at its start.
    pub fn kind(self) -> Kind {
        match self {
            Field::RxLatency => Kind::Live,
            _ => Kind::Restart,
        }
    }

    /// Its name in the window.
    pub fn label(self) -> &'static str {
        match self {
            Field::Iface => "Ethernet interface",
            Field::Block => "Buffer",
            Field::RxLatency => "Dante latency (RX)",
            Field::TxLatency => "TX latency",
            Field::Inputs => "Inputs",
            Field::Outputs => "Outputs",
            Field::PumpCpu => "Audio pump CPU",
            Field::IoCpu => "Send (TX) CPU",
            Field::RxCpu => "Receive (RX) CPU",
        }
    }

    /// Whether a change of it changes the shared memory's layout between
    /// the service and the DAW's driver (dasio's daw-device-switch rule:
    /// the block and the channel counts), which the DAW must release first.
    pub fn reshapes(self) -> bool {
        matches!(self, Field::Block | Field::Inputs | Field::Outputs)
    }
}

/// The fields that differ between `old` and `new`, in the window's order.
pub fn changes(old: &Fields, new: &Fields) -> Vec<Field> {
    let pairs = [
        (Field::Iface, old.iface != new.iface),
        (Field::Block, old.block != new.block),
        (Field::RxLatency, old.latency_ns != new.latency_ns),
        (Field::TxLatency, old.tx_latency_ns != new.tx_latency_ns),
        (Field::Inputs, old.rx != new.rx),
        (Field::Outputs, old.tx != new.tx),
        (Field::PumpCpu, old.cpus.pump != new.cpus.pump),
        (Field::IoCpu, old.cpus.io != new.cpus.io),
        (Field::RxCpu, old.cpus.rx != new.cpus.rx),
    ];
    pairs
        .into_iter()
        .filter_map(|(f, changed)| changed.then_some(f))
        .collect()
}

/// How many of `changes` need a restart.
pub fn restarts(changes: &[Field]) -> usize {
    changes.iter().filter(|f| f.kind() == Kind::Restart).count()
}

/// The banner over the buttons: how many changes need a restart of
/// `name`, and, when one of them `reshapes` the driver's shared memory,
/// that the DAW must release the driver first (dasio's rule: a restart of
/// the same shape is fine with the DAW open); `None` with none.
pub fn banner(name: &str, restarts: usize, reshapes: bool) -> Option<String> {
    let what = match restarts {
        0 => return None,
        1 => "1 change needs".to_string(),
        n => format!("{n} changes need"),
    };
    let daw = if reshapes {
        "The DAW must release the driver first (Ableton: Options → Audio → Driver: None, \
         then Apply). dtray waits until it is released."
    } else {
        "The DAW may keep the driver (the buffer and the channel counts stay)."
    };
    Some(format!(
        "↻ {what} a restart of {name} (~5 s of silence). {daw}"
    ))
}

/// The CPUs `others` (the other instances) pin, each once.
pub fn busy_cpus(others: &[Fields]) -> BTreeSet<usize> {
    others.iter().flat_map(|f| f.cpus.named()).collect()
}

/// A CPU picker's text: `CPU n`, or `Any` for none.
pub fn cpu_label(cpu: Option<usize>) -> String {
    cpu.map_or_else(|| "Any".to_string(), |c| format!("CPU {c}"))
}

/// Why `cpus` cannot be applied on a PC with `count` CPUs while the other
/// instances pin `busy`: each picker on a CPU the PC does not have or one
/// another instance uses. Empty when it can. The three pickers of one
/// instance may share a CPU (TX and RX on one core is a dasio setup).
pub fn cpu_problems(cpus: &Cpus, busy: &BTreeSet<usize>, count: usize) -> Vec<String> {
    [
        (Field::PumpCpu, cpus.pump),
        (Field::IoCpu, cpus.io),
        (Field::RxCpu, cpus.rx),
    ]
    .into_iter()
    .filter_map(|(f, cpu)| {
        let c = cpu?;
        if c >= count {
            Some(format!(
                "{}: CPU {c}, this PC has CPUs 0 to {}",
                f.label(),
                count.saturating_sub(1)
            ))
        } else if busy.contains(&c) {
            Some(format!("{}: CPU {c} is another instance's", f.label()))
        } else {
            None
        }
    })
    .collect()
}

/// What one apply does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The RX latency to write through the device's ARC, ns.
    pub latency: Option<u32>,
    /// The new config text, when a field needs a restart.
    pub config: Option<String>,
    /// The DAW must release the driver before the restart.
    pub release: bool,
}

/// The apply of `new` over `old` (the window's starting point: the
/// instance's config `text` as read, its RX latency the device's own), on
/// a PC with `cpus` CPUs while the other instances pin `busy` (only a
/// picker that changed is checked: [`changed_cpus`]): the live
/// latency write, and for any restart change the new file (with the
/// file's own RX latency: that one is the live write's) and whether the
/// DAW must release the driver. The reason when nothing changed, a CPU is
/// refused or the file is.
pub fn plan(
    text: &str,
    old: &Fields,
    new: &Fields,
    busy: &BTreeSet<usize>,
    cpus: usize,
) -> Result<Plan, String> {
    let changed = changes(old, new);
    if changed.is_empty() {
        return Err("nothing changed".into());
    }
    let problems = cpu_problems(&changed_cpus(&old.cpus, &new.cpus), busy, cpus);
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    let latency = if changed.contains(&Field::RxLatency) {
        Some(u32::try_from(new.latency_ns).map_err(|_| "the RX latency is too large".to_string())?)
    } else {
        None
    };
    let config = if restarts(&changed) > 0 {
        let mut file = new.clone();
        file.latency_ns = Fields::read(text)?.latency_ns;
        Some(edit(text, &file)?)
    } else {
        None
    };
    Ok(Plan {
        latency,
        config,
        release: changed.iter().any(|f| f.reshapes()),
    })
}

/// The status line on top of an instance's tab, from its device's status
/// line (`Err`: it could not be read: the reason).
pub fn status_line(status: &Result<String, String>) -> String {
    let line = match status {
        Err(e) => return format!("Not running ({e})"),
        Ok(line) => line,
    };
    let Some(s) = model::parse(line) else {
        return "Running · status not readable".to_string();
    };
    let mut out = "Running".to_string();
    if let Some(app) = s.app.as_ref().and_then(|a| a.line.as_deref()) {
        out.push_str(" · ");
        out.push_str(app);
    }
    let source = s.source.as_deref().unwrap_or("?");
    let ready = if s.ready { "ready" } else { "not ready" };
    out.push_str(&format!(" · clock: {source} {ready}"));
    out
}

/// How long the status line keeps saying "Restarting…" after the device
/// went away during an apply's restart (dasio takes about 20 s); then the
/// real reason shows.
pub const RESTART_WAIT: Duration = Duration::from_secs(60);

/// An apply's restart as the status line follows it, from the elevated
/// helper's start. `T` is the time (`Instant` in the window).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Restart<T> {
    /// When the device was first missed (`None`: still up, the helper has
    /// not restarted it yet).
    pub gone: Option<T>,
}

impl<T: Copy + Sub<Output = Duration>> Restart<T> {
    /// The helper started.
    pub fn started() -> Self {
        Restart { gone: None }
    }

    /// The restart after one reading at `now` (`found`: the device's
    /// status was read; `applying`: the apply still runs); `None` when it
    /// is over.
    pub fn next(self, found: bool, applying: bool, now: T) -> Option<Self> {
        let _ = (found, applying, now);
        None
    }
}

/// The status line of a tab: "Restarting…" while its device is missing
/// during a restart, else [`status_line`].
pub fn shown_status(status: &Result<String, String>, restarting: bool) -> String {
    let _ = restarting;
    status_line(status)
}

/// The app part's field `key` of a status line.
fn app_field(status: &str, key: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(status).ok()?;
    v.get("app")?.get(key).cloned()
}

/// Whether the DAW holds the driver, from the status's `app.daw_attached`
/// (dasio's part); `None` when the status does not say.
pub fn daw_attached(status: &str) -> Option<bool> {
    app_field(status, "daw_attached")?.as_bool()
}

/// What the restarted device reads back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadBack {
    /// `app.block` of its status.
    pub block: Option<u64>,
    /// Its channel counts (ARC 0x1000).
    pub rx: Option<usize>,
    pub tx: Option<usize>,
    /// Its configured latency (ARC settings), ns.
    pub latency_ns: Option<u64>,
    /// The alias of the adapter holding its address.
    pub iface: Option<String>,
}

/// The block of a status line's app part.
pub fn status_block(status: &str) -> Option<u64> {
    app_field(status, "block")?.as_u64()
}

/// The verdict on `back` against what was applied (`want`): the line to
/// show when every value read reads as applied, the mismatches otherwise
/// (a value not read is one). The CPUs and the TX latency are written but
/// not reported by the device, which the line says.
pub fn verify(want: &Fields, back: &ReadBack) -> Result<String, String> {
    let mut wrong = Vec::new();
    let mut check = |what: &str, want: String, got: Option<String>| match got {
        Some(g) if g == want => {}
        Some(g) => wrong.push(format!("{what}: {want} applied, {g} read back")),
        None => wrong.push(format!("{what}: not read back")),
    };
    check(
        "Buffer",
        want.block.to_string(),
        back.block.map(|b| b.to_string()),
    );
    check(
        "Inputs",
        want.rx.len().to_string(),
        back.rx.map(|n| n.to_string()),
    );
    check(
        "Outputs",
        want.tx.len().to_string(),
        back.tx.map(|n| n.to_string()),
    );
    check(
        "Dante latency (RX)",
        latency::label_wide(want.latency_ns),
        back.latency_ns.map(latency::label_wide),
    );
    if let Some(alias) = &want.iface {
        check("Ethernet interface", alias.clone(), back.iface.clone());
    }
    if !wrong.is_empty() {
        return Err(wrong.join("; "));
    }
    Ok(format!(
        "Applied: {} runs with buffer {}, {} in / {} out, RX latency {}. \
         CPUs and TX latency are in the file (the device does not report them).",
        want.name,
        want.block,
        want.rx.len(),
        want.tx.len(),
        latency::label_wide(want.latency_ns),
    ))
}

/// The elevated helper's write: `text` written as instance `number`'s
/// [`CONFIG`] under `root`, through [`STAGED`] renamed over it, so a reader
/// never sees half a file. The folder must exist with a config already
/// (the helper creates no instance), and `text` must be that config with
/// only the window's fields changed: [`edit`] of the current file with
/// `text`'s fields must give `text` exactly. So a staged file swapped by
/// another program of the user (the staged copy sits in the user's
/// folder) can change nothing the window cannot, such as the state
/// file's path. The written path; the reason otherwise.
pub fn install(root: &Path, number: u32, text: &str) -> Result<PathBuf, String> {
    let dir = root.join(format!("{INSTANCE_PREFIX}{number}"));
    // A link (a junction on Windows) could aim the admin's writes at
    // another folder: refused.
    for d in [root, dir.as_path()] {
        if std::fs::symlink_metadata(d).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{} is a link; nothing written", d.display()));
        }
    }
    let target = dir.join(CONFIG);
    let current =
        std::fs::read_to_string(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    let want = Fields::read(text).map_err(|e| format!("the new config is refused: {e}"))?;
    let rebuilt = edit(&current, &want).map_err(|e| format!("the new config is refused: {e}"))?;
    if rebuilt != text {
        return Err(
            "the new config changes more than the settings window's fields; nothing written"
                .to_string(),
        );
    }
    let staged = dir.join(STAGED);
    std::fs::write(&staged, text).map_err(|e| format!("{}: {e}", staged.display()))?;
    std::fs::rename(&staged, &target).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("{}: {e}", target.display())
    })?;
    Ok(target)
}

/// The instance number the helper's argument names (plain digits); the
/// reason otherwise.
pub fn instance_arg(arg: &str) -> Result<u32, String> {
    instance_number(&format!("{INSTANCE_PREFIX}{arg}"))
        .ok_or_else(|| format!("instance: {arg} is not an instance number"))
}

/// The elevated helper (`dtray-cli --dasio-apply <instance> <file>`): the
/// staged config at `file` written as instance `instance`'s under `root`
/// ([`install`]), then its service restarted by `restart` (the binary
/// passes [`service::restart`]). The line to print; the reason otherwise,
/// which says whether the file was written.
pub fn apply_staged(
    root: &Path,
    instance: &str,
    file: &Path,
    restart: impl FnOnce(&str) -> Result<(), String>,
) -> Result<String, String> {
    let n = instance_arg(instance)?;
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let written = install(root, n, &text)?;
    let service = service_name(n);
    restart(&service).map_err(|e| {
        format!(
            "{} was written, but {service} did not restart: {e}",
            written.display()
        )
    })?;
    Ok(format!(
        "{} written, {service} restarted",
        written.display()
    ))
}

/// The helper's verdict file in an instance's folder: the window reads
/// it after the helper ends (the elevated helper's console is not the
/// window's). The folder is the admin's, so no other program of the user
/// can redirect the elevated write.
pub const RESULT: &str = "dtray-apply.result";

/// The verdict file of instance `number` under `root`.
pub fn result_path(root: &Path, number: u32) -> PathBuf {
    root.join(format!("{INSTANCE_PREFIX}{number}")).join(RESULT)
}

/// The verdict file's text: `ok` or `error`, then the line.
pub fn result_text(outcome: &Result<String, String>) -> String {
    match outcome {
        Ok(line) => format!("ok\n{line}"),
        Err(reason) => format!("error\n{reason}"),
    }
}

/// A verdict file's outcome ([`result_text`]); an unknown text is an
/// error that quotes it.
pub fn parse_result(text: &str) -> Result<String, String> {
    match text.split_once('\n') {
        Some(("ok", line)) => Ok(line.to_string()),
        Some(("error", reason)) => Err(reason.to_string()),
        _ => Err(format!("the helper's verdict is not readable: {text:?}")),
    }
}

/// The helper's command line after the program: the mode, the instance
/// and the staged file in double quotes (a path with spaces).
pub fn helper_params(instance: u32, staged: &Path) -> String {
    format!("--dasio-apply {instance} \"{}\"", staged.display())
}

/// Whether the program at `exe` may be started as administrator: only
/// from under `program_files` (an admin-only folder: dasio's installer
/// puts dtray in `%ProgramFiles%\dasio`), compared without case as
/// Windows does. A copy anywhere the user can write could be replaced by
/// another program of the user and then run elevated. The reason when
/// not.
pub fn may_elevate(exe: &Path, program_files: Option<&Path>) -> Result<(), String> {
    let Some(base) = program_files.filter(|p| !p.as_os_str().is_empty()) else {
        return Err("the Program Files folder is not known; nothing is changed".to_string());
    };
    let lower = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    let (e, b) = (lower(exe), lower(base));
    if e.len() > b.len() && e.starts_with(&b) {
        Ok(())
    } else {
        Err(format!(
            "settings are changed only by a dtray installed under {} (this one runs from {})",
            base.display(),
            exe.display()
        ))
    }
}

/// Of `new`, the CPU pickers that differ from `old`; the others `None`
/// (a pin the file already has is not this apply's to refuse).
pub fn changed_cpus(old: &Cpus, new: &Cpus) -> Cpus {
    let pick = |o: Option<usize>, n: Option<usize>| if o == n { None } else { n };
    Cpus {
        pump: pick(old.pump, new.pump),
        io: pick(old.io, new.io),
        rx: pick(old.rx, new.rx),
    }
}

pub mod apply;
pub mod choices;
pub mod service;
#[cfg(windows)]
pub mod system;

#[cfg(test)]
mod tests;
