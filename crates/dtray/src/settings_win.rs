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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use dstack::arc::control;
use dtray::dasio::apply::{self, Ops};
use dtray::dasio::choices::{self, Adapter, Choice};
use dtray::dasio::{self, Fields, Instance, ReadBack};
use dtray::discover::{self, Local, Reading, Scanner};
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::DeleteObject;
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
use windows_sys::Win32::UI::WindowsAndMessaging::IsDialogMessageW;
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

mod controls;
mod ops;
mod paint;

use controls::*;
use ops::WinOps;
use paint::*;

/// A static's styles (`SS_LEFT`, `SS_NOPREFIX`: an `&` shows as itself).
const SS_LEFT: u32 = 0;
const SS_NOPREFIX: u32 = 0x80;

/// The window's class.
const CLASS: &str = "dtray-dasio-settings";
/// The worker's progress is waiting in the channel.
const WM_PROGRESS: u32 = 0x8000 + 46; // WM_APP + #246
/// Esc in a window that `IsDialogMessageW` handles (`IDCANCEL`).
const IDCANCEL_ID: usize = 2;
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
    /// The running apply: its tab's index and its progress.
    busy: Option<(usize, Receiver<Progress>)>,
    /// `dtray --settings`: the window is the program, its close ends it.
    standalone: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
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
    // SAFETY: a thread timer, no window, no callback; its id is the one
    // returned (a thread timer's own).
    let timer = unsafe { SetTimer(std::ptr::null_mut(), TIMER, 1000, None) };
    // SAFETY: plain data.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: a valid MSG; all of this thread's messages.
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got == 0 || got == -1 {
            // SAFETY: the timer set above.
            unsafe { KillTimer(std::ptr::null_mut(), timer) };
            return Ok(());
        }
        if msg.message == WM_TIMER && msg.hwnd.is_null() {
            let _ = scanner.refresh();
            let devices = scanner.devices();
            let readings = discover::read(&devices);
            feed(&devices, &readings);
            continue;
        }
        if dialog_message(&msg) {
            continue;
        }
        // SAFETY: a message GetMessageW filled.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// The window's keyboard handling (Tab between the controls, Esc): `true`
/// when the message was the window's and is handled. The tray's loop
/// calls it too.
pub fn dialog_message(msg: &MSG) -> bool {
    match open_hwnd() {
        // SAFETY: our window and a message GetMessageW filled.
        Some(h) => unsafe { IsDialogMessageW(h, msg) != 0 },
        None => false,
    }
}

/// Whether an apply runs (the tray's Exit waits for it).
pub fn applying() -> bool {
    APPLYING.load(Ordering::Acquire)
}

/// An apply runs (process-wide: a window closed and opened again while
/// its worker runs starts no second one on the same files).
static APPLYING: AtomicBool = AtomicBool::new(false);

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
    let problems = dasio::cpu_problems(
        &dasio::changed_cpus(&old.cpus, &new.cpus),
        &dasio::busy_cpus(&others(st)),
        st.cpu_count,
    );
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
    if APPLYING.swap(true, Ordering::AcqRel) {
        set_text(
            h,
            ID_RESULT,
            "An apply is still running; wait for it to end.",
        );
        return;
    }
    let (tx, rx) = mpsc::channel();
    let mut ops = WinOps {
        name: new.name.clone(),
        number: tab.inst.number,
        devices: st.devices.clone(),
        scanner: None,
        adapters: st.adapters.clone(),
        owner: h as isize,
        helper_running: false,
    };
    let want = new.clone();
    let hwnd = h as isize;
    st.busy = Some((st.cur, rx));
    for id in COMBOS {
        enable(h, id, false);
    }
    enable(h, ID_APPLY, false);
    enable(h, ID_TAB, false);
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
            // A helper still running keeps the guard until dtray ends.
            if !ops.helper_running {
                APPLYING.store(false, Ordering::Release);
            }
            post(Progress::Done(out), &tx);
        });
    if let Err(e) = spawned {
        APPLYING.store(false, Ordering::Release);
        st.busy = None;
        enable(h, ID_TAB, true);
        show_tab(st);
        set_text(h, ID_RESULT, &format!("the apply did not start: {e}"));
    }
}

/// The worker's messages (WM_PROGRESS).
fn progress(st: &mut State) {
    let h = st.hwnd;
    let mut done = None;
    if let Some((_, rx)) = &st.busy {
        while let Ok(p) = rx.try_recv() {
            match p {
                Progress::Say(s) => set_text(h, ID_RESULT, &s),
                Progress::Done(out) => done = Some(out),
            }
        }
    }
    if let Some(out) = done {
        let applied = st.busy.take().map_or(st.cur, |(i, _)| i);
        enable(h, ID_TAB, true);
        let text = match &out {
            Ok(line) => line.clone(),
            Err(e) => format!("Not applied: {e}"),
        };
        // The tab again from the file and the device, as they are now.
        let inst = st.tabs[applied].inst.clone();
        st.tabs[applied] = load_tab(inst, &st.devices);
        st.cur = applied;
        show_tab(st);
        set_text(h, ID_RESULT, &text);
    }
}

/// Esc, Cancel or the close box: the window closes, unless an apply runs
/// (it ends with a restart and its read-back; the window shows them).
fn close(h: HWND) {
    let running = STATE.with(|s| {
        s.try_borrow()
            .map_or(true, |s| s.as_ref().is_some_and(|st| st.busy.is_some()))
    });
    if running {
        set_text(
            h,
            ID_RESULT,
            "An apply is running; the window can close when it ends.",
        );
    } else {
        // SAFETY: our window.
        unsafe { DestroyWindow(h) };
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
                ID_CANCEL | IDCANCEL_ID if code == 0 => close(h),
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
            close(h);
            0
        }
        WM_DESTROY => {
            let gone = STATE.with(|s| {
                let mut s = s.try_borrow_mut().ok()?;
                let st = s.take()?;
                Some((st.standalone, st.font))
            });
            if let Some((standalone, font)) = gone {
                if !font.is_null() {
                    // SAFETY: the font this window created.
                    unsafe { DeleteObject(font) };
                }
                if standalone {
                    // SAFETY: plain call.
                    unsafe { PostQuitMessage(0) };
                }
            }
            0
        }
        // SAFETY: the default handling.
        _ => unsafe { DefWindowProcW(h, msg, wp, lp) },
    }
}
