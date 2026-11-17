#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The CI peer gate's runner-stall witness (dstack #305).

A GitHub runner is a shared VM: now and then the host does not run it for
several ms. Two peers' pumps then wake late at the same wall time, and one
more than its margin W (6 ms on the normal profile) late re-bootstraps.
That is the runner, not dstack. A defect in the pump stalls only the
peer's own process. So a third process with no dstack code, the witness,
sleeps 1 ms in a loop and logs every wake at least WITNESS_MIN_US late.

`witness <file>`: run the witness, one line `<unix_ms> <late_us>` per late
wake (unix_ms at the wake, after the stall).

`void <witness file> <from_ms> <to_ms> <peer>=<rise>,<late rise>:<status.json>[:<log>] ...`
(`rise` the peer's `engine.rebootstraps` rise, `late rise` its
`engine.rebootstraps_late` rise, `log` its stderr): exit 0 when a
measuring window whose counts rose is VOID (the runner stalled us), 1 when
the rise stands (a failure), 2 for a usage or read error, with the reason
on stdout. Void only when at least one peer re-bootstrapped in the window,
every re-bootstrap was a late wake (no clock-back, re-anchor or re-time),
each peer's recent late wakes hold at least its late rise inside the
window, and every such re-bootstrap has runner evidence: it overlaps a
stall the witness saw, or its own pump's watch line shows the pump's sleep
returned that late (`overslept`). Other counters (tone gaps, holes, late
packets) may rise with it: a peer's restart is heard as a gap by the other.

The oversleep evidence (dstack #339, with v0.6.158's split, #354): the
pump watch's warning line, once a second, carries that second's latest
wake (`behind_max_us`) and its latest sleep return past the sleep's target
(`oversleep_max_us`). The pump's sleep target is the next block's wake
time, so a sleep that returned more than the margin W past it
re-bootstrapped, and an oversleep about the wake's whole lateness says the
thread was not run when its timer fired: the scheduler's miss, not dstack's
work. A thread's CPU time against its wall time is NOT evidence: on a
Hyper-V runner the host's steal counts as the thread's CPU (#354). A line
without the field (before v0.6.158) is no evidence. The oversleep is read
on the media clock, so a forward step of it would read as an oversleep
too: this holds only because the gate's peers run on the TEST system
clock (`--test-clock-ppm`; ci-peer.test.sh checks `clock.source ==
"system"`), the app's monotonic clock at a fixed skew, which never steps.
"""

import datetime
import json
import re
import sys
import time

# The witness's sleep and the lateness it logs, us.
WITNESS_SLEEP_US = 1000
WITNESS_MIN_US = 2000
# Slack between the pump's and the witness's stall intervals, ms (the two
# read the wall clock at different points of their wakes).
SLACK_MS = 2
# How far before the window's first read a re-bootstrap may lie and still
# be in its rise, ms: the status is rewritten once a second.
STATUS_AGE_MS = 1500
# How long after a re-bootstrap its pump watch line may be logged, ms: the
# watch looks once a second, on the daemon's tick.
WATCH_AFTER_MS = 2000
# How far the sleep's oversleep may fall short of the wake's lateness and
# still be the whole of it, us (the pump reads its clocks again after the
# sleep's return).
OVERSLEEP_SLACK_US = 500
# The peers' sample rate (ci-peer.test.sh runs the default), for the watch
# line's margin, which is in samples.
RATE_HZ = 48000

WATCH_LINE = re.compile(
    r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d+))?Z\s.*engine pump lost or overran blocks this second"
)


def step(sleep, monotonic_ns):
    """One sleep of the witness: how late it woke, us, when at least
    WITNESS_MIN_US late, else None."""
    before = monotonic_ns()
    sleep(WITNESS_SLEEP_US / 1e6)
    late_us = (monotonic_ns() - before) // 1000 - WITNESS_SLEEP_US
    return late_us if late_us >= WITNESS_MIN_US else None


def witness(path):
    """Log every wake of a 1 ms sleep loop at least WITNESS_MIN_US late."""
    with open(path, "a", encoding="ascii") as out:
        while True:
            late_us = step(time.sleep, time.monotonic_ns)
            if late_us is not None:
                out.write(f"{time.time_ns() // 1_000_000} {late_us}\n")
                out.flush()


def stalls(lines):
    """The witness's stalls as (start_ms, end_ms)."""
    out = []
    for line in lines:
        if line.strip():
            end_ms, late_us = (int(v) for v in line.split())
            out.append((end_ms - late_us / 1000, end_ms))
    return out


def rebootstraps(status, from_ms, to_ms):
    """A peer's recent re-bootstrapping wakes in the window, as
    (unix_ms, behind_us): the pump woke at unix_ms, behind_us late."""
    wakes = status["device"]["engine"]["late_wakes_recent"]
    return [
        (w["unix_ms"], w["behind_us"])
        for w in wakes
        if w["rebootstrap"] and from_ms - STATUS_AGE_MS <= w["unix_ms"] <= to_ms
    ]


def field(line, name):
    """A `name=<int>` field of a log line, or None."""
    m = re.search(rf"\b{name}=(\d+)\b", line)
    return int(m.group(1)) if m else None


def watch_lines(lines):
    """The pump watch's warning lines as dicts: `unix_ms` (logged at, UTC),
    `rebootstraps`, `cause`, `behind_max_us`, `oversleep_max_us` (None
    before v0.6.158) and `margin` (samples)."""
    out = []
    for line in lines:
        # A log written with colours keeps its fields between escapes.
        line = re.sub(r"\x1b\[[0-9;]*m", "", line)
        m = WATCH_LINE.match(line)
        if not m:
            continue
        at = datetime.datetime.fromisoformat(m.group(1)).replace(tzinfo=datetime.timezone.utc)
        # The fraction to whole ms, whatever its digits (tracing writes 6).
        frac_ms = int((m.group(2) or "0")[:3].ljust(3, "0"))
        cause = re.search(r"\bcause=(\S+)", line)
        out.append(
            {
                "unix_ms": int(at.timestamp()) * 1000 + frac_ms,
                "rebootstraps": field(line, "rebootstraps"),
                "cause": cause.group(1) if cause else None,
                "behind_max_us": field(line, "behind_max_us"),
                "oversleep_max_us": field(line, "oversleep_max_us"),
                "margin": field(line, "margin"),
            }
        )
    return out


def overslept(wake, watch):
    """The watch line showing this re-bootstrap's wake (`unix_ms`,
    `behind_us`) was a sleep the scheduler returned late, or None. The
    line, logged up to WATCH_AFTER_MS after the wake, must count one
    re-bootstrap only, caused by a late wake, name this wake as its
    second's latest (`behind_max_us` == `behind_us`), and show an
    oversleep past the margin W (any sleep returned past W re-bootstraps,
    so the oversleeping wake is this one) and within OVERSLEEP_SLACK_US of
    the wake's whole lateness. Per-second peaks are all the line carries,
    so a rebootstrap that shares its second with another, or whose sleep
    the watch took in the second before its wake's (the pump records the
    oversleep before the next block's lateness), stays unexplained: the
    gate leans to "the rise stands"."""
    unix_ms, behind_us = wake
    for w in watch:
        if not unix_ms < w["unix_ms"] <= unix_ms + WATCH_AFTER_MS:
            continue
        over, margin = w["oversleep_max_us"], w["margin"]
        if (
            w["rebootstraps"] == 1
            and w["cause"] == "late"
            and w["behind_max_us"] == behind_us
            and over is not None
            and margin is not None
            and over * RATE_HZ > margin * 1_000_000
            and over >= behind_us - OVERSLEEP_SLACK_US
        ):
            return w
    return None


def witnessed(interval, seen):
    """The interval overlaps a witness stall, within SLACK_MS."""
    start, end = interval
    return any(s - SLACK_MS <= end and start <= e + SLACK_MS for s, e in seen)


def void(seen, from_ms, to_ms, peers):
    """(void, reason) for a window whose counts rose: `peers` maps a name
    to (its rebootstrap rise, its late rebootstrap rise, its status, its
    pump watch lines)."""
    if not any(rise > 0 for rise, _, _, _ in peers.values()):
        return False, "no peer re-bootstrapped: the rise is not a runner stall"
    why = []
    for name, (rise, late, status, watch) in peers.items():
        if rise != late:
            return False, f"{name}: {rise - late} of {rise} re-bootstraps were not late wakes"
        found = rebootstraps(status, from_ms, to_ms)
        if len(found) < late:
            return False, f"{name}: {late} re-bootstraps rose, {len(found)} in its recent late wakes"
        for unix_ms, behind_us in found:
            if witnessed((unix_ms - behind_us / 1000, unix_ms), seen):
                why.append(f"{name}: re-bootstrap at {unix_ms} ms in a witnessed stall")
                continue
            line = overslept((unix_ms, behind_us), watch)
            if line is None:
                return False, (
                    f"{name}: re-bootstrap at {unix_ms} ms ({behind_us} us behind) not seen by "
                    "the witness, and no pump watch line shows its sleep returned that late"
                )
            why.append(
                f"{name}: re-bootstrap at {unix_ms} ms, {behind_us} us behind, its sleep "
                f"returned {line['oversleep_max_us']} us late (the scheduler's)"
            )
    return True, "every re-bootstrap has runner evidence: " + "; ".join(why)


def main(argv):
    """The command line: its exit code. Any error reading the input is 2,
    never 1 ("the rise stands")."""
    try:
        return run(argv)
    except Exception as e:  # noqa: BLE001 — every error is reported as exit 2
        print(f"gate error: {e!r}")
        return 2


def run(argv):
    if len(argv) == 3 and argv[1] == "witness":
        witness(argv[2])
        return 0
    if len(argv) >= 6 and argv[1] == "void":
        with open(argv[2], encoding="ascii") as f:
            seen = stalls(f)
        from_ms, to_ms = int(argv[3]), int(argv[4])
        peers = {}
        for arg in argv[5:]:
            name, rest = arg.split("=", 1)
            rises, *paths = rest.split(":")
            if len(paths) not in (1, 2):
                raise ValueError(f"{arg}: not <peer>=<rise>,<late>:<status>[:<log>]")
            rise, late = rises.split(",")
            with open(paths[0], encoding="utf-8") as f:
                status = json.load(f)
            watch = []
            if len(paths) == 2:
                with open(paths[1], encoding="utf-8", errors="replace") as f:
                    watch = watch_lines(f)
            peers[name] = (int(rise), int(late), status, watch)
        is_void, reason = void(seen, from_ms, to_ms, peers)
        print(reason)
        return 0 if is_void else 1
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
