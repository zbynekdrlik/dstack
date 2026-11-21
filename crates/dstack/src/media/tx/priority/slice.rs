// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A media thread's EEVDF slice on Linux (dstack #302): the pump and the
//! TX thread ask the kernel for a short slice ([`SHORT_SLICE_NS`]) for
//! themselves, so a wake-up preempts an equal-weight thread that holds
//! their CPU (Linux 6.12 and later: a shorter requested slice gives an
//! earlier deadline). Under the default slice (2.8 ms on stream-snv) the
//! pump waited up to 5.5 ms in the run queue behind nice-0 work after an
//! on-time timer; with 200 us its wakes more than 3 ms late fell from 6.8
//! to 0.9 an hour (A/B on stream-snv, 21.11.2026).
//!
//! It is the thread's own attribute: `SCHED_OTHER` and its nice stay as
//! they are, no privilege is needed, nothing is asked of the app. A thread
//! that runs anything else (`SCHED_FIFO` under the real-time profile) is
//! never touched. The decision is [`request`] over the OS reads and writes
//! as closures; the Linux glue is [`read_attr`] and [`write_attr`].

use super::MediaThread;

/// The slice the pump and the TX thread ask, ns (dstack #302; the kernel
/// takes 100 us to 100 ms).
pub const SHORT_SLICE_NS: u64 = 200_000;

/// Linux's `SCHED_OTHER` (the normal, EEVDF-scheduled policy).
const SCHED_OTHER: u32 = 0;

/// `SCHED_FLAG_RESET_ON_FORK`: the one flag a request keeps from the read.
const RESET_ON_FORK: u64 = 0x01;

/// A thread's scheduling attributes as `sched_getattr` reads them, the
/// fields a slice request keeps or sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attr {
    /// `sched_policy`.
    pub policy: u32,
    /// `sched_flags`.
    pub flags: u64,
    /// `sched_nice`.
    pub nice: i32,
    /// `sched_runtime`: a `SCHED_OTHER` thread's slice, ns (6.12 and
    /// later; 0 before).
    pub runtime: u64,
}

/// What a thread's slice request gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slice {
    /// Not asked: off Linux, a thread that asks none (the RX thread, the
    /// TX relay), or one that does not run `SCHED_OTHER`.
    NotAsked,
    /// Asked, and read back as asked.
    Short,
    /// The kernel took the request but reads back another slice: a kernel
    /// before 6.12, which has no requested slices. The default slice runs.
    Ignored,
    /// The kernel refused the request or its attributes' read (EINVAL,
    /// EPERM, ...): the default slice runs.
    Refused,
}

impl Slice {
    /// Every outcome, in the order of [`Slice::code`].
    const ALL: [Slice; 4] = [
        Slice::NotAsked,
        Slice::Short,
        Slice::Ignored,
        Slice::Refused,
    ];

    /// Its code in a [`super::Scheduling`] slot, 0 to 3.
    pub(super) fn code(self) -> u8 {
        match self {
            Slice::NotAsked => 0,
            Slice::Short => 1,
            Slice::Ignored => 2,
            Slice::Refused => 3,
        }
    }

    /// The outcome of `code` ([`Slice::code`]); [`Slice::NotAsked`] for
    /// any other.
    pub(super) fn of_code(code: u8) -> Slice {
        Slice::ALL
            .into_iter()
            .find(|s| s.code() == code)
            .unwrap_or(Slice::NotAsked)
    }

    /// The status word: `not_asked`, `short`, `ignored`, `refused`.
    pub fn word(self) -> &'static str {
        match self {
            Slice::NotAsked => "not_asked",
            Slice::Short => "short",
            Slice::Ignored => "ignored",
            Slice::Refused => "refused",
        }
    }
}

/// A slice request's result: the outcome, the slice read back after it
/// (ns, `None` when not read) and the errno of a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SliceRequest {
    /// What the request gave.
    pub slice: Slice,
    /// The thread's slice read back after the request
    /// (`sched_runtime`, ns); `None` when not read.
    pub read_ns: Option<u64>,
    /// The errno of a refused request or a refused first read.
    pub error: Option<i32>,
}

impl SliceRequest {
    /// Nothing asked.
    pub const NOT: SliceRequest = SliceRequest {
        slice: Slice::NotAsked,
        read_ns: None,
        error: None,
    };
}

/// Whether `thread` asks a short slice: the pump and the TX thread.
pub(crate) fn asks_slice(thread: MediaThread) -> bool {
    matches!(thread, MediaThread::Pump | MediaThread::Tx)
}

/// What a write's answer `set` and the slice read back after it (`read`)
/// mean: refused when the write was, short when the read-back is the
/// asked slice, else ignored.
pub(crate) fn slice_of(set: Result<(), i32>, read: Option<u64>) -> Slice {
    match (set, read) {
        (Err(_), _) => Slice::Refused,
        (Ok(()), Some(SHORT_SLICE_NS)) => Slice::Short,
        (Ok(()), _) => Slice::Ignored,
    }
}

/// The calling `thread`'s slice request over the OS calls `read`
/// (`sched_getattr`) and `write` (`sched_setattr`), each an errno on
/// failure: for a thread that asks one ([`asks_slice`]) and runs
/// `SCHED_OTHER`, the same policy, nice and reset-on-fork flag with
/// `sched_runtime` = [`SHORT_SLICE_NS`], then read back. A first read that
/// fails is a refusal with nothing written; any other policy is left as
/// it is.
pub(crate) fn request(
    thread: MediaThread,
    read: impl Fn() -> Result<Attr, i32>,
    write: impl Fn(&Attr) -> Result<(), i32>,
) -> SliceRequest {
    if !asks_slice(thread) {
        return SliceRequest::NOT;
    }
    let before = match read() {
        Ok(attr) => attr,
        Err(error) => {
            return SliceRequest {
                slice: Slice::Refused,
                read_ns: None,
                error: Some(error),
            };
        }
    };
    if before.policy != SCHED_OTHER {
        return SliceRequest::NOT;
    }
    let set = write(&Attr {
        flags: before.flags & RESET_ON_FORK,
        runtime: SHORT_SLICE_NS,
        ..before
    });
    let read_ns = read().ok().map(|attr| attr.runtime);
    SliceRequest {
        slice: slice_of(set, read_ns),
        read_ns,
        error: set.err(),
    }
}

/// The start line of `thread` (its log name) for `asked`: `None` when no
/// slice was asked.
pub(crate) fn slice_line(thread: &str, asked: &SliceRequest) -> Option<String> {
    let asked_us = SHORT_SLICE_NS / 1000;
    let read = asked
        .read_ns
        .map_or_else(|| "nothing".to_string(), |ns| format!("{} us", ns / 1000));
    match asked.slice {
        Slice::NotAsked => None,
        Slice::Short => Some(format!(
            "{thread} runs with a short EEVDF slice: {asked_us} us asked, {read} read back (dstack #302)"
        )),
        Slice::Ignored => Some(format!(
            "{thread} asked a {asked_us} us EEVDF slice; the kernel took it but reads back {read} \
             (no requested slices before Linux 6.12): it runs on the default slice (dstack #302)"
        )),
        Slice::Refused => Some(format!(
            "{thread} asked a {asked_us} us EEVDF slice; the kernel refused it (errno {}), reads back \
             {read}: it runs on the default slice (dstack #302)",
            asked
                .error
                .map_or_else(|| "-".to_string(), |e| e.to_string())
        )),
    }
}

/// The calling thread's slice request on Linux ([`request`] over the
/// kernel's calls).
#[cfg(target_os = "linux")]
pub(crate) fn ask(thread: MediaThread) -> SliceRequest {
    request(thread, read_attr, write_attr)
}

/// `sched_attr`'s first version, the fields a slice request needs.
#[cfg(target_os = "linux")]
const ATTR_SIZE: u32 = 48;
#[cfg(target_os = "linux")]
const _: () = assert!(std::mem::size_of::<libc::sched_attr>() == ATTR_SIZE as usize);

/// The calling thread's attributes (`sched_getattr`); its errno on
/// failure.
#[cfg(target_os = "linux")]
pub(crate) fn read_attr() -> Result<Attr, i32> {
    // SAFETY: an all-zero sched_attr is valid plain data.
    let mut attr: libc::sched_attr = unsafe { std::mem::zeroed() };
    // SAFETY: pid 0 is the calling thread; the kernel writes at most
    // ATTR_SIZE bytes into `attr`, which is that size.
    let r = unsafe { libc::syscall(libc::SYS_sched_getattr, 0, &raw mut attr, ATTR_SIZE, 0) };
    if r != 0 {
        return Err(errno());
    }
    Ok(Attr {
        policy: attr.sched_policy,
        flags: attr.sched_flags,
        nice: attr.sched_nice,
        runtime: attr.sched_runtime,
    })
}

/// Sets the calling thread's attributes to `want` (`sched_setattr`); its
/// errno on failure.
#[cfg(target_os = "linux")]
pub(crate) fn write_attr(want: &Attr) -> Result<(), i32> {
    // SAFETY: an all-zero sched_attr is valid plain data.
    let mut attr: libc::sched_attr = unsafe { std::mem::zeroed() };
    attr.size = ATTR_SIZE;
    attr.sched_policy = want.policy;
    attr.sched_flags = want.flags;
    attr.sched_nice = want.nice;
    attr.sched_runtime = want.runtime;
    // SAFETY: pid 0 is the calling thread; `attr` is a valid sched_attr of
    // its stated size, only read.
    let r = unsafe { libc::syscall(libc::SYS_sched_setattr, 0, &raw const attr, 0) };
    if r != 0 {
        return Err(errno());
    }
    Ok(())
}

/// The calling thread's last errno.
#[cfg(target_os = "linux")]
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(test)]
mod tests;
