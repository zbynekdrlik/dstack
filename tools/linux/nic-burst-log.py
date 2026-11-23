#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Logs a NIC's receive rate per millisecond and its rx_missed steps (dstack #408).

A USB NIC that drops packets at a low average rate (stream-snv's RTL8156B
lost Dante packets at ~0.6 Gb/s) loses them in bursts too short for a
per-second counter. This samples `rx_bytes`, `rx_packets` and `tx_bytes`
from sysfs every millisecond and `rx_missed` from `ethtool -S` in its own
thread (every second by default). It writes, the moment it sees an rx_missed
step, the busiest milliseconds of the read interval before it; a heartbeat
line every 60 s; and at the end the busiest RX and RX+TX milliseconds of the
whole run.

Memory is bounded however long it runs (22.11.2026: the first version kept
every sample, grew to 12 GB in 14 h, was OOM-killed and stalled obs ~28 s).
It keeps only a rolling window of the last per-ms rates (the read interval,
at most 60 s) and two fixed-size top-K lists. Every line is flushed when
written, so a kill loses nothing already reported.

It only reads counters: no capture, no socket, no change to the NIC. It
busy-waits on its CPU's time, so on a box where the application owns cores
it must run on the others, at the lowest priority, with a memory cap. On an
OBS box (OBS owns CPUs 2-15; CPUs 0-1 are the rig's background cores):

    sudo systemd-run --unit=dstack-burst --collect \\
      -p MemoryMax=200M -p MemorySwapMax=0 -p CPUAffinity=0,1 -p Nice=19 \\
      -p CPUSchedulingPolicy=idle \\
      -p StandardOutput=file:/var/tmp/dstack-408/burst.txt \\
      -p StandardError=file:/var/tmp/dstack-408/burst.err \\
      python3 -I /var/tmp/dstack-408/nic-burst-log.py rig0 50400 1

At SCHED_IDLE a busy CPU 0-1 can cost the logger samples, never obs a frame;
the cap ends the logger, never the box. Elsewhere, for a short run:

    taskset -c 18 python3 -I nic-burst-log.py rig0 600 1 > burst.txt

Arguments: the interface, the run length in seconds (default 600), and how
often rx_missed is read, in seconds (default 1; 0 = only at the start and
the end, the control run). On r8152 `ethtool -S` is NOT a counter read: it
is a USB control transfer to the chip (`PLA_TALLYCNT` under the control
mutex, camera-box's read of r8152.c), so a fast cadence could cause the
loss it measures. sysfs `rx_bytes`/`rx_packets` are software counters.
"""

import collections
import heapq
import queue
import subprocess
import sys
import threading
import time

MISSED_EVERY_S = 1.0  # default: how often ethtool is read (a USB control transfer on r8152)
TOP_K = 10  # the busiest milliseconds kept for the end summary
STEP_TOP = 12  # the busiest milliseconds shown for one step
HEARTBEAT_S = 60.0
WINDOW_MAX_S = 60.0  # the longest interval a step report looks back over


def window_len(every_s):
    """Per-ms rates kept: the rx_missed read interval (1 s at least, 60 s at
    most) plus a margin for a late read."""
    return int(min(max(every_s, 1.0), WINDOW_MAX_S) * 1000) + 100


class BurstLog:
    """The sampling core: fed one counter sample at a time, it holds only a
    rolling window of per-ms rates and two top-K heaps, and writes each
    step's report, the heartbeat and the summary to `out` as they happen.

    `stamp(t)` turns a sample time into the text printed for it."""

    def __init__(self, out, stamp, every_s, first_missed, top_k=TOP_K, heartbeat_s=HEARTBEAT_S):
        self.out = out
        self.stamp = stamp
        self.every_s = every_s
        self.first_missed = first_missed
        self.missed = first_missed
        self.top_k = top_k
        self.heartbeat_s = heartbeat_s
        self.window = collections.deque(maxlen=window_len(every_s))
        self.top_rx = []  # min-heap of (RX Mb/s, seq, rate)
        self.top_both = []  # min-heap of (RX+TX Mb/s, seq, rate)
        self.samples = 0
        self.steps = 0
        self.prev = None
        self.next_beat = None

    def write(self, lines):
        self.out.write("".join(line + "\n" for line in lines))
        self.out.flush()

    def _keep(self, heap, key, rate):
        entry = (key, self.samples, rate)
        if len(heap) < self.top_k:
            heapq.heappush(heap, entry)
        elif key > heap[0][0]:
            heapq.heapreplace(heap, entry)

    def add(self, t, rx_bytes, rx_packets, tx_bytes):
        """One sample of the counters at time `t` (seconds)."""
        if self.next_beat is None:
            self.next_beat = t + self.heartbeat_s
        prev = self.prev
        self.prev = (t, rx_bytes, rx_packets, tx_bytes)
        self.samples += 1
        if prev is not None:
            t0, b0, p0, x0 = prev
            dt = t - t0
            if dt > 0:
                rate = (t, (rx_bytes - b0) * 8 / dt / 1e6, rx_packets - p0, (tx_bytes - x0) * 8 / dt / 1e6)
                self.window.append(rate)
                self._keep(self.top_rx, rate[1], rate)
                self._keep(self.top_both, rate[1] + rate[3], rate)
        if t >= self.next_beat:
            self.next_beat += self.heartbeat_s
            peak = max((e[0] for e in self.top_rx), default=0.0)
            self.write([
                f"# heartbeat {self.stamp(t)}: {self.samples} samples, "
                f"max RX {peak:.0f} Mb/s, rx_missed {self.missed}"
            ])

    def step(self, t, before, after):
        """rx_missed read `after` at `t`, `before` at the read before: write
        the busiest milliseconds of the interval before the read, now."""
        self.steps += 1
        self.missed = after
        span = min(self.every_s, WINDOW_MAX_S) if self.every_s > 0 else WINDOW_MAX_S
        interval = [r for r in self.window if t - span <= r[0] <= t]
        top = sorted(sorted(interval, key=lambda r: r[1], reverse=True)[:STEP_TOP])
        peak = max((r[1] for r in interval), default=0.0)
        lines = [
            f"# step {before} -> {after} (+{after - before}) read at {self.stamp(t)}: "
            f"peak {peak:.0f} Mb/s in the {span:g} s before the read; its busiest ms:"
        ]
        for r in top:
            lines.append(f"  {self.stamp(r[0])} RX {r[1]:8.0f} Mb/s {r[2]:5d} pkts, TX {r[3]:8.0f} Mb/s")
        self.write(lines)

    @staticmethod
    def _ranked(heap):
        return [e[2] for e in sorted(heap, key=lambda e: (-e[0], e[1]))]

    def busiest_rx(self):
        return self._ranked(self.top_rx)

    def busiest_both(self):
        return self._ranked(self.top_both)

    def finish(self, run_s, last_missed):
        """The end summary: the run's counts and its top-K milliseconds."""
        lines = [
            f"# end: {self.samples} samples over {run_s:.0f} s; rx_missed {self.first_missed} -> "
            f"{last_missed} (read every {self.every_s:g} s, 0 = start and end only), "
            f"{self.steps} steps seen"
        ]
        lines.append("# busiest RX milliseconds (local time, RX Mb/s, RX packets, TX Mb/s):")
        for t, mbps, pkts, tx in self.busiest_rx():
            lines.append(f"{self.stamp(t)} {mbps:8.0f} {pkts:5d} {tx:8.0f}")
        lines.append("# busiest RX+TX milliseconds (the USB bus carries both):")
        for t, mbps, pkts, tx in self.busiest_both():
            lines.append(f"{self.stamp(t)} {mbps:8.0f} {pkts:5d} {tx:8.0f}")
        self.write(lines)


def sample_loop(log, read_counters, clock, run_s, steps):
    """Samples `read_counters()` once a millisecond of `clock()` for `run_s`
    seconds, feeding `log`; after each sample, hands `log` every step the
    rx_missed reader put on `steps` (an exception put there is raised).
    Returns the number of samples."""
    n = 0
    start = clock()
    end = start + run_s
    now = start
    while now < end:
        if now >= start + n / 1000.0:
            rx_bytes, rx_packets, tx_bytes = read_counters()
            log.add(now, rx_bytes, rx_packets, tx_bytes)
            n += 1
            while not steps.empty():
                item = steps.get_nowait()
                if isinstance(item, BaseException):
                    raise item
                log.step(*item)
        now = clock()
    return n


def read_int(path):
    with open(path) as f:
        return int(f.read())


def missed_now(iface):
    out = subprocess.run(
        ["ethtool", "-S", iface], capture_output=True, text=True, check=True
    ).stdout
    for line in out.splitlines():
        name, _, value = line.strip().partition(":")
        if name == "rx_missed":
            return int(value)
    raise SystemExit(f"{iface}: no rx_missed in ethtool -S")


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    iface = sys.argv[1]
    run_s = float(sys.argv[2]) if len(sys.argv) > 2 else 600.0
    every_s = float(sys.argv[3]) if len(sys.argv) > 3 else MISSED_EVERY_S
    base = f"/sys/class/net/{iface}/statistics/"
    bytes_path, packets_path = base + "rx_bytes", base + "rx_packets"
    tx_path = base + "tx_bytes"

    def read_counters():
        return read_int(bytes_path), read_int(packets_path), read_int(tx_path)

    first = missed_now(iface)
    start = time.perf_counter()
    wall0 = time.time()

    def stamp(t):
        wall = wall0 + t - start
        return time.strftime("%H:%M:%S", time.localtime(wall)) + f".{int((wall % 1) * 1000):03d}"

    log = BurstLog(sys.stdout, stamp, every_s, first)
    log.write([
        f"# {iface}: start {stamp(start)}, {run_s:.0f} s at 1 ms; rx_missed {first}, "
        f"read every {every_s:g} s (0 = start and end only)"
    ])

    steps = queue.SimpleQueue()  # (perf time, missed before, missed after) or an exception
    stop = threading.Event()

    def watch_missed():
        last = first
        try:
            while not stop.wait(every_s):
                now = missed_now(iface)
                if now != last:
                    steps.put((time.perf_counter(), last, now))
                    last = now
        except BaseException as e:  # handed to the sampling loop, which raises it
            steps.put(e)

    watcher = threading.Thread(target=watch_missed, daemon=True)
    if every_s > 0:
        watcher.start()
    try:
        sample_loop(log, read_counters, time.perf_counter, run_s, steps)
    finally:
        stop.set()
    if every_s > 0:
        watcher.join()
    while not steps.empty():
        item = steps.get_nowait()
        if isinstance(item, BaseException):
            raise item
        log.step(*item)
    log.finish(run_s, missed_now(iface))


if __name__ == "__main__":
    main()
