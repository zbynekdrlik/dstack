// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

use std::cell::{Cell, RefCell};

use super::*;

/// A `SCHED_OTHER` thread at nice 5 on a 2.8 ms default slice, its
/// reset-on-fork flag set beside a util-clamp flag (0x20).
const OTHER: Attr = Attr {
    policy: SCHED_OTHER,
    flags: 0x21,
    nice: 5,
    runtime: 2_800_000,
};

/// A fake kernel: `reads` answered in turn, every write recorded and
/// answered `set`.
struct Kernel {
    reads: RefCell<Vec<Result<Attr, i32>>>,
    writes: RefCell<Vec<Attr>>,
    set: Result<(), i32>,
}

impl Kernel {
    fn new(reads: Vec<Result<Attr, i32>>, set: Result<(), i32>) -> Kernel {
        Kernel {
            reads: RefCell::new(reads),
            writes: RefCell::new(Vec::new()),
            set,
        }
    }

    fn ask(&self, thread: MediaThread) -> SliceRequest {
        request(
            thread,
            || self.reads.borrow_mut().remove(0),
            |attr| {
                self.writes.borrow_mut().push(*attr);
                self.set
            },
        )
    }
}

#[test]
fn the_kernel_s_answer_and_the_read_back_give_the_outcome() {
    assert_eq!(slice_of(Ok(()), Some(SHORT_SLICE_NS)), Slice::Short);
    assert_eq!(slice_of(Ok(()), Some(SHORT_SLICE_NS - 1)), Slice::Ignored);
    assert_eq!(slice_of(Ok(()), Some(SHORT_SLICE_NS + 1)), Slice::Ignored);
    assert_eq!(slice_of(Ok(()), Some(0)), Slice::Ignored);
    assert_eq!(slice_of(Ok(()), None), Slice::Ignored);
    for read in [None, Some(SHORT_SLICE_NS), Some(2_800_000)] {
        assert_eq!(slice_of(Err(libc_einval()), read), Slice::Refused);
    }
}

/// EINVAL's number on Linux (the errno a kernel before 6.12 may give).
fn libc_einval() -> i32 {
    22
}

/// dstack #302: only the pump and the TX thread ask; the others are never
/// read.
#[test]
fn only_the_pump_and_the_tx_thread_ask_a_slice() {
    assert!(asks_slice(MediaThread::Pump));
    assert!(asks_slice(MediaThread::Tx));
    assert!(!asks_slice(MediaThread::TxRelay));
    assert!(!asks_slice(MediaThread::Rx));
    for thread in [MediaThread::Rx, MediaThread::TxRelay] {
        let asked = request(
            thread,
            || panic!("{thread:?} is never read"),
            |_| panic!("{thread:?} is never written"),
        );
        assert_eq!(asked, SliceRequest::NOT, "{thread:?}");
    }
}

/// The request keeps the policy, the nice and the reset-on-fork flag, sets
/// the slice, and reads it back.
#[test]
fn a_sched_other_thread_asks_the_short_slice_with_its_nice_kept() {
    let short = Attr {
        runtime: SHORT_SLICE_NS,
        ..OTHER
    };
    for thread in [MediaThread::Pump, MediaThread::Tx] {
        let kernel = Kernel::new(vec![Ok(OTHER), Ok(short)], Ok(()));
        assert_eq!(
            kernel.ask(thread),
            SliceRequest {
                slice: Slice::Short,
                read_ns: Some(SHORT_SLICE_NS),
                error: None,
            }
        );
        assert_eq!(
            *kernel.writes.borrow(),
            [Attr {
                policy: SCHED_OTHER,
                flags: RESET_ON_FORK,
                nice: 5,
                runtime: SHORT_SLICE_NS,
            }]
        );
        assert!(kernel.reads.borrow().is_empty(), "read back once");
    }
    // Without the reset-on-fork flag none is set.
    let plain = Attr {
        flags: 0x20,
        ..OTHER
    };
    let kernel = Kernel::new(vec![Ok(plain), Ok(short)], Ok(()));
    kernel.ask(MediaThread::Pump);
    assert_eq!(kernel.writes.borrow()[0].flags, 0);
}

/// iemmixer's low_latency: a thread that runs SCHED_FIFO (1), or any other
/// policy (SCHED_BATCH 3), is left as it is.
#[test]
fn a_thread_on_another_policy_is_never_written() {
    for policy in [1, 2, 3, 5, 6] {
        let attr = Attr { policy, ..OTHER };
        let asked = request(MediaThread::Pump, || Ok(attr), |_| panic!("written"));
        assert_eq!(asked, SliceRequest::NOT, "{policy}");
    }
}

/// A kernel that refuses (EINVAL before 6.12, EPERM): refused, its errno
/// and the slice still read back.
#[test]
fn a_refused_request_keeps_its_errno_and_reads_back() {
    for errno in [22, 1] {
        let kernel = Kernel::new(vec![Ok(OTHER), Ok(OTHER)], Err(errno));
        assert_eq!(
            kernel.ask(MediaThread::Tx),
            SliceRequest {
                slice: Slice::Refused,
                read_ns: Some(2_800_000),
                error: Some(errno),
            }
        );
        assert_eq!(kernel.writes.borrow().len(), 1);
    }
}

/// A kernel before 6.12 takes the request and reads back no slice (0).
#[test]
fn a_request_taken_but_not_read_back_is_ignored() {
    let old = Attr {
        runtime: 0,
        ..OTHER
    };
    let kernel = Kernel::new(vec![Ok(old), Ok(old)], Ok(()));
    assert_eq!(
        kernel.ask(MediaThread::Pump),
        SliceRequest {
            slice: Slice::Ignored,
            read_ns: Some(0),
            error: None,
        }
    );
    // The read-back fails: nothing read, still not short.
    let kernel = Kernel::new(vec![Ok(OTHER), Err(38)], Ok(()));
    assert_eq!(
        kernel.ask(MediaThread::Pump),
        SliceRequest {
            slice: Slice::Ignored,
            read_ns: None,
            error: None,
        }
    );
}

/// The first read refused (no sched_getattr, ENOSYS 38): refused, nothing
/// written, since the nice to keep is unknown.
#[test]
fn a_thread_whose_attributes_are_unreadable_is_refused_without_a_write() {
    let reads = Cell::new(0);
    let asked = request(
        MediaThread::Pump,
        || {
            reads.set(reads.get() + 1);
            Err(38)
        },
        |_| panic!("written"),
    );
    assert_eq!(
        asked,
        SliceRequest {
            slice: Slice::Refused,
            read_ns: None,
            error: Some(38),
        }
    );
    assert_eq!(reads.get(), 1);
}

#[test]
fn each_outcome_has_its_code_and_word() {
    for s in Slice::ALL {
        assert_eq!(Slice::of_code(s.code()), s, "{s:?}");
    }
    assert_eq!(Slice::of_code(4), Slice::NotAsked);
    assert_eq!(Slice::of_code(u8::MAX), Slice::NotAsked);
    assert_eq!(
        Slice::ALL.map(Slice::code),
        [0, 1, 2, 3],
        "the record's codes"
    );
    assert_eq!(
        Slice::ALL.map(Slice::word),
        ["not_asked", "short", "ignored", "refused"]
    );
}

#[test]
fn the_start_line_names_the_slice_asked_and_read() {
    assert_eq!(slice_line("engine pump", &SliceRequest::NOT), None);
    let short = SliceRequest {
        slice: Slice::Short,
        read_ns: Some(200_000),
        error: None,
    };
    assert_eq!(
        slice_line("engine pump", &short).unwrap(),
        "engine pump runs with a short EEVDF slice: 200 us asked, 200 us read back (dstack #302)"
    );
    let ignored = SliceRequest {
        slice: Slice::Ignored,
        read_ns: Some(2_800_999),
        error: None,
    };
    assert_eq!(
        slice_line("TX thread", &ignored).unwrap(),
        "TX thread asked a 200 us EEVDF slice; the kernel took it but reads back 2800 us \
         (no requested slices before Linux 6.12): it runs on the default slice (dstack #302)"
    );
    let refused = SliceRequest {
        slice: Slice::Refused,
        read_ns: None,
        error: Some(22),
    };
    assert_eq!(
        slice_line("TX thread", &refused).unwrap(),
        "TX thread asked a 200 us EEVDF slice; the kernel refused it (errno 22), reads back \
         nothing: it runs on the default slice (dstack #302)"
    );
    let no_errno = SliceRequest {
        error: None,
        ..refused
    };
    assert!(
        slice_line("TX thread", &no_errno)
            .unwrap()
            .contains("(errno -)")
    );
}

/// The kernel's release (`uname`), as `(major, minor)`.
#[cfg(target_os = "linux")]
fn kernel_release() -> (u32, u32) {
    // SAFETY: an all-zero utsname is valid plain data, filled by uname.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `u` is a valid utsname the call writes.
    assert_eq!(unsafe { libc::uname(&mut u) }, 0);
    // SAFETY: uname NUL-terminates `release`.
    let release = unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    major_minor(&release).unwrap_or_else(|| panic!("release {release:?}"))
}

/// `"7.0.0-42-generic"` -> `(7, 0)`.
#[cfg(target_os = "linux")]
fn major_minor(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.split(['.', '-']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(target_os = "linux")]
#[test]
fn a_kernel_release_parses_to_major_and_minor() {
    assert_eq!(major_minor("7.0.0-42-generic"), Some((7, 0)));
    assert_eq!(major_minor("6.8.0-1017-azure"), Some((6, 8)));
    assert_eq!(major_minor("6.12-rc1"), Some((6, 12)));
    assert_eq!(major_minor("x"), None);
}

/// Requested slices came with Linux 6.12.
#[cfg(target_os = "linux")]
fn kernel_takes_slices() -> bool {
    kernel_release() >= (6, 12)
}

/// The calling thread's `sched_attr` read with its own raw call (not the
/// code under test's): `(policy, nice, runtime)`.
#[cfg(target_os = "linux")]
fn own_attr() -> (u32, i32, u64) {
    let mut buf = [0u8; 56];
    // SAFETY: pid 0 is the calling thread; the kernel writes at most 56
    // bytes into `buf`.
    let r = unsafe { libc::syscall(libc::SYS_sched_getattr, 0, buf.as_mut_ptr(), 56u32, 0) };
    assert_eq!(r, 0, "{}", std::io::Error::last_os_error());
    let u32_at = |o: usize| u32::from_ne_bytes(buf[o..o + 4].try_into().unwrap());
    let runtime = u64::from_ne_bytes(buf[24..32].try_into().unwrap());
    (u32_at(4), u32_at(16) as i32, runtime)
}

/// dstack #302 on Linux: a thread started through `take_priority` (asking
/// the normal profile's priority, as dlink's pump and TX thread on Linux)
/// reads its short slice back with the kernel's own read, the record and
/// the start line say so, and its nice is kept. A kernel before 6.12 must
/// show the logged refusal path instead (ignored or refused, the default
/// slice still in place).
#[cfg(target_os = "linux")]
#[test]
fn a_pump_and_a_tx_thread_on_linux_read_their_short_slice_back() {
    use crate::clock::ThreadPriority;
    use crate::media::tx::priority::{Scheduling, take_priority_asked};

    let scheduling = Scheduling::new();
    for thread in [MediaThread::Pump, MediaThread::Tx, MediaThread::Rx] {
        let (before, after, log) = std::thread::scope(|s| {
            s.spawn(|| {
                let before = own_attr();
                let log = crate::test_log::capture_at(tracing::Level::INFO, || {
                    take_priority_asked(thread, 40, &scheduling, ThreadPriority::Normal);
                });
                (before, own_attr(), log)
            })
            .join()
            .unwrap()
        });
        let got = scheduling
            .threads()
            .into_iter()
            .find(|t| t.thread == thread)
            .unwrap();
        assert_eq!(before.0, SCHED_OTHER, "a fresh test thread");
        assert_eq!(
            (after.0, after.1),
            (before.0, before.1),
            "policy, nice kept"
        );
        if thread == MediaThread::Rx {
            assert_eq!((got.slice, got.slice_ns), (Slice::NotAsked, None));
            assert_eq!(after.2, before.2, "the RX thread's slice untouched");
            assert!(!log.contains("EEVDF"), "{log}");
        } else if kernel_takes_slices() {
            assert_eq!(after.2, SHORT_SLICE_NS, "{thread:?}");
            assert_eq!(
                (got.slice, got.slice_ns),
                (Slice::Short, Some(SHORT_SLICE_NS))
            );
            assert!(
                log.contains("short EEVDF slice: 200 us asked, 200 us read back"),
                "{log}"
            );
        } else {
            assert!(
                matches!(got.slice, Slice::Ignored | Slice::Refused),
                "{got:?}"
            );
            assert_eq!(after.2, before.2, "the default slice stays");
            assert_eq!(got.slice_ns, Some(after.2));
            assert!(log.contains("runs on the default slice"), "{log}");
        }
    }
}

/// A `SCHED_FIFO` grant (the real-time profile, where the box allows it)
/// is never asked a slice; where FIFO is refused the thread runs
/// SCHED_OTHER and asks one.
#[cfg(target_os = "linux")]
#[test]
fn a_real_time_thread_keeps_its_policy_and_asks_no_slice() {
    use crate::clock::ThreadPriority;
    use crate::media::tx::priority::{Priority, Scheduling, take_priority_asked};

    let scheduling = Scheduling::new();
    let after = std::thread::scope(|s| {
        s.spawn(|| {
            take_priority_asked(MediaThread::Pump, 40, &scheduling, ThreadPriority::Realtime);
            own_attr()
        })
        .join()
        .unwrap()
    });
    let got = scheduling.threads()[0];
    if got.outcome == Priority::Granted {
        assert_eq!(after.0, 1, "SCHED_FIFO kept");
        assert_eq!((got.slice, got.slice_ns), (Slice::NotAsked, None));
    } else {
        assert_eq!(after.0, SCHED_OTHER);
        assert_ne!(got.slice, Slice::NotAsked, "{got:?}");
    }
}

/// The Linux glue: the kernel's errno comes back (an unknown policy is
/// EINVAL), and a thread's own attributes read as its raw read does.
#[cfg(target_os = "linux")]
#[test]
fn the_linux_calls_answer_the_kernel_s_errno_and_read() {
    std::thread::spawn(|| {
        let attr = read_attr().unwrap();
        let own = own_attr();
        assert_eq!((attr.policy, attr.nice, attr.runtime), own);
        assert_eq!(write_attr(&Attr { policy: 99, ..attr }), Err(libc::EINVAL));
        assert_eq!(write_attr(&attr), Ok(()), "its own attributes again");
        assert_eq!(own_attr(), own);
    })
    .join()
    .unwrap();
}
