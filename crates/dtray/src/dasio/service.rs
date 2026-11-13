// SPDX-License-Identifier: MIT OR Apache-2.0

//! A dasio instance's Windows service restarted (the settings window's
//! elevated helper, dstack #246): through the Service Control Manager on
//! Windows (`service/windows.rs`, thin glue outside the mutation gate),
//! refused elsewhere.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::restart;

/// Off Windows there is no dasio service: the reason.
#[cfg(not(windows))]
pub fn restart(service: &str) -> Result<(), String> {
    Err(format!("{service}: dasio's services run on Windows only"))
}

#[cfg(test)]
mod tests {
    #[cfg(not(windows))]
    #[test]
    fn off_windows_a_restart_is_refused() {
        assert_eq!(
            super::restart("dasio-1"),
            Err("dasio-1: dasio's services run on Windows only".to_string())
        );
    }
}
