//! Why the follower is not in use (dstack #420, strih-snv 18.11.2026: the
//! in-process follower started, the selector took the service, and for
//! 2.5 h nothing said whether the follower heard PTP, heard it and never
//! locked, or locked and never agreed with the service).
//!
//! While the follower is not the selected source, the selector keeps the
//! reason ([`waiting_of`]) for the status and logs [`NOT_IN_USE_LINE`] once
//! a minute ([`Minute`]); nothing once the follower is in use.

/// The follower not in use, once a minute, with its reason.
pub const NOT_IN_USE_LINE: &str = "media clock: the in-process follower is not in use";

/// How often [`NOT_IN_USE_LINE`] is logged, ns.
pub const NOT_IN_USE_EVERY_NS: i64 = 60_000_000_000;

/// Why the selector does not use the follower ([`waiting_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    /// Its receive thread ended.
    Stopped,
    /// No phase-lock window closed lately: it hears no Sync/Follow_Up
    /// pairs.
    NoPtp,
    /// It hears PTP but is not locked (dsync's lock gate: |e| within 50 µs
    /// for 3 windows).
    NotLocked,
    /// Locked, but its boot fit or a pull is not done: it takes over a
    /// ready service only once settled.
    NotSettled,
    /// Locked and settled: the selector waits for its time to agree with
    /// the service's ([`super::handover`]).
    WaitsToAgree,
}

impl Waiting {
    /// The status word.
    pub fn word(self) -> &'static str {
        match self {
            Waiting::Stopped => "stopped",
            Waiting::NoPtp => "no_ptp",
            Waiting::NotLocked => "not_locked",
            Waiting::NotSettled => "not_settled",
            Waiting::WaitsToAgree => "waits_to_agree",
        }
    }
}

/// Why a follower with the view `v` that is not in use waits, having
/// closed `heard` windows lately ([`Minute::round`]): its thread ended;
/// else it hears nothing; else not locked; else not settled; else (locked
/// and settled, yet not chosen) the selector waits for it to agree with
/// the service.
pub fn waiting_of(v: super::View, heard: u64) -> Waiting {
    if !v.alive {
        Waiting::Stopped
    } else if heard == 0 {
        Waiting::NoPtp
    } else if !v.locked {
        Waiting::NotLocked
    } else if !v.settled {
        Waiting::NotSettled
    } else {
        Waiting::WaitsToAgree
    }
}

/// The once-a-minute limiter of [`NOT_IN_USE_LINE`] and the windows the
/// follower closed in that minute. A period of waiting starts its own
/// minute; the follower in use ends it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Minute {
    /// The minute's start and the follower's windows then.
    mark: Option<(i64, u64)>,
    /// The windows of the last whole minute.
    last: Option<u64>,
}

impl Minute {
    /// One selector round at `now_ns` with the follower's `windows` so far,
    /// `waiting` while it is not in use. Returns the windows it closed
    /// lately (the last whole minute's or this minute's so far, the more;
    /// 0 while in use) and, when a line is due (a minute of waiting since
    /// the last), the windows of that minute.
    pub fn round(&mut self, now_ns: i64, windows: u64, waiting: bool) -> (u64, Option<u64>) {
        if !waiting {
            *self = Minute::default();
            return (0, None);
        }
        let (at, base) = *self.mark.get_or_insert((now_ns, windows));
        let since = windows.saturating_sub(base);
        let due = (now_ns.saturating_sub(at) >= NOT_IN_USE_EVERY_NS).then_some(since);
        if due.is_some() {
            self.mark = Some((now_ns, windows));
            self.last = due;
        }
        (self.last.unwrap_or(0).max(since), due)
    }
}

/// A grandmaster UUID as the log shows it (`00:1d:c1:ab:cd:ef`), `none`
/// before one.
pub fn grandmaster_text(gm: Option<[u8; 6]>) -> String {
    gm.map_or_else(
        || "none".to_owned(),
        |u| {
            u.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":")
        },
    )
}
