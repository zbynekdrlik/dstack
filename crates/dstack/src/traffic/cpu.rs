// SPDX-License-Identifier: MIT OR Apache-2.0

//! The CPU times the traffic recorder samples each second (dstack #195):
//! this process's and the whole machine's ([`super::recorder::CpuTimes`]).
//! Linux reads `/proc/stat` and `/proc/self/stat` ([`machine_times`] and
//! [`process_ticks`] parse them, pure); Windows asks `GetSystemTimes` and
//! `GetProcessTimes` (`traffic/windows.rs`).

#[cfg(not(windows))]
use super::recorder::CpuTimes;

/// The machine's busy and total time from `/proc/stat`'s text, in clock
/// ticks over every CPU: its `cpu` line's user, nice, system, idle, iowait,
/// irq, softirq and steal (guest time is inside user already); idle and
/// iowait are not busy. `None` when the line is missing or short.
pub fn machine_times(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [user, nice, system, idle, iowait, irq, softirq, steal] = fields[..] else {
        return None;
    };
    let busy = user + nice + system + irq + softirq + steal;
    Some((busy, busy + idle + iowait))
}

/// This process's user and kernel time from `/proc/self/stat`'s text, in
/// clock ticks: fields 14 and 15, counted after the command name's last
/// `)` (the name may hold spaces and parentheses). `None` when unreadable.
pub fn process_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace().skip(11);
    let user: u64 = fields.next()?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    Some(user + system)
}

/// The CPU times now; `None` when they cannot be read.
#[cfg(target_os = "linux")]
pub fn times() -> Option<CpuTimes> {
    let (busy, total) = machine_times(&std::fs::read_to_string("/proc/stat").ok()?)?;
    let process = process_ticks(&std::fs::read_to_string("/proc/self/stat").ok()?)?;
    Some(CpuTimes {
        process,
        busy,
        total,
    })
}

#[cfg(windows)]
pub use super::windows::cpu_times as times;

#[cfg(not(any(target_os = "linux", windows)))]
pub use unread_times as times;

/// The CPU times now; none on this OS (dstack builds for Linux and Windows;
/// excluded from the gate, which never compiles it).
#[cfg(not(any(target_os = "linux", windows)))]
pub fn unread_times() -> Option<CpuTimes> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_line_gives_busy_and_total() {
        let stat = "cpu  10 2 3 100 5 1 1 4 7 0\ncpu0 1 1 1 1 1 1 1 1 0 0\nintr 5\n";
        // busy: 10 + 2 + 3 + 1 + 1 + 4; idle and iowait: 100 + 5.
        assert_eq!(machine_times(stat), Some((21, 126)));
        // A per-CPU line alone, a short line or a word: none.
        assert_eq!(machine_times("cpu0 1 1 1 1 1 1 1 1\n"), None);
        assert_eq!(machine_times("cpu  1 2 3 4 5 6 7\n"), None);
        assert_eq!(machine_times("cpu  1 2 3 x 5 6 7 8\n"), None);
    }

    #[test]
    fn the_process_line_gives_user_and_kernel_past_the_name() {
        // Fields 14 (utime) 40 and 15 (stime) 2; the name holds ") (".
        let stat = "123 (a) (b c) S 1 2 3 4 5 6 7 8 9 10 40 2 0 0 20 0 1";
        assert_eq!(process_ticks(stat), Some(42));
        assert_eq!(process_ticks("123 (a) S 1 2 3"), None);
        assert_eq!(process_ticks("123 a S"), None);
        assert_eq!(process_ticks("1 (a) S 1 2 3 4 5 6 7 8 9 10 x 2"), None);
        assert_eq!(process_ticks("1 (a) S 1 2 3 4 5 6 7 8 9 10 4 y"), None);
    }

    /// The real read on this OS: the machine's total covers its busy time,
    /// and this process has run (Windows CI runs it through its own call).
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn the_cpu_times_read_here() {
        let t = times().expect("the CPU times are readable");
        assert!(t.total > 0 && t.busy <= t.total, "{t:?}");
        // Spin until this process's own time moves (it starts at 0 on a
        // fast box within a tick).
        let start = std::time::Instant::now();
        while times().unwrap().process == 0 {
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
        }
        assert!(times().unwrap().process <= times().unwrap().total);
    }
}
