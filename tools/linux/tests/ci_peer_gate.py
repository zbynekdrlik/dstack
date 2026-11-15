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

`void <witness file> <from_ms> <to_ms> <peer>=<rise>,<late rise>:<status.json> ...`
(`rise` the peer's `engine.rebootstraps` rise, `late rise` its
`engine.rebootstraps_late` rise): exit 0 when a measuring window whose
counts rose is VOID (the runner stalled us), 1 when the rise stands (a
failure), 2 for a usage or read error, with the reason on stdout. Void
only when at least one peer re-bootstrapped in the window, every
re-bootstrap was a late wake (no clock-back, re-anchor or re-time), each
peer's recent late wakes hold at least its late rise inside the window,
and every such re-bootstrap overlaps a stall the witness saw. Other
counters (tone gaps, holes, late packets) may rise with it: a peer's
restart is heard as a gap by the other.
"""

import json
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
    (start_ms, end_ms): the pump woke at unix_ms, behind_us late."""
    wakes = status["device"]["engine"]["late_wakes_recent"]
    return [
        (w["unix_ms"] - w["behind_us"] / 1000, w["unix_ms"])
        for w in wakes
        if w["rebootstrap"] and from_ms - STATUS_AGE_MS <= w["unix_ms"] <= to_ms
    ]


def witnessed(interval, seen):
    """The interval overlaps a witness stall, within SLACK_MS."""
    start, end = interval
    return any(s - SLACK_MS <= end and start <= e + SLACK_MS for s, e in seen)


def void(seen, from_ms, to_ms, peers):
    """(void, reason) for a window whose counts rose: `peers` maps a name
    to (its rebootstrap rise, its late rebootstrap rise, its status)."""
    if not any(rise > 0 for rise, _, _ in peers.values()):
        return False, "no peer re-bootstrapped: the rise is not a runner stall"
    for name, (rise, late, status) in peers.items():
        if rise != late:
            return False, f"{name}: {rise - late} of {rise} re-bootstraps were not late wakes"
        found = rebootstraps(status, from_ms, to_ms)
        if len(found) < late:
            return False, f"{name}: {late} re-bootstraps rose, {len(found)} in its recent late wakes"
        for interval in found:
            if not witnessed(interval, seen):
                return False, f"{name}: re-bootstrap at {interval[1]} ms not seen by the witness"
    return True, "every re-bootstrap fell in a runner stall the witness saw"


def main(argv):
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
            rises, path = rest.split(":", 1)
            rise, late = rises.split(",")
            with open(path, encoding="utf-8") as f:
                peers[name] = (int(rise), int(late), json.load(f))
        is_void, reason = void(seen, from_ms, to_ms, peers)
        print(reason)
        return 0 if is_void else 1
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    try:
        CODE = main(sys.argv)
    except (OSError, ValueError, KeyError, TypeError) as e:
        # A read or usage error is never "the rise stands" (exit 1).
        print(f"gate error: {e!r}")
        CODE = 2
    sys.exit(CODE)
