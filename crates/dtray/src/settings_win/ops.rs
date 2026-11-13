// SPDX-License-Identifier: MIT OR Apache-2.0

//! The apply's real steps on the PC ([`Ops`] for the worker thread):
//! the device's ARC and status, the elevated helper, the read-back.

use super::*;

/// The apply's real steps.
pub(super) struct WinOps {
    pub(super) name: String,
    pub(super) number: u32,
    /// The tray's devices at the apply's start.
    pub(super) devices: Vec<Local>,
    /// After the restart: a discovery of its own (the device may come
    /// back on other ports).
    pub(super) scanner: Option<Scanner>,
    pub(super) adapters: Vec<Adapter>,
    /// The window the UAC prompt opens in front of.
    pub(super) owner: isize,
    /// The helper did not end in time: it may still be running.
    pub(super) helper_running: bool,
}

/// The new config in a file of its own in the user's temp folder (a new
/// name each time, never an existing file); its path.
fn stage(number: u32, config: &str) -> Result<PathBuf, String> {
    use std::io::Write;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = std::env::temp_dir().join(format!(
        "dtray-dasio-{number}-{}-{nanos}.toml",
        std::process::id()
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(config.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
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
        let exe = std::env::current_exe()
            .map_err(|e| format!("the program's path: {e}"))?
            .with_file_name("dtray-cli.exe");
        let program_files = dasio::system::program_files().map(PathBuf::from);
        dasio::may_elevate(&exe, program_files.as_deref())?;
        let staged = stage(self.number, config)?;
        let ran = dasio::system::run_elevated(
            &exe,
            &dasio::helper_params(self.number, &staged),
            Duration::from_secs(180),
            self.owner as HWND,
        );
        let _ = std::fs::remove_file(&staged);
        let code = match ran {
            Ok(code) => code,
            Err(e) if e.starts_with("the helper did not end") => {
                // It may still write and restart: no other apply until
                // dtray restarts (the worker keeps the guard).
                self.helper_running = true;
                return Err(format!(
                    "{e}; it may still be writing and restarting, so wait before the next apply"
                ));
            }
            Err(e) => return Err(e),
        };
        // The helper's verdict, in the instance's (admin-only) folder.
        let result = dasio::result_path(&dasio::root(), self.number);
        match std::fs::read_to_string(&result) {
            // A verdict counts only with the helper's own exit code.
            Ok(text) => match (code, dasio::parse_result(&text)) {
                (0, Ok(line)) => line,
                (_, Err(e)) => return Err(e),
                (_, Ok(_)) => {
                    return Err(format!("the helper ended with code {code}"));
                }
            },
            Err(_) => {
                return Err(format!(
                    "the helper ended with code {code} and left no verdict"
                ));
            }
        };
        // Find the restarted device afresh (it may come back on other
        // ports).
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
