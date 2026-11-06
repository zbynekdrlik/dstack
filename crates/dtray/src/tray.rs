// SPDX-License-Identifier: MIT OR Apache-2.0

//! The tray itself, the same on Windows and Linux: thin glue over the
//! library and `tray-icon`. Each [`Tray::tick`] (once a second, from the
//! OS's event loop: `win.rs`, `linux.rs`) refreshes discovery, reads every
//! local device and the expected ones, and shows [`Tracker`]'s view. The
//! menu: a submenu per device (its line; inside, its latency values,
//! [`latency::choices`], the configured one checked: choosing one writes it
//! with [`control::set_latency`]), a line per expected device not running,
//! and, for an app that offers outputs (dstack #139: dlisten), its Output
//! list, the one in use checked: choosing one sends `app output <n>` to the
//! device ([`probe::ask`], [`output::request`]); Copy status (every device's
//! JSON, to the clipboard by the OS's glue), Exit (the tray only; the
//! devices are other processes and keep running). The decisions (when to
//! read settings, when to build the menu again, whether a choice writes)
//! are [`dtray::menu`]'s.

use std::net::SocketAddrV4;
use std::sync::atomic::{AtomicBool, AtomicU16};
use std::time::Duration;

use dstack::arc::client::Udp;
use dstack::arc::control;
use dstack::ctl::Requests;
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use dtray::discover::{self, Local, Reading, Scanner};
use dtray::latency;
use dtray::menu::{self, Known, Shape};
use dtray::model::{self, Colour, Tracker, View};
use dtray::{expected, icon, output, probe};

/// How long an ARC read or write waits for each reply.
const ARC_WAIT: Duration = Duration::from_millis(300);

/// What a menu click asks of the OS's glue.
pub enum Action {
    /// End the tray.
    Exit,
    /// Put this text (every device's status) on the clipboard.
    Copy(String),
    /// Show this error.
    Error(String),
}

/// The menu's fixed items.
struct Fixed {
    version: MenuItem,
    copy: MenuItem,
    exit: MenuItem,
}

/// The menu's items that change between builds.
#[derive(Default)]
struct Built {
    /// Each device's submenu, in the readings' order.
    devices: Vec<Submenu>,
    /// Each expected device not running.
    missing: Vec<MenuItem>,
    /// Each latency item: the device's ARC and the value.
    choices: Vec<(MenuId, SocketAddrV4, u32)>,
    /// Each output item: the device's ARC and the request choosing it.
    outputs: Vec<(MenuId, SocketAddrV4, usize, String)>,
}

impl Built {
    fn counts(&self) -> menu::Counts {
        menu::Counts {
            devices: self.devices.len(),
            missing: self.missing.len(),
        }
    }
}

/// One tick's input: the devices, their readings and the expected names.
struct Tick {
    devices: Vec<Local>,
    readings: Vec<Reading>,
    expected: Vec<String>,
}

/// The tray icon, its menu and what they show.
pub struct Tray {
    fixed: Fixed,
    scanner: Scanner,
    tracker: Tracker,
    known: Known,
    ticks: u32,
    now: Tick,
    shapes: Vec<Shape>,
    built: Built,
    shown: View,
    /// A latency item was clicked: it checked itself, so the next tick
    /// builds the menu again from what the device reads.
    forced: bool,
    icon: TrayIcon,
}

impl Tray {
    /// The first tick and the icon. Must run on the thread of the OS's
    /// event loop (GTK's on Linux, after `gtk::init`).
    pub fn start() -> Result<Tray, String> {
        let fixed = Fixed {
            version: MenuItem::new(format!("dstack {}", env!("CARGO_PKG_VERSION")), false, None),
            copy: MenuItem::new("Copy status", true, None),
            exit: MenuItem::new("Exit (the devices keep running)", true, None),
        };
        let mut scanner = Scanner::new();
        let mut tracker = Tracker::default();
        let mut known = Known::default();
        let now = tick(&mut scanner);
        read_settings(&now.devices, &mut known, 0);
        let first = tracker.update_with(&now.readings, &now.expected);
        let shapes = menu::shapes_of(&now.devices, &known, &now.readings);
        let (first_menu, built) = build(&fixed, &first, &shapes, now.readings.len())?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(first_menu))
            .with_tooltip(&first.tooltip)
            .with_icon(tray_icon(first.colour)?)
            .build()
            .map_err(|e| format!("the tray icon: {e}"))?;
        Ok(Tray {
            fixed,
            scanner,
            tracker,
            known,
            ticks: 0,
            now,
            shapes,
            built,
            shown: first,
            forced: false,
            icon,
        })
    }

    /// One tick: the devices read again and the icon, tooltip and menu
    /// set to the new view.
    pub fn tick(&mut self) -> Result<(), String> {
        self.now = tick(&mut self.scanner);
        self.ticks = self.ticks.wrapping_add(1);
        read_settings(&self.now.devices, &mut self.known, self.ticks);
        let v = self
            .tracker
            .update_with(&self.now.readings, &self.now.expected);
        let new_shapes = menu::shapes_of(&self.now.devices, &self.known, &self.now.readings);
        let devices = self.now.readings.len();
        if menu::needs_rebuild(
            self.forced,
            self.built.counts(),
            &self.shapes,
            &new_shapes,
            devices,
            v.lines.len(),
        ) {
            let (m, b) = build(&self.fixed, &v, &new_shapes, devices)?;
            self.built = b;
            self.shapes = new_shapes;
            self.forced = false;
            self.icon.set_menu(Some(Box::new(m)));
        } else {
            set_texts(&self.built, &v.lines, devices);
        }
        if v.colour != self.shown.colour {
            self.icon
                .set_icon(Some(tray_icon(v.colour)?))
                .map_err(|e| format!("the tray icon: {e}"))?;
        }
        if v.tooltip != self.shown.tooltip {
            self.icon
                .set_tooltip(Some(&v.tooltip))
                .map_err(|e| format!("the tooltip: {e}"))?;
        }
        self.shown = v;
        Ok(())
    }

    /// The menu clicks since the last call, handled: a latency choice is
    /// written here; Exit, Copy status and a failed write are left to the
    /// OS's glue.
    pub fn menu_events(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        while let Ok(e) = MenuEvent::receiver().try_recv() {
            if e.id == *self.fixed.exit.id() {
                // Clicks after Exit write nothing.
                out.push(Action::Exit);
                break;
            } else if e.id == *self.fixed.copy.id() {
                out.push(Action::Copy(model::copy_json(&self.now.readings)));
            } else if let Some(&(_, arc, ns)) =
                self.built.choices.iter().find(|(id, _, _)| *id == e.id)
            {
                self.forced = true;
                if !menu::write_needed(&self.shapes, arc, ns) {
                    continue;
                }
                match arc_call(arc, |c| {
                    control::set_latency(c, ns).map_err(|e| e.to_string())
                }) {
                    Ok(s) => self.known.record(arc, Some(s), self.ticks),
                    Err(err) => out.push(Action::Error(format!(
                        "dtray: the latency {} was not set: {err}",
                        latency::label(ns)
                    ))),
                }
            } else if let Some((_, arc, index, request)) =
                self.built.outputs.iter().find(|(id, _, _, _)| *id == e.id)
            {
                let (arc, index) = (*arc, *index);
                // The item checked itself: the next tick builds the menu
                // from what the app reports.
                self.forced = true;
                if !menu::output_needed(&self.shapes, arc, index) {
                    continue;
                }
                let Some(port) = self
                    .now
                    .devices
                    .iter()
                    .find(|d| d.arc == arc)
                    .map(|d| d.port)
                else {
                    continue;
                };
                if let Err(err) = probe::ask(port, request) {
                    out.push(Action::Error(format!(
                        "dtray: the output was not chosen: {err}"
                    )));
                }
            }
        }
        out
    }
}

/// The view's lines into the built menu's items, where they changed:
/// the first `devices` into the device submenus, the rest into the
/// missing lines (their counts match: [`menu::needs_rebuild`]).
fn set_texts(built: &Built, lines: &[String], devices: usize) {
    for (sub, text) in built.devices.iter().zip(lines) {
        if sub.text() != *text {
            sub.set_text(text);
        }
    }
    for (item, text) in built.missing.iter().zip(lines.iter().skip(devices)) {
        if item.text() != *text {
            item.set_text(text);
        }
    }
}

/// Discovery kept in step with the PC's addresses (a failure is tried
/// again on the next tick), then every local device's reading and the
/// expected devices.
fn tick(scanner: &mut Scanner) -> Tick {
    let _failed = scanner.refresh();
    let devices = scanner.devices();
    let readings = discover::read(&devices);
    let expected = expected::expected(&expected::devices_dir());
    Tick {
        devices,
        readings,
        expected,
    }
}

/// One exchange with the ARC at `arc`, from the PC's address it sits on.
fn arc_call<T>(
    arc: SocketAddrV4,
    f: impl FnOnce(&mut Udp<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let seq = AtomicU16::new(1);
    let requests = Requests::default();
    let stop = AtomicBool::new(false);
    let mut udp = Udp::open(*arc.ip(), arc, &seq, ARC_WAIT, &requests, &stop)
        .map_err(|e| format!("no socket: {e}"))?;
    f(&mut udp)
}

/// Reads the settings of the devices [`Known::due`] names at `tick`; gone
/// devices are forgotten.
fn read_settings(devices: &[Local], known: &mut Known, tick: u32) {
    let arcs: Vec<SocketAddrV4> = devices.iter().map(|d| d.arc).collect();
    known.forget_gone(&arcs);
    for arc in known.due(&arcs, tick) {
        let read = arc_call(arc, |c| control::settings(c).map_err(|e| e.to_string())).ok();
        known.record(arc, read, tick);
    }
}

/// The menu: the version, a submenu per device (its line, its latency
/// values), a line per expected device not running, Copy status, Exit.
/// `devices` is how many of the view's lines are devices'.
fn build(
    fixed: &Fixed,
    v: &View,
    shapes: &[Shape],
    devices: usize,
) -> Result<(Menu, Built), String> {
    let menu = Menu::new();
    let e = |r: Result<(), tray_icon::menu::Error>| r.map_err(|e| format!("the menu: {e}"));
    e(menu.append(&fixed.version))?;
    e(menu.append(&PredefinedMenuItem::separator()))?;
    let mut built = Built::default();
    if v.lines.is_empty() {
        e(menu.append(&MenuItem::new("No dstack device on this PC", false, None)))?;
    }
    for (text, shape) in v.lines.iter().take(devices).zip(shapes) {
        let sub = Submenu::new(text, true);
        if shape.choices.is_empty() {
            e(sub.append(&MenuItem::new("Latency: not known", false, None)))?;
        } else {
            e(sub.append(&MenuItem::new("Latency", false, None)))?;
        }
        for c in &shape.choices {
            let item = CheckMenuItem::new(&c.label, true, c.checked, None);
            built.choices.push((item.id().clone(), shape.arc, c.ns));
            e(sub.append(&item))?;
        }
        if !shape.outputs.is_empty() {
            e(sub.append(&PredefinedMenuItem::separator()))?;
            e(sub.append(&MenuItem::new("Output", false, None)))?;
        }
        for o in &shape.outputs {
            let item = CheckMenuItem::new(&o.label, true, o.checked, None);
            built.outputs.push((
                item.id().clone(),
                shape.arc,
                o.index,
                output::request(o.index, &o.name),
            ));
            e(sub.append(&item))?;
        }
        e(menu.append(&sub))?;
        built.devices.push(sub);
    }
    for text in v.lines.iter().skip(devices) {
        let item = MenuItem::new(text, false, None);
        e(menu.append(&item))?;
        built.missing.push(item);
    }
    e(menu.append(&PredefinedMenuItem::separator()))?;
    e(menu.append(&fixed.copy))?;
    e(menu.append(&fixed.exit))?;
    Ok((menu, built))
}

/// The icon in the view's colour ([`icon::rgba`]).
fn tray_icon(c: Colour) -> Result<Icon, String> {
    Icon::from_rgba(icon::rgba(c), icon::SIZE, icon::SIZE).map_err(|e| format!("the icon: {e}"))
}
