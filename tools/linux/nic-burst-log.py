#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Logs a NIC's receive rate per millisecond and its rx_missed steps (dstack #408).

A USB NIC that drops packets at a low average rate (stream-snv's RTL8156B
lost Dante packets at ~0.6 Gb/s) loses them in bursts too short for a
per-second counter. This samples `rx_bytes` and `rx_packets` from sysfs
every millisecond and `rx_missed` from `ethtool -S` in its own thread, and
writes, for every rx_missed step, the receive rate of the milliseconds
around it, plus a summary of the busiest milliseconds of the run.

It only reads counters: no capture, no socket, no change to the NIC. Pin it
to a CPU the application does not use (`taskset -c <cpu>`), since it busy-waits
on that CPU's time.

    taskset -c 18 python3 -I nic-burst-log.py rig0 600 > burst.txt

Arguments: the interface, the run length in seconds (default 600).
"""

import subprocess
import sys
import threading
import time

WINDOW_MS = 50  # milliseconds kept before and after an rx_missed step
MISSED_EVERY_S = 0.02  # how often ethtool is read (it takes a few ms)


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
    base = f"/sys/class/net/{iface}/statistics/"
    bytes_path, packets_path = base + "rx_bytes", base + "rx_packets"

    steps = []  # (perf time, missed before, missed after)
    stop = threading.Event()

    def watch_missed():
        last = missed_now(iface)
        while not stop.is_set():
            time.sleep(MISSED_EVERY_S)
            now = missed_now(iface)
            if now != last:
                steps.append((time.perf_counter(), last, now))
                last = now

    watcher = threading.Thread(target=watch_missed, daemon=True)
    watcher.start()

    samples = []  # (perf time, rx_bytes, rx_packets), one a millisecond
    start = time.perf_counter()
    due = start
    end = start + run_s
    while True:
        now = time.perf_counter()
        if now >= end:
            break
        if now < due:
            continue
        samples.append((now, read_int(bytes_path), read_int(packets_path)))
        due += 0.001
    stop.set()
    watcher.join()

    rates = []  # (time, Mb/s, packets) per sample interval
    for (t0, b0, p0), (t1, b1, p1) in zip(samples, samples[1:]):
        dt = t1 - t0
        rates.append((t1, (b1 - b0) * 8 / dt / 1e6, p1 - p0))

    wall0 = time.time() - (time.perf_counter() - start)

    def stamp(t):
        wall = wall0 + t - start
        return time.strftime("%H:%M:%S", time.localtime(wall)) + f".{int((wall % 1) * 1000):03d}"

    print(f"# {iface}: {len(samples)} samples over {run_s:.0f} s, {len(steps)} rx_missed steps")
    busiest = sorted(rates, key=lambda r: r[1], reverse=True)[:10]
    print("# busiest milliseconds (local time, Mb/s, packets):")
    for t, mbps, pkts in busiest:
        print(f"{stamp(t)} {mbps:8.0f} {pkts:5d}")
    for t, before, after in steps:
        around = [r for r in rates if t - WINDOW_MS / 1000 <= r[0] <= t + WINDOW_MS / 1000]
        peak = max((r[1] for r in around), default=0.0)
        print(
            f"# step {before} -> {after} (+{after - before}) at "
            f"{stamp(t)}: "
            f"peak {peak:.0f} Mb/s in the {2 * WINDOW_MS} ms around it"
        )
        for r in around:
            print(f"  {r[0] - t:+.4f} s {r[1]:8.0f} Mb/s {r[2]:5d} pkts")


if __name__ == "__main__":
    main()
