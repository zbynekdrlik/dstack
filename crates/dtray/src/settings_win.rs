// SPDX-License-Identifier: MIT OR Apache-2.0

//! The dasio settings window on Windows (dstack #246; mockup v1,
//! `docs/mockups/dtray-dasio-settings-v1.html`): thin Win32 glue over
//! [`dtray::dasio`], whose functions decide every list, change, banner,
//! plan and verdict (tested there). One window, a tab per instance, on the
//! thread of the tray's message loop (or of its own loop for
//! `dtray --settings`); an apply runs on a worker thread
//! ([`dtray::dasio::apply::run`] with [`WinOps`]) and posts its progress
//! back. Outside the mutation gate, as `win.rs`.

use std::cell::{Cell, RefCell};
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use dstack::arc::control;
use dtray::dasio::apply::{self, Ops};
use dtray::dasio::choices::{self, Adapter, Choice};
use dtray::dasio::{self, Fields, Instance, ReadBack};
use dtray::discover::{self, Local, Reading, Scanner};
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT,
    CreateFontIndirectW, CreateSolidBrush, DT_SINGLELINE, DT_VCENTER, DrawTextW, FillRect,
    GetSysColor, GetSysColorBrush, HBRUSH, HDC, HFONT, SetBkColor, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ICC_TAB_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, MEASUREITEMSTRUCT,
    NMHDR, ODS_SELECTED, TCIF_TEXT, TCITEMW, TCM_GETCURSEL, TCM_INSERTITEMW, TCN_SELCHANGE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_GROUPBOX, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_GETITEMDATA,
    CB_GETLBTEXT, CB_GETLBTEXTLEN, CB_RESETCONTENT, CB_SETCURSEL, CB_SETITEMDATA, CBN_SELCHANGE,
    CBS_DROPDOWNLIST, CBS_HASSTRINGS, CBS_OWNERDRAWFIXED, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, IDC_ARROW,
    KillTimer, LoadCursorW, MSG, NONCLIENTMETRICSW, PostMessageW, PostQuitMessage, RegisterClassW,
    SPI_GETNONCLIENTMETRICS, SW_SHOW, SendMessageW, SetForegroundWindow, SetTimer, SetWindowTextW,
    ShowWindow, SystemParametersInfoW, TranslateMessage, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_MEASUREITEM, WM_NOTIFY,
    WM_SETFONT, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

/// A static's styles (`SS_LEFT`, `SS_NOPREFIX`: an `&` shows as itself).
const SS_LEFT: u32 = 0;
const SS_NOPREFIX: u32 = 0x80;

/// The window's class.
const CLASS: &str = "dtray-dasio-settings";
/// The worker's progress is waiting in the channel.
const WM_PROGRESS: u32 = 0x8000 + 46; // WM_APP + #246
/// The standalone window's discovery timer.
const TIMER: usize = 1;

// Control ids.
const ID_TAB: usize = 100;
const ID_STATUS: usize = 101;
const ID_IFACE: usize = 110;
const ID_BLOCK: usize = 111;
const ID_RXLAT: usize = 112;
const ID_TXLAT: usize = 113;
const ID_RATE: usize = 114;
const ID_IN: usize = 115;
const ID_OUT: usize = 116;
const ID_PUMP: usize = 117;
const ID_IO: usize = 118;
const ID_RXCPU: usize = 119;
const ID_BANNER: usize = 130;
const ID_RESULT: usize = 131;
const ID_CANCEL: usize = 140;
const ID_APPLY: usize = 141;
/// The ● marker (applies at once).
const ID_LIVE: usize = 150;
/// The ↻ markers (need a restart).
const ID_RESTART: usize = 160;

/// The client area.
const WIDTH: i32 = 640;
const HEIGHT: i32 = 580;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

const WARN: COLORREF = rgb(0xb4, 0x53, 0x09);
const WARN_BG: COLORREF = rgb(0xff, 0xf7, 0xe6);
const OK: COLORREF = rgb(0x15, 0x80, 0x3d);

/// What the worker sends back.
enum Progress {
    Say(String),
    Done(Result<String, String>),
}

/// One instance's tab.
struct Tab {
    inst: Instance,
    /// The config as read when the tab loaded.
    text: String,
    /// The window's starting point and the choices, or why the file is
    /// not readable.
    fields: Result<(Fields, Fields), String>,
    /// The device's latency range (ARC), when it was read.
    range: Option<(u64, u64)>,
}

/// The lists the controls show for the current tab.
#[derive(Default)]
struct Lists {
    iface: Vec<Choice<Option<String>>>,
    block: Vec<Choice<u64>>,
    rxlat: Vec<Choice<u64>>,
    txlat: Vec<Choice<Option<u64>>>,
    inputs: Vec<Choice<usize>>,
    outputs: Vec<Choice<usize>>,
    pump: Vec<Choice<Option<usize>>>,
    io: Vec<Choice<Option<usize>>>,
    rx: Vec<Choice<Option<usize>>>,
}

struct State {
    hwnd: HWND,
    font: HFONT,
    tabs: Vec<Tab>,
    cur: usize,
    lists: Lists,
    adapters: Vec<Adapter>,
    cpu_count: usize,
    devices: Vec<Local>,
    readings: Vec<Reading>,
    shown_status: String,
    busy: Option<Receiver<Progress>>,
    /// `dtray --settings`: the window is the program, its close ends it.
    standalone: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static WARN_BRUSH: Cell<HBRUSH> = const { Cell::new(std::ptr::null_mut()) };
}

/// The window's handle when it is open.
fn open_hwnd() -> Option<HWND> {
    STATE.with(|s| s.try_borrow().ok()?.as_ref().map(|st| st.hwnd))
}

/// Opens the window, or brings it to the front when it is open. `devices`
/// and `readings` are the tray's last tick.
pub fn open(devices: &[Local], readings: &[Reading]) -> Result<(), String> {
    if let Some(h) = open_hwnd() {
        // SAFETY: our own window.
        unsafe { SetForegroundWindow(h) };
        return Ok(());
    }
    create(devices.to_vec(), readings.to_vec(), false)
}

/// The tray's tick: the devices and their readings, for the status line.
pub fn feed(devices: &[Local], readings: &[Reading]) {
    let update = STATE.with(|s| {
        let Ok(mut s) = s.try_borrow_mut() else {
            return None;
        };
        let st = s.as_mut()?;
        st.devices = devices.to_vec();
        st.readings = readings.to_vec();
        let line = status_text(st);
        (line != st.shown_status).then(|| {
            st.shown_status = line.clone();
            (st.hwnd, line)
        })
    });
    if let Some((h, line)) = update {
        set_text(h, ID_STATUS, &line);
    }
}

/// `dtray --settings`: the window alone, with its own discovery, until it
/// is closed.
pub fn run_standalone() -> Result<(), String> {
    let mut scanner = Scanner::new();
    let _ = scanner.refresh();
    std::thread::sleep(dtray::cli::PRINT_WAIT);
    let devices = scanner.devices();
    let readings = discover::read(&devices);
    create(devices, readings, true)?;
    // SAFETY: a thread timer, no window, no callback.
    unsafe { SetTimer(std::ptr::null_mut(), TIMER, 1000, None) };
    // SAFETY: plain data.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: a valid MSG; all of this thread's messages.
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got == 0 || got == -1 {
            return Ok(());
        }
        if msg.message == WM_TIMER && msg.hwnd.is_null() {
            let _ = scanner.refresh();
            let devices = scanner.devices();
            let readings = discover::read(&devices);
            feed(&devices, &readings);
            continue;
        }
        // SAFETY: a message GetMessageW filled.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// The message font (Segoe UI on current Windows).
fn message_font() -> HFONT {
    // SAFETY: plain data.
    let mut m: NONCLIENTMETRICSW = unsafe { std::mem::zeroed() };
    m.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
    // SAFETY: a sized, writable structure.
    let ok =
        unsafe { SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, m.cbSize, (&raw mut m).cast(), 0) };
    if ok == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the font the system returned.
    unsafe { CreateFontIndirectW(&m.lfMessageFont) }
}

/// The tabs of this PC's instances: each config read, the device's own
/// latency and range read through its ARC when it runs.
fn load_tabs(devices: &[Local]) -> Vec<Tab> {
    dasio::instances(&dasio::root())
        .into_iter()
        .map(|inst| load_tab(inst, devices))
        .collect()
}

fn load_tab(inst: Instance, devices: &[Local]) -> Tab {
    let (text, fields) = match std::fs::read_to_string(inst.config()) {
        Ok(t) => {
            let f = Fields::read(&t).map(|f| (f.clone(), f));
            (t, f)
        }
        Err(e) => (
            String::new(),
            Err(format!("{}: {e}", inst.config().display())),
        ),
    };
    let mut tab = Tab {
        inst,
        text,
        fields,
        range: None,
    };
    if let Ok((old, new)) = &mut tab.fields
        && let Some(d) = device_of(devices, &old.name)
        && let Ok(s) = dtray::arc::call(d.arc, |c| control::settings(c).map_err(|e| e.to_string()))
    {
        if let Some(ns) = s.latency_configured_ns {
            old.latency_ns = u64::from(ns);
            new.latency_ns = u64::from(ns);
        }
        if let (Some(min), Some(max)) = (s.latency_min_ns, s.latency_max_ns) {
            tab.range = Some((u64::from(min), u64::from(max)));
        }
    }
    tab
}

/// The local device named `name` (case aside).
fn device_of<'a>(devices: &'a [Local], name: &str) -> Option<&'a Local> {
    dtray::set::find(devices, name).ok()
}

fn create(devices: Vec<Local>, readings: Vec<Reading>, standalone: bool) -> Result<(), String> {
    let tabs = load_tabs(&devices);
    if tabs.is_empty() {
        return Err(format!(
            "no dasio instance on this PC (no instance-N folder with a {} under {})",
            dasio::CONFIG,
            dasio::root().display()
        ));
    }
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_TAB_CLASSES,
    };
    // SAFETY: a filled structure.
    unsafe { InitCommonControlsEx(&icc) };
    // SAFETY: this program's module.
    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = wide(CLASS);
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst,
        // SAFETY: a system cursor.
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        hbrBackground: (COLOR_WINDOW + 1) as usize as HBRUSH,
        lpszClassName: class.as_ptr(),
        // SAFETY: the rest is plain data.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: a filled class; registering twice fails harmlessly.
    unsafe { RegisterClassW(&wc) };
    let adapters = dasio::system::adapters().unwrap_or_default();
    let cpu_count = std::thread::available_parallelism().map_or(1, usize::from);
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            hwnd: std::ptr::null_mut(),
            font: message_font(),
            tabs,
            cur: 0,
            lists: Lists::default(),
            adapters,
            cpu_count,
            devices,
            readings,
            shown_status: String::new(),
            busy: None,
            standalone,
        })
    });
    let title = wide(&format!(
        "dasio Settings — dtray {}",
        env!("CARGO_PKG_VERSION")
    ));
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
    // The outer size for the client area (caption and borders, classic
    // metrics: close enough for a fixed dialog).
    // SAFETY: the class registered above; WM_CREATE builds the controls.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIDTH + 16,
            HEIGHT + 39,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        STATE.with(|s| *s.borrow_mut() = None);
        return Err("the settings window was not created".into());
    }
    // SAFETY: our window.
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
    Ok(())
}

/// A child control.
fn control(
    parent: HWND,
    font: HFONT,
    class: &str,
    text: &str,
    style: u32,
    id: usize,
    r: (i32, i32, i32, i32),
) -> HWND {
    let (c, t) = (wide(class), wide(text));
    // SAFETY: valid strings; the id travels in the menu handle's place.
    let h = unsafe {
        CreateWindowExW(
            0,
            c.as_ptr(),
            t.as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            r.0,
            r.1,
            r.2,
            r.3,
            parent,
            id as _,
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if !font.is_null() {
        // SAFETY: a control and a font handle.
        unsafe { SendMessageW(h, WM_SETFONT, font as WPARAM, 1) };
    }
    h
}

fn item(parent: HWND, id: usize) -> HWND {
    // SAFETY: plain call.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetDlgItem(parent, id as i32) }
}

fn set_text(parent: HWND, id: usize, text: &str) {
    let w = wide(text);
    // SAFETY: a valid string.
    unsafe { SetWindowTextW(item(parent, id), w.as_ptr()) };
}

/// Builds every control (WM_CREATE).
fn build(h: HWND, font: HFONT, tabs: &[Tab]) {
    let st =
        |text: &str, id: usize, r| control(h, font, "STATIC", text, SS_LEFT | SS_NOPREFIX, id, r);
    let group = |text: &str, r| control(h, font, "BUTTON", text, BS_GROUPBOX as u32, 0, r);
    let combo = |id: usize, r: (i32, i32, i32, i32), owner: bool| {
        let extra = if owner {
            (CBS_OWNERDRAWFIXED | CBS_HASSTRINGS) as u32
        } else {
            0
        };
        control(
            h,
            font,
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL | WS_TABSTOP | extra,
            id,
            (r.0, r.1, r.2, 320),
        )
    };
    let tab = control(
        h,
        font,
        "SysTabControl32",
        "",
        WS_CLIPCHILDREN,
        ID_TAB,
        (8, 8, WIDTH - 16, 26),
    );
    for (i, t) in tabs.iter().enumerate() {
        let name = match &t.fields {
            Ok((old, _)) => old.name.clone(),
            Err(_) => t.inst.service(),
        };
        let mut w = wide(&name);
        // SAFETY: plain data.
        let mut ti: TCITEMW = unsafe { std::mem::zeroed() };
        ti.mask = TCIF_TEXT;
        ti.pszText = w.as_mut_ptr();
        // SAFETY: a tab control and a filled item.
        unsafe { SendMessageW(tab, TCM_INSERTITEMW, i, (&raw const ti) as LPARAM) };
    }
    st("", ID_STATUS, (16, 42, WIDTH - 32, 20));
    let label_x = 28;
    let ctl_x = 200;
    let ctl_w = 380;
    let mark_x = 592;
    let row = |y: i32, text: &str, id: usize, mark: usize, glyph: &str| {
        st(text, 0, (label_x, y + 3, 168, 20));
        combo(id, (ctl_x, y, ctl_w, 0), false);
        if !glyph.is_empty() {
            st(glyph, mark, (mark_x, y + 3, 20, 20));
        }
    };
    group("Network", (16, 68, WIDTH - 32, 52));
    row(88, "Ethernet interface", ID_IFACE, ID_RESTART, "↻");
    group("Audio", (16, 126, WIDTH - 32, 132));
    row(146, "Buffer (block)", ID_BLOCK, ID_RESTART + 1, "↻");
    row(174, "Dante latency (RX)", ID_RXLAT, ID_LIVE, "●");
    row(202, "TX latency", ID_TXLAT, ID_RESTART + 2, "↻");
    st("Sample rate", 0, (label_x, 233, 168, 20));
    st("", ID_RATE, (ctl_x + 4, 233, ctl_w, 20));
    group("Channels", (16, 264, WIDTH - 32, 80));
    row(284, "Inputs (Dante RX)", ID_IN, ID_RESTART + 3, "↻");
    row(312, "Outputs (Dante TX)", ID_OUT, ID_RESTART + 4, "↻");
    group("CPUs", (16, 350, WIDTH - 32, 92));
    let cw = 180;
    for (i, (text, id)) in [
        ("Audio pump", ID_PUMP),
        ("Send (TX)", ID_IO),
        ("Receive (RX)", ID_RXCPU),
    ]
    .into_iter()
    .enumerate()
    {
        let x = 28 + i as i32 * (cw + 16);
        st(text, 0, (x, 368, cw, 18));
        combo(id, (x, 388, cw, 0), true);
    }
    st(
        "Busy CPUs of the other instance are greyed out in the lists.",
        0,
        (28, 418, 540, 18),
    );
    st("↻", ID_RESTART + 5, (mark_x, 418, 20, 20));
    st("", ID_BANNER, (16, 450, WIDTH - 32, 44));
    st("", ID_RESULT, (16, 498, WIDTH - 32, 36));
    st(
        "●  applies at once      ↻  needs a restart",
        0,
        (16, 548, 300, 20),
    );
    control(
        h,
        font,
        "BUTTON",
        "Cancel",
        BS_PUSHBUTTON as u32 | WS_TABSTOP,
        ID_CANCEL,
        (404, 542, 90, 28),
    );
    control(
        h,
        font,
        "BUTTON",
        "Apply and restart",
        BS_DEFPUSHBUTTON as u32 | WS_TABSTOP,
        ID_APPLY,
        (502, 542, 122, 28),
    );
}

/// Fills `id`'s combo box with `list`, `selected` chosen; the greyed
/// entries carry item data 1 (the owner-drawn CPU lists).
fn fill<T>(h: HWND, id: usize, list: &[Choice<T>], selected: usize) {
    let c = item(h, id);
    // SAFETY: a combo box of ours; the strings outlive each call.
    unsafe {
        SendMessageW(c, CB_RESETCONTENT, 0, 0);
        for (i, ch) in list.iter().enumerate() {
            let w = wide(&ch.text);
            SendMessageW(c, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
            SendMessageW(c, CB_SETITEMDATA, i, isize::from(ch.greyed));
        }
        SendMessageW(c, CB_SETCURSEL, selected, 0);
    }
}

/// The selected index of `id`'s combo box (0 when none).
fn selected(h: HWND, id: usize) -> usize {
    // SAFETY: a combo box of ours.
    let i = unsafe { SendMessageW(item(h, id), CB_GETCURSEL, 0, 0) };
    usize::try_from(i).unwrap_or(0)
}

fn enable(h: HWND, id: usize, on: bool) {
    // SAFETY: a control of ours.
    unsafe { EnableWindow(item(h, id), i32::from(on)) };
}

const COMBOS: [usize; 9] = [
    ID_IFACE, ID_BLOCK, ID_RXLAT, ID_TXLAT, ID_IN, ID_OUT, ID_PUMP, ID_IO, ID_RXCPU,
];

/// The other instances' fields (their files as read).
fn others(st: &State) -> Vec<Fields> {
    st.tabs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != st.cur)
        .filter_map(|(_, t)| t.fields.as_ref().ok().map(|(old, _)| old.clone()))
        .collect()
}

/// The current tab's status line.
fn status_text(st: &State) -> String {
    let Some(tab) = st.tabs.get(st.cur) else {
        return String::new();
    };
    let Ok((old, _)) = &tab.fields else {
        return "Settings not readable".to_string();
    };
    let reading = st
        .readings
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(&old.name))
        .map(|(_, r)| r.clone())
        .unwrap_or_else(|| Err("not found on this PC".to_string()));
    dasio::status_line(&reading)
}

/// The current tab into the controls (lists, selections, texts).
fn show_tab(st: &mut State) {
    let h = st.hwnd;
    let busy = dasio::busy_cpus(&others(st));
    let tab = &st.tabs[st.cur];
    let (old, new) = match &tab.fields {
        Ok(f) => f.clone(),
        Err(e) => {
            for id in COMBOS {
                fill::<u8>(h, id, &[], 0);
                enable(h, id, false);
            }
            enable(h, ID_APPLY, false);
            set_text(h, ID_RATE, "");
            set_text(h, ID_BANNER, "");
            set_text(
                h,
                ID_RESULT,
                &format!("This instance's settings cannot be read: {e}"),
            );
            return;
        }
    };
    let running = device_of(&st.devices, &old.name)
        .and_then(|d| choices::alias_of(&st.adapters, *d.arc.ip()));
    let l = Lists {
        iface: choices::interfaces(&st.adapters, old.iface.as_deref(), running.as_deref()),
        block: choices::blocks(old.block, old.sample_rate),
        rxlat: choices::rx_latencies(old.latency_ns, tab.range),
        txlat: choices::tx_latencies(old.tx_latency_ns),
        inputs: choices::channels(old.rx.len()),
        outputs: choices::channels(old.tx.len()),
        pump: choices::cpus(st.cpu_count, &busy, old.cpus.pump),
        io: choices::cpus(st.cpu_count, &busy, old.cpus.io),
        rx: choices::cpus(st.cpu_count, &busy, old.cpus.rx),
    };
    fill(
        h,
        ID_IFACE,
        &l.iface,
        choices::index_of(&l.iface, &new.iface),
    );
    fill(
        h,
        ID_BLOCK,
        &l.block,
        choices::index_of(&l.block, &new.block),
    );
    fill(
        h,
        ID_RXLAT,
        &l.rxlat,
        choices::index_of(&l.rxlat, &new.latency_ns),
    );
    fill(
        h,
        ID_TXLAT,
        &l.txlat,
        choices::index_of(&l.txlat, &new.tx_latency_ns),
    );
    fill(
        h,
        ID_IN,
        &l.inputs,
        choices::index_of(&l.inputs, &new.rx.len()),
    );
    fill(
        h,
        ID_OUT,
        &l.outputs,
        choices::index_of(&l.outputs, &new.tx.len()),
    );
    fill(
        h,
        ID_PUMP,
        &l.pump,
        choices::index_of(&l.pump, &new.cpus.pump),
    );
    fill(h, ID_IO, &l.io, choices::index_of(&l.io, &new.cpus.io));
    fill(h, ID_RXCPU, &l.rx, choices::index_of(&l.rx, &new.cpus.rx));
    st.lists = l;
    for id in COMBOS {
        enable(h, id, st.busy.is_none());
    }
    set_text(
        h,
        ID_RATE,
        &format!(
            "{} kHz (the network's; read only)",
            choices::khz(old.sample_rate)
        ),
    );
    set_text(h, ID_RESULT, "");
    st.shown_status = status_text(st);
    set_text(h, ID_STATUS, &st.shown_status);
    refresh(st);
}

/// The controls' selections into the current tab's choices.
fn read_controls(st: &mut State) {
    let h = st.hwnd;
    let l = &st.lists;
    let Some(Ok((old, new))) = st.tabs.get_mut(st.cur).map(|t| t.fields.as_mut()) else {
        return;
    };
    let pick = |id: usize| selected(h, id);
    if let Some(c) = l.iface.get(pick(ID_IFACE)) {
        new.iface = c.value.clone();
    }
    if let Some(c) = l.block.get(pick(ID_BLOCK)) {
        new.block = c.value;
    }
    if let Some(c) = l.rxlat.get(pick(ID_RXLAT)) {
        new.latency_ns = c.value;
    }
    if let Some(c) = l.txlat.get(pick(ID_TXLAT)) {
        new.tx_latency_ns = c.value;
    }
    if let Some(c) = l.inputs.get(pick(ID_IN)) {
        new.rx = dasio::resize(&old.rx, c.value);
    }
    if let Some(c) = l.outputs.get(pick(ID_OUT)) {
        new.tx = dasio::resize(&old.tx, c.value);
    }
    if let Some(c) = l.pump.get(pick(ID_PUMP)) {
        new.cpus.pump = c.value;
    }
    if let Some(c) = l.io.get(pick(ID_IO)) {
        new.cpus.io = c.value;
    }
    if let Some(c) = l.rx.get(pick(ID_RXCPU)) {
        new.cpus.rx = c.value;
    }
}

/// The banner, the button and the check of the current choices.
fn refresh(st: &State) {
    let h = st.hwnd;
    let Some(Ok((old, new))) = st.tabs.get(st.cur).map(|t| t.fields.as_ref()) else {
        return;
    };
    let changed = dasio::changes(old, new);
    let restarts = dasio::restarts(&changed);
    let reshapes = changed.iter().any(|f| f.reshapes());
    set_text(
        h,
        ID_BANNER,
        &dasio::banner(&old.name, restarts, reshapes).unwrap_or_default(),
    );
    set_text(
        h,
        ID_APPLY,
        if restarts > 0 {
            "Apply and restart"
        } else {
            "Apply"
        },
    );
    let problems = dasio::cpu_problems(&new.cpus, &dasio::busy_cpus(&others(st)), st.cpu_count);
    let ok = !changed.is_empty() && problems.is_empty() && st.busy.is_none();
    enable(h, ID_APPLY, ok);
    if st.busy.is_none() {
        set_text(h, ID_RESULT, &problems.join("; "));
    }
}

/// Apply: the plan, then the worker.
fn start_apply(st: &mut State) {
    let h = st.hwnd;
    let busy = dasio::busy_cpus(&others(st));
    let tab = &st.tabs[st.cur];
    let Ok((old, new)) = &tab.fields else {
        return;
    };
    let plan = match dasio::plan(&tab.text, old, new, &busy, st.cpu_count) {
        Ok(p) => p,
        Err(e) => {
            set_text(h, ID_RESULT, &e);
            return;
        }
    };
    let (tx, rx) = mpsc::channel();
    let mut ops = WinOps {
        name: new.name.clone(),
        number: tab.inst.number,
        devices: st.devices.clone(),
        scanner: None,
        adapters: st.adapters.clone(),
    };
    let want = new.clone();
    let hwnd = h as isize;
    st.busy = Some(rx);
    for id in COMBOS {
        enable(h, id, false);
    }
    enable(h, ID_APPLY, false);
    let spawned = std::thread::Builder::new()
        .name("dtray-dasio-apply".into())
        .spawn(move || {
            let post = |p: Progress, tx: &Sender<Progress>| {
                if tx.send(p).is_ok() {
                    // SAFETY: a message to a window; failing (closed) is fine.
                    unsafe { PostMessageW(hwnd as HWND, WM_PROGRESS, 0, 0) };
                }
            };
            let out = apply::run(&plan, &want, &mut ops, |s| {
                post(Progress::Say(s.to_string()), &tx)
            });
            post(Progress::Done(out), &tx);
        });
    if let Err(e) = spawned {
        st.busy = None;
        set_text(h, ID_RESULT, &format!("the apply did not start: {e}"));
        show_tab(st);
    }
}

/// The worker's messages (WM_PROGRESS).
fn progress(st: &mut State) {
    let h = st.hwnd;
    let mut done = None;
    if let Some(rx) = &st.busy {
        while let Ok(p) = rx.try_recv() {
            match p {
                Progress::Say(s) => set_text(h, ID_RESULT, &s),
                Progress::Done(out) => done = Some(out),
            }
        }
    }
    if let Some(out) = done {
        st.busy = None;
        let text = match &out {
            Ok(line) => line.clone(),
            Err(e) => format!("Not applied: {e}"),
        };
        // The tab again from the file and the device, as they are now.
        let inst = st.tabs[st.cur].inst.clone();
        st.tabs[st.cur] = load_tab(inst, &st.devices);
        show_tab(st);
        set_text(h, ID_RESULT, &text);
    }
}

/// The apply's real steps.
struct WinOps {
    name: String,
    number: u32,
    /// The tray's devices at the apply's start.
    devices: Vec<Local>,
    /// After the restart: a discovery of its own (the device may come
    /// back on other ports).
    scanner: Option<Scanner>,
    adapters: Vec<Adapter>,
}

impl WinOps {
    fn device(&self) -> Option<Local> {
        let now = self.scanner.as_ref().map(Scanner::devices);
        let list = now.as_deref().unwrap_or(&self.devices);
        device_of(list, &self.name).cloned()
    }

    fn arc_call<T>(
        arc: SocketAddrV4,
        f: impl FnOnce(&mut dstack::arc::client::Udp<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        dtray::arc::call(arc, f)
    }
}

impl Ops for WinOps {
    fn set_latency(&mut self, ns: u32) -> Result<(), String> {
        let d = self
            .device()
            .ok_or_else(|| format!("{} is not running", self.name))?;
        Self::arc_call(d.arc, |c| {
            control::set_latency(c, ns).map_err(|e| e.to_string())
        })
        .map(|_| ())
    }

    fn daw_attached(&mut self) -> Option<bool> {
        let d = self.device()?;
        dasio::daw_attached(&dtray::probe::status(d.port).ok()?)
    }

    fn wait(&mut self) {
        std::thread::sleep(Duration::from_secs(1));
    }

    fn install(&mut self, config: &str) -> Result<String, String> {
        let staged: PathBuf = std::env::temp_dir().join(format!(
            "dtray-dasio-{}-{}.toml",
            self.number,
            std::process::id()
        ));
        std::fs::write(&staged, config).map_err(|e| format!("{}: {e}", staged.display()))?;
        let result = dasio::result_path(&staged);
        let _ = std::fs::remove_file(&result);
        let exe = std::env::current_exe()
            .map_err(|e| format!("the program's path: {e}"))?
            .with_file_name("dtray-cli.exe");
        let ran = dasio::system::run_elevated(
            &exe,
            &dasio::helper_params(self.number, &staged),
            Duration::from_secs(180),
        );
        let verdict = std::fs::read_to_string(&result).ok();
        let _ = std::fs::remove_file(&staged);
        let _ = std::fs::remove_file(&result);
        let code = ran?;
        match verdict {
            Some(text) => dasio::parse_result(&text),
            None => Err(format!(
                "the helper ended with code {code} and left no verdict"
            )),
        }?;
        // Find the restarted device afresh.
        let mut scanner = Scanner::new();
        let _ = scanner.refresh();
        self.scanner = Some(scanner);
        Ok("written".into())
    }

    fn read_back(&mut self) -> ReadBack {
        let Some(d) = self.device() else {
            return ReadBack::default();
        };
        let status = dtray::probe::status(d.port).ok();
        let labels = Self::arc_call(d.arc, |c| control::labels(c).map_err(|e| e.to_string())).ok();
        let settings =
            Self::arc_call(d.arc, |c| control::settings(c).map_err(|e| e.to_string())).ok();
        if let Ok(a) = dasio::system::adapters() {
            self.adapters = a;
        }
        ReadBack {
            block: status.as_deref().and_then(dasio::status_block),
            rx: labels.as_ref().map(|l| l.rx_count),
            tx: labels.as_ref().map(|l| l.tx_count),
            latency_ns: settings
                .and_then(|s| s.latency_configured_ns)
                .map(u64::from),
            iface: choices::alias_of(&self.adapters, *d.arc.ip()),
        }
    }
}

/// An owner-drawn CPU list's entry: grey for another instance's CPU
/// (item data 1). Stateless: it may run while the state is borrowed.
fn draw_item(d: &DRAWITEMSTRUCT) {
    let hdc: HDC = d.hDC;
    let selected = d.itemState & ODS_SELECTED != 0;
    // SAFETY: the system colours and the DC the system gave.
    unsafe {
        let bg = if selected {
            COLOR_HIGHLIGHT
        } else {
            COLOR_WINDOW
        };
        FillRect(hdc, &d.rcItem, GetSysColorBrush(bg));
        if d.itemID == u32::MAX {
            return;
        }
        let combo = d.hwndItem;
        let len = SendMessageW(combo, CB_GETLBTEXTLEN, d.itemID as usize, 0);
        let mut buf = vec![0u16; usize::try_from(len).unwrap_or(0) + 1];
        SendMessageW(
            combo,
            CB_GETLBTEXT,
            d.itemID as usize,
            buf.as_mut_ptr() as LPARAM,
        );
        let greyed = SendMessageW(combo, CB_GETITEMDATA, d.itemID as usize, 0) == 1;
        let fg = if selected {
            COLOR_HIGHLIGHTTEXT
        } else if greyed {
            COLOR_GRAYTEXT
        } else {
            COLOR_WINDOWTEXT
        };
        SetTextColor(hdc, GetSysColor(fg));
        SetBkMode(hdc, TRANSPARENT as i32);
        let mut r: RECT = d.rcItem;
        r.left += 4;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        DrawTextW(
            hdc,
            buf.as_ptr(),
            n as i32,
            &mut r,
            DT_SINGLELINE | DT_VCENTER,
        );
    }
}

/// The colours of the markers and the banner (stateless, as
/// [`draw_item`]).
fn static_colour(hdc: HDC, id: usize) -> HBRUSH {
    // SAFETY: the DC the system gave; system brushes and our one brush.
    unsafe {
        if id == ID_BANNER {
            SetTextColor(hdc, WARN);
            SetBkColor(hdc, WARN_BG);
            let b = WARN_BRUSH.with(|c| {
                if c.get().is_null() {
                    c.set(CreateSolidBrush(WARN_BG));
                }
                c.get()
            });
            return b;
        }
        if id == ID_LIVE {
            SetTextColor(hdc, OK);
        } else if (ID_RESTART..ID_RESTART + 10).contains(&id) {
            SetTextColor(hdc, WARN);
        }
        SetBkColor(hdc, GetSysColor(COLOR_WINDOW));
        GetSysColorBrush(COLOR_WINDOW)
    }
}

/// Runs `f` on the state when it is free (a message sent while another
/// handler holds it is left to the default handling).
fn with_state(f: impl FnOnce(&mut State)) -> bool {
    STATE.with(|s| match s.try_borrow_mut() {
        Ok(mut s) => match s.as_mut() {
            Some(st) => {
                f(st);
                true
            }
            None => false,
        },
        Err(_) => false,
    })
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let built = with_state(|st| {
                st.hwnd = h;
                build(h, st.font, &st.tabs);
                show_tab(st);
            });
            if built { 0 } else { -1 }
        }
        WM_MEASUREITEM => {
            // SAFETY: the system's structure for this message.
            let m = unsafe { &mut *(lp as *mut MEASUREITEMSTRUCT) };
            m.itemHeight = 18;
            1
        }
        WM_DRAWITEM => {
            // SAFETY: the system's structure for this message.
            draw_item(unsafe { &*(lp as *const DRAWITEMSTRUCT) });
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            // SAFETY: plain call on the child the system names.
            let id =
                unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetDlgCtrlID(lp as HWND) };
            static_colour(wp as HDC, usize::try_from(id).unwrap_or(0)) as LRESULT
        }
        WM_NOTIFY => {
            // SAFETY: the system's header for this message.
            let n = unsafe { &*(lp as *const NMHDR) };
            if n.idFrom == ID_TAB && n.code == TCN_SELCHANGE {
                with_state(|st| {
                    // SAFETY: our tab control.
                    let i = unsafe { SendMessageW(item(h, ID_TAB), TCM_GETCURSEL, 0, 0) };
                    if let Ok(i) = usize::try_from(i)
                        && i < st.tabs.len()
                    {
                        st.cur = i;
                        show_tab(st);
                    }
                });
            }
            0
        }
        WM_COMMAND => {
            let id = wp & 0xffff;
            let code = (wp >> 16) as u32;
            match id {
                ID_CANCEL if code == 0 => {
                    // SAFETY: our window.
                    unsafe { DestroyWindow(h) };
                }
                ID_APPLY if code == 0 => {
                    with_state(start_apply);
                }
                _ if code == CBN_SELCHANGE && COMBOS.contains(&id) => {
                    with_state(|st| {
                        read_controls(st);
                        refresh(st);
                    });
                }
                _ => {}
            }
            0
        }
        WM_PROGRESS => {
            with_state(progress);
            0
        }
        WM_CLOSE => {
            // SAFETY: our window.
            unsafe { DestroyWindow(h) };
            0
        }
        WM_DESTROY => {
            let standalone = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let standalone = s.as_ref().is_some_and(|st| st.standalone);
                *s = None;
                standalone
            });
            if standalone {
                // SAFETY: plain calls.
                unsafe {
                    KillTimer(std::ptr::null_mut(), TIMER);
                    PostQuitMessage(0);
                }
            }
            0
        }
        // SAFETY: the default handling.
        _ => unsafe { DefWindowProcW(h, msg, wp, lp) },
    }
}
