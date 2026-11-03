// SPDX-License-Identifier: MIT OR Apache-2.0

//! The tray on Linux: the OS's glue around [`Tray`] (the icon and menu,
//! shared with Windows; `tray-icon`'s GTK backend shows it through
//! AppIndicator). GTK's main loop runs [`Tray::tick`] every [`TICK`] and
//! handles the menu's clicks every [`CLICKS`]. One tray a user session
//! (a lock on [`dtray::desktop::lock_path`]). Copy status goes to the GTK
//! clipboard; an error goes to stderr (the session's log).
//!
//! [`autostart`]: the XDG autostart entry for `dtray --autostart on|off`.

use std::cell::RefCell;
use std::fs::{File, OpenOptions, TryLockError};
use std::rc::Rc;
use std::time::Duration;

use glib::ControlFlow;

use dtray::desktop;

use crate::tray::{Action, Tray};

/// How often the devices are read.
const TICK: Duration = Duration::from_secs(1);
/// How often the menu's clicks are handled.
const CLICKS: Duration = Duration::from_millis(100);

/// An environment variable as a path, when set.
fn env_path(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(name).map(Into::into)
}

/// Takes the session's single-instance lock: the open file holding it, or
/// `None` when another dtray holds it.
fn single_instance() -> Result<Option<File>, String> {
    let path = desktop::lock_path(env_path("XDG_RUNTIME_DIR").as_deref())
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set: no user session".to_string())?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("the lock {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(format!("the lock {}: {e}", path.display())),
    }
}

/// Runs the tray until Exit; a second start in the session leaves at once.
/// Fails at once without a desktop (GTK cannot start).
pub fn run() -> Result<(), String> {
    gtk::init().map_err(|e| format!("no desktop to show the tray on (GTK): {e}"))?;
    let Some(_lock) = single_instance()? else {
        return Ok(());
    };
    let tray = Rc::new(RefCell::new(Tray::start()?));
    let failed: Rc<RefCell<Option<String>>> = Rc::default();
    {
        let (tray, failed) = (Rc::clone(&tray), Rc::clone(&failed));
        glib::timeout_add_local(TICK, move || match tray.borrow_mut().tick() {
            Ok(()) => ControlFlow::Continue,
            Err(e) => {
                *failed.borrow_mut() = Some(e);
                gtk::main_quit();
                ControlFlow::Break
            }
        });
    }
    glib::timeout_add_local(CLICKS, move || {
        for action in tray.borrow_mut().menu_events() {
            match action {
                Action::Exit => {
                    gtk::main_quit();
                    return ControlFlow::Break;
                }
                Action::Copy(text) => {
                    gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_text(&text);
                }
                Action::Error(err) => eprintln!("{err}"),
            }
        }
        ControlFlow::Continue
    });
    gtk::main();
    match failed.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Writes (`on`) or removes the user's autostart entry
/// ([`desktop::autostart_file`], [`desktop::desktop_entry`]); removing one
/// that is not there is done.
pub fn autostart(on: bool) -> Result<(), String> {
    let file = desktop::autostart_file(
        env_path("XDG_CONFIG_HOME").as_deref(),
        env_path("HOME").as_deref(),
    )
    .ok_or_else(|| "neither XDG_CONFIG_HOME nor HOME is set".to_string())?;
    if on {
        let exe = std::env::current_exe().map_err(|e| format!("the program's path: {e}"))?;
        let text = desktop::desktop_entry(&exe).ok_or_else(|| {
            "the program's path cannot be written in an autostart entry".to_string()
        })?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    } else {
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", file.display())),
        }
    }
    Ok(())
}
