// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Service Control Manager's restart of a service: stop it, wait for
//! STOPPED, start it, wait for RUNNING, each wait bounded by [`WAIT`].
//! Windows-only glue, outside the mutation gate (`.cargo/mutants.toml`);
//! the helper around it ([`super::super::apply_staged`]) is tested.

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_SERVICE_NOT_ACTIVE, GetLastError};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
    SC_HANDLE, SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_START, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STOP, SERVICE_STOPPED,
    StartServiceW,
};

/// How long the stop and the start each may take.
pub const WAIT: Duration = Duration::from_secs(30);
/// The pause between two status reads.
const POLL: Duration = Duration::from_millis(200);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A service handle closed when dropped.
struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle the SCM gave and nothing else closes.
        unsafe { CloseServiceHandle(self.0) };
    }
}

/// The last OS error as text.
fn last_error(what: &str) -> String {
    // SAFETY: plain call, right after the failed one.
    let code = unsafe { GetLastError() };
    format!("{what} failed (error {code})")
}

/// The service's status now.
fn status(svc: &Handle) -> Result<SERVICE_STATUS, String> {
    // SAFETY: SERVICE_STATUS is plain data.
    let mut st: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: a valid handle with SERVICE_QUERY_STATUS and a writable status.
    if unsafe { QueryServiceStatus(svc.0, &mut st) } == 0 {
        return Err(last_error("QueryServiceStatus"));
    }
    Ok(st)
}

/// Waits until the service is in `want`; the reason when [`WAIT`] passed
/// or, waiting for RUNNING, it stopped (its exit codes).
fn wait_for(svc: &Handle, want: SERVICE_STATUS_CURRENT_STATE) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let st = status(svc)?;
        if st.dwCurrentState == want {
            return Ok(());
        }
        if want == SERVICE_RUNNING && st.dwCurrentState == SERVICE_STOPPED {
            return Err(format!(
                "it stopped at its start (exit code {}, service code {})",
                st.dwWin32ExitCode, st.dwServiceSpecificExitCode
            ));
        }
        if WAIT.saturating_sub(start.elapsed()).is_zero() {
            return Err(format!(
                "it is still in state {} after {} s",
                st.dwCurrentState,
                WAIT.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// Stops the service `name` (one already stopped is fine) and starts it
/// again; the reason otherwise.
pub fn restart(name: &str) -> Result<(), String> {
    // SAFETY: the local SCM, connect access only.
    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if scm.is_null() {
        return Err(last_error("OpenSCManagerW"));
    }
    let scm = Handle(scm);
    let w = wide(name);
    // SAFETY: a valid SCM handle and NUL-terminated name.
    let svc = unsafe {
        OpenServiceW(
            scm.0,
            w.as_ptr(),
            SERVICE_STOP | SERVICE_START | SERVICE_QUERY_STATUS,
        )
    };
    if svc.is_null() {
        return Err(last_error(&format!("opening the service {name}")));
    }
    let svc = Handle(svc);
    // SAFETY: SERVICE_STATUS is plain data.
    let mut st: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: a valid handle with SERVICE_STOP and a writable status.
    if unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut st) } == 0 {
        // SAFETY: plain call, right after the failed one.
        let code = unsafe { GetLastError() };
        if code != ERROR_SERVICE_NOT_ACTIVE {
            return Err(format!("stopping {name} failed (error {code})"));
        }
    }
    wait_for(&svc, SERVICE_STOPPED).map_err(|e| format!("stopping {name}: {e}"))?;
    // SAFETY: a valid handle with SERVICE_START; no arguments.
    if unsafe { StartServiceW(svc.0, 0, std::ptr::null()) } == 0 {
        return Err(last_error(&format!("starting {name}")));
    }
    wait_for(&svc, SERVICE_RUNNING).map_err(|e| format!("starting {name}: {e}"))
}
