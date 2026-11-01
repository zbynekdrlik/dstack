use super::*;

/// An exit code as text: `ExitCode` has no equality.
fn code(c: ExitCode) -> String {
    format!("{c:?}")
}

#[test]
fn clock_polls_once_and_ends_on_every_platform() {
    // One poll of the local dantesync's time server: a line on stdout
    // whether it answers (dev boxes run one) or not (CI), exit 0. The
    // Windows CI job runs this, so the Windows binary is no stub.
    assert_eq!(code(clock("test", 1)), code(ExitCode::SUCCESS));
}

#[cfg(windows)]
#[test]
fn ctl_refuses_on_windows() {
    assert_eq!(
        code(ctl_request("test", Path::new("control.sock"), "status")),
        code(ExitCode::from(1))
    );
}

#[cfg(unix)]
#[test]
fn ctl_without_a_device_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        code(ctl_request("test", &dir.path().join("none.sock"), "status")),
        code(ExitCode::from(1))
    );
}
