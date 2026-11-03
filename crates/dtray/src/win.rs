// SPDX-License-Identifier: MIT OR Apache-2.0

//! The tray on Windows: thin glue over the library. A thread timer wakes
//! the message loop once a second; each tick refreshes discovery, reads
//! every local device and the expected ones, and shows [`Tracker`]'s view.
//! One tray a desktop session (the named mutex [`MUTEX`]). The menu: a
//! submenu per device (its line; inside, its latency values,
//! [`latency::choices`], the configured one checked: choosing one writes it
//! with [`control::set_latency`]), a line per expected device not running,
//! Copy status (every device's JSON to the clipboard), Exit (the tray only;
//! the devices are other processes and keep running). The decisions (when
//! to read settings, when to build the menu again, whether a choice
//! writes) are [`dtray::menu`]'s.
//!
//! [`autostart`]: the user's Run value for `dtray --autostart on|off`.

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
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetLastError, GlobalFree,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MB_ICONERROR, MB_OK, MSG, MessageBoxW, SetTimer,
    TranslateMessage, WM_TIMER,
};

use dtray::discover::{self, Local, Reading, Scanner};
use dtray::latency;
use dtray::menu::{self, Known, Shape};
use dtray::model::{self, Colour, Tracker, View};
use dtray::{expected, icon};

/// The single-instance mutex of a desktop session.
const MUTEX: &str = r"Local\dtray";
/// The timer's period, ms.
const TICK_MS: u32 = 1000;
/// How long an ARC read or write waits for each reply.
const ARC_WAIT: Duration = Duration::from_millis(300);
/// The user's key of programs started at logon.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// An error box (the tray has no console).
pub fn message(text: &str) {
    let (t, c) = (wide(text), wide("dtray"));
    // SAFETY: valid NUL-terminated strings, no owner window.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            t.as_ptr(),
            c.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

/// Sets (`on`) or removes the user's Run value [`dtray::RUN_VALUE_NAME`]
/// for this program ([`dtray::run_value`]); removing one that is not
/// there is done.
pub fn autostart(on: bool) -> Result<(), String> {
    let (key, name) = (wide(RUN_KEY), wide(dtray::RUN_VALUE_NAME));
    if on {
        let exe = std::env::current_exe().map_err(|e| format!("the program's path: {e}"))?;
        let text = dtray::run_value(&exe)
            .ok_or_else(|| "the program's path is not valid Unicode".to_string())?;
        let value = wide(&text);
        let bytes = u32::try_from(value.len() * std::mem::size_of::<u16>())
            .map_err(|_| "the program's path is too long".to_string())?;
        // SAFETY: valid NUL-terminated strings; `bytes` covers `value` and
        // its NUL.
        let r = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                value.as_ptr().cast(),
                bytes,
            )
        };
        if r != ERROR_SUCCESS {
            return Err(format!("the Run value was not set (error {r})"));
        }
    } else {
        // SAFETY: valid NUL-terminated strings.
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
            return Err(format!("the Run value was not removed (error {r})"));
        }
    }
    Ok(())
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

/// Runs the tray until Exit; a second start in the session leaves at once.
pub fn run() -> Result<(), String> {
    let name = wide(MUTEX);
    // SAFETY: a valid name; the handle is held until the process ends.
    let _mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: plain call, right after CreateMutexW.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(());
    }
    let fixed = Fixed {
        version: MenuItem::new(format!("dstack {}", env!("CARGO_PKG_VERSION")), false, None),
        copy: MenuItem::new("Copy status", true, None),
        exit: MenuItem::new("Exit (the devices keep running)", true, None),
    };
    let mut scanner = Scanner::new();
    let mut tracker = Tracker::default();
    let mut known = Known::default();
    let mut ticks: u32 = 0;
    let mut now = tick(&mut scanner);
    read_settings(&now.devices, &mut known, ticks);
    let first = tracker.update_with(&now.readings, &now.expected);
    let mut shapes = menu::shapes_of(&now.devices, &known);
    let (first_menu, mut built) = build(&fixed, &first, &shapes, now.readings.len())?;
    let tray: TrayIcon = TrayIconBuilder::new()
        .with_menu(Box::new(first_menu))
        .with_tooltip(&first.tooltip)
        .with_icon(tray_icon(first.colour)?)
        .build()
        .map_err(|e| format!("the tray icon: {e}"))?;
    let mut shown = first;
    let mut forced = false;
    // A thread timer: WM_TIMER in this thread's queue every TICK_MS.
    // SAFETY: no window, no callback.
    unsafe { SetTimer(std::ptr::null_mut(), 0, TICK_MS, None) };
    let menu_events = MenuEvent::receiver();
    // SAFETY: MSG is plain data.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: a valid MSG; all of this thread's messages.
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got == -1 {
            // SAFETY: plain call, right after GetMessageW failed.
            let code = unsafe { GetLastError() };
            return Err(format!(
                "the message loop failed (GetMessageW, error {code})"
            ));
        }
        if got == 0 {
            return Ok(());
        }
        if msg.message == WM_TIMER && msg.hwnd.is_null() {
            now = tick(&mut scanner);
            ticks = ticks.wrapping_add(1);
            read_settings(&now.devices, &mut known, ticks);
            let v = tracker.update_with(&now.readings, &now.expected);
            let new_shapes = menu::shapes_of(&now.devices, &known);
            let devices = now.readings.len();
            if menu::needs_rebuild(
                forced,
                built.counts(),
                &shapes,
                &new_shapes,
                devices,
                v.lines.len(),
            ) {
                let (m, b) = build(&fixed, &v, &new_shapes, devices)?;
                built = b;
                shapes = new_shapes;
                forced = false;
                tray.set_menu(Some(Box::new(m)));
            } else {
                set_texts(&built, &v.lines, devices);
            }
            if v.colour != shown.colour {
                tray.set_icon(Some(tray_icon(v.colour)?))
                    .map_err(|e| format!("the tray icon: {e}"))?;
            }
            if v.tooltip != shown.tooltip {
                tray.set_tooltip(Some(&v.tooltip))
                    .map_err(|e| format!("the tooltip: {e}"))?;
            }
            shown = v;
        } else {
            // SAFETY: a message GetMessageW filled.
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        while let Ok(e) = menu_events.try_recv() {
            if e.id == *fixed.exit.id() {
                return Ok(());
            }
            if e.id == *fixed.copy.id()
                && let Err(err) = copy(&model::copy_json(&now.readings))
            {
                message(&format!("dtray: {err}"));
            }
            if let Some(&(_, arc, ns)) = built.choices.iter().find(|(id, _, _)| *id == e.id) {
                // The item checked or unchecked itself on the click: the
                // next tick builds the menu from what the device reads.
                forced = true;
                if !menu::write_needed(&shapes, arc, ns) {
                    continue;
                }
                match arc_call(arc, |c| {
                    control::set_latency(c, ns).map_err(|e| e.to_string())
                }) {
                    Ok(s) => known.record(arc, Some(s), ticks),
                    Err(err) => message(&format!(
                        "dtray: the latency {} was not set: {err}",
                        latency::label(ns)
                    )),
                }
            }
        }
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

/// `text` on the clipboard as Unicode text.
fn copy(text: &str) -> Result<(), String> {
    let w = wide(text);
    let bytes = w.len() * std::mem::size_of::<u16>();
    // SAFETY: no owner window; every path below closes the clipboard once.
    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return Err("the clipboard could not be opened".into());
    }
    // SAFETY: the clipboard is open (above); the memory is ours until
    // SetClipboardData takes it, and freed by us when it does not.
    let result = unsafe {
        EmptyClipboard();
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if mem.is_null() {
            Err("no memory for the clipboard".to_string())
        } else {
            let dst = GlobalLock(mem).cast::<u16>();
            if dst.is_null() {
                GlobalFree(mem);
                Err("the clipboard memory could not be locked".to_string())
            } else {
                std::ptr::copy_nonoverlapping(w.as_ptr(), dst, w.len());
                GlobalUnlock(mem);
                if SetClipboardData(u32::from(CF_UNICODETEXT), mem).is_null() {
                    GlobalFree(mem);
                    Err("the clipboard did not take the text".to_string())
                } else {
                    Ok(())
                }
            }
        }
    };
    // SAFETY: opened above.
    unsafe { CloseClipboard() };
    result
}
