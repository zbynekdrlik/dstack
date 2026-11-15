// SPDX-License-Identifier: MIT OR Apache-2.0

//! The processes on the Dante NIC's address, for the traffic recorder's
//! dump (dstack #195): which programs hold TCP or UDP endpoints there, the
//! most first, with their CPU. Windows reads the endpoint tables through
//! IP Helper (`traffic/windows.rs`, off the media path, once a burst); the
//! ranking and the text are pure, here.

/// How many processes a dump names.
pub const TOP: usize = 5;

/// A process on the NIC's address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// Its executable's file name; empty when unreadable.
    pub name: String,
    /// Its TCP and UDP endpoints on the address.
    pub endpoints: usize,
    /// Its CPU time since it started, ms; `None` when unreadable.
    pub cpu_ms: Option<u64>,
}

/// The `TOP` processes holding the most of `owners` (an endpoint's owner
/// pid each), most first, ties by pid; with their endpoint counts. The
/// pids in `skip` are left out: this process (its own Dante sockets would
/// crowd the list) and pid 0 (Windows's owner of TIME_WAIT rows).
pub fn rank(owners: impl IntoIterator<Item = u32>, skip: &[u32]) -> Vec<(u32, usize)> {
    let mut counts = std::collections::BTreeMap::<u32, usize>::new();
    for pid in owners.into_iter().filter(|p| !skip.contains(p)) {
        *counts.entry(pid).or_default() += 1;
    }
    let mut out: Vec<(u32, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out.truncate(TOP);
    out
}

/// The processes as the dump shows them, `, ` between:
/// `<name>(<pid>) endpoints=<n> cpu_ms=<ms>` (`?` for an unread name or
/// time); `none` for no process.
pub fn text(processes: &[Process]) -> String {
    if processes.is_empty() {
        return String::from("none");
    }
    processes
        .iter()
        .map(|p| {
            format!(
                "{}({}) endpoints={} cpu_ms={}",
                if p.name.is_empty() { "?" } else { &p.name },
                p.pid,
                p.endpoints,
                p.cpu_ms
                    .map_or_else(|| String::from("?"), |v| v.to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The file name of the executable path `path` (the last `\` or `/`
/// part).
pub fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// The pids [`rank`] leaves out here: pid 0 and this process.
pub fn skipped() -> [u32; 2] {
    [0, std::process::id()]
}

/// The processes with endpoints on the address `ip`, as [`text`] shows
/// them ([`NOT_READ`] where this OS has no read: Windows only).
pub fn on_address(ip: std::net::Ipv4Addr) -> String {
    read(ip).map_or_else(|| String::from(NOT_READ), |p| text(&p))
}

/// What a dump names for the processes off Windows.
pub const NOT_READ: &str = "not read on this OS";

#[cfg(windows)]
use super::windows::processes_on as read;

/// The processes on `_ip`: no read on this OS.
#[cfg(not(windows))]
fn read(_ip: std::net::Ipv4Addr) -> Option<Vec<Process>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rank_counts_owners_and_keeps_the_top_five() {
        let owners = [7, 3, 7, 9, 3, 7, 1, 2, 4, 9, 5, 5];
        assert_eq!(
            rank(owners, &[]),
            vec![(7, 3), (3, 2), (5, 2), (9, 2), (1, 1)]
        );
        assert_eq!(rank([], &[]), vec![]);
        assert_eq!(rank([4, 4], &[]), vec![(4, 2)]);
        // Skipped pids never count: the next ones move up.
        assert_eq!(
            rank(owners, &[7, 0]),
            vec![(3, 2), (5, 2), (9, 2), (1, 1), (2, 1)]
        );
    }

    #[test]
    fn pid_0_and_this_process_are_skipped() {
        assert_eq!(skipped(), [0, std::process::id()]);
    }

    #[test]
    fn the_text_names_each_process_or_none() {
        let p = |pid, name: &str, endpoints, cpu_ms| Process {
            pid,
            name: name.to_string(),
            endpoints,
            cpu_ms,
        };
        assert_eq!(text(&[]), "none");
        assert_eq!(
            text(&[p(12, "web.exe", 40, Some(3500)), p(4, "", 2, None)]),
            "web.exe(12) endpoints=40 cpu_ms=3500, ?(4) endpoints=2 cpu_ms=?"
        );
    }

    #[test]
    fn the_file_name_is_the_last_part() {
        assert_eq!(file_name(r"C:\Program Files\A\a b.exe"), "a b.exe");
        assert_eq!(file_name("/usr/bin/x"), "x");
        assert_eq!(file_name("plain"), "plain");
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_the_processes_are_not_read() {
        assert_eq!(on_address(std::net::Ipv4Addr::LOCALHOST), NOT_READ);
    }

    /// The real read: a child process holding a UDP socket on 127.0.0.1
    /// is listed with its name (Windows CI; this process is skipped).
    #[cfg(windows)]
    #[test]
    fn on_windows_a_process_with_a_socket_on_the_address_is_named() {
        let mut child = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "$s = 1..30 | % { [System.Net.Sockets.UdpClient]::new([System.Net.IPEndPoint]::new([System.Net.IPAddress]::Loopback, 0)) }; Start-Sleep 30",
            ])
            .spawn()
            .unwrap();
        let pid = child.id();
        let start = std::time::Instant::now();
        let found = loop {
            let found = read(std::net::Ipv4Addr::LOCALHOST).expect("read on Windows");
            if found.iter().any(|p| p.pid == pid) || start.elapsed().as_secs() > 20 {
                break found;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        let _ = child.kill();
        let _ = child.wait();
        let p = found
            .iter()
            .find(|p| p.pid == pid)
            .expect("the child listed");
        assert!(p.endpoints >= 30, "{p:?}");
        assert!(p.name.eq_ignore_ascii_case("powershell.exe"), "{p:?}");
        assert!(p.cpu_ms.is_some(), "{p:?}");
        assert!(
            found.iter().all(|p| !skipped().contains(&p.pid)),
            "{found:?}"
        );
    }
}
