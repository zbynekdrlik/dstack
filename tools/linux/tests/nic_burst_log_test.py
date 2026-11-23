#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests of the per-ms NIC burst logger (dstack #408):
`python3 -m unittest tools/linux/tests/nic_burst_log_test.py`.

22.11.2026: a 14 h run kept every 1 ms sample in memory, grew to 12 GB, was
OOM-killed and stalled obs ~28 s, and wrote nothing (all output came at the
end). These drive the sampling core and its loop with a fake counter source
and a fake clock: memory stays at its bound, a step's report is written when
the step is seen, and the run's top-K is the brute-force top-K."""

import importlib.util
import io
import os
import queue
import random
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))

_spec = importlib.util.spec_from_file_location(
    "nic_burst_log", os.path.join(HERE, "..", "nic-burst-log.py")
)
nbl = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(nbl)


def stamp(t):
    return f"{t:.3f}"


class Out(io.StringIO):
    """An output stream that counts its flushes."""

    def __init__(self):
        super().__init__()
        self.flushes = 0

    def flush(self):
        self.flushes += 1
        super().flush()


class FakeClock:
    """Advances 1 ms on every read, so each loop pass takes one sample."""

    def __init__(self):
        self.calls = 0

    def __call__(self):
        t = self.calls / 1000.0
        self.calls += 1
        return t


class FakeCounters:
    """rx_bytes / rx_packets / tx_bytes that grow by a per-call amount."""

    def __init__(self, rx_per_ms, tx_per_ms=None, on_call=None):
        self.rx_per_ms = rx_per_ms
        self.tx_per_ms = tx_per_ms or (lambda n: 0)
        self.on_call = on_call
        self.n = 0
        self.rx = 0
        self.pkts = 0
        self.tx = 0

    def __call__(self):
        if self.on_call is not None:
            self.on_call(self.n)
        self.rx += self.rx_per_ms(self.n)
        self.pkts += 1
        self.tx += self.tx_per_ms(self.n)
        self.n += 1
        return self.rx, self.pkts, self.tx


class BoundedMemory(unittest.TestCase):
    def test_a_million_samples_keep_the_window_and_the_top_k_at_their_bound(self):
        out = Out()
        log = nbl.BurstLog(out, stamp, every_s=1.0, first_missed=0)
        rng = random.Random(408)
        counters = FakeCounters(lambda n: rng.randrange(0, 250_000), lambda n: rng.randrange(0, 100_000))
        n = nbl.sample_loop(log, counters, FakeClock(), 1000.0, queue.SimpleQueue())
        self.assertGreaterEqual(n, 999_000)
        self.assertEqual(log.samples, n)
        self.assertEqual(len(log.window), log.window.maxlen)
        self.assertEqual(log.window.maxlen, nbl.window_len(1.0))
        self.assertLessEqual(log.window.maxlen, 1100)
        self.assertEqual(len(log.top_rx), nbl.TOP_K)
        self.assertEqual(len(log.top_both), nbl.TOP_K)

    def test_the_window_is_capped_for_a_slow_cadence(self):
        self.assertLessEqual(nbl.window_len(3600.0), nbl.WINDOW_MAX_S * 1000 + 100)
        self.assertGreaterEqual(nbl.window_len(0.0), 1000)


class StepsStreamed(unittest.TestCase):
    def test_a_step_is_written_and_flushed_when_seen_not_at_the_end(self):
        out = Out()
        log = nbl.BurstLog(out, stamp, every_s=1.0, first_missed=100)
        steps = queue.SimpleQueue()
        burst_at = 1500  # sample index of a 2 Gb/s millisecond

        def on_call(n):
            if n == 1800:
                # The ethtool thread saw rx_missed step: 100 -> 169.
                steps.put((n / 1000.0, 100, 169))

        counters = FakeCounters(lambda n: 250_000 if n == burst_at else 1000, on_call=on_call)
        nbl.sample_loop(log, counters, FakeClock(), 3.0, steps)
        text = out.getvalue()
        # finish() was never called: the report is already out.
        self.assertIn("# step 100 -> 169 (+69)", text)
        self.assertNotIn("# busiest RX milliseconds", text)
        report = text[text.index("# step 100 -> 169"):]
        self.assertIn(f"{burst_at / 1000.0:.3f} RX     2000 Mb/s", report)
        self.assertGreater(out.flushes, 0)
        self.assertEqual(log.missed, 169)

    def test_the_step_report_covers_only_the_read_interval_before_it(self):
        out = Out()
        log = nbl.BurstLog(out, stamp, every_s=1.0, first_missed=0)
        t = 0.0
        rx = 0
        for n in range(3000):
            t = n / 1000.0
            rx += 250_000 if n == 500 else 1000  # the burst 2.5 s before the step
            log.add(t, rx, n, 0)
        log.step(t, 0, 5)
        report = out.getvalue()
        self.assertIn("# step 0 -> 5 (+5)", report)
        self.assertNotIn("0.500 RX", report)
        self.assertIn("peak 8 Mb/s", report)

    def test_a_heartbeat_line_every_minute(self):
        out = Out()
        log = nbl.BurstLog(out, stamp, every_s=1.0, first_missed=7)
        nbl.sample_loop(log, FakeCounters(lambda n: 1000), FakeClock(), 130.0, queue.SimpleQueue())
        beats = [line for line in out.getvalue().splitlines() if line.startswith("# heartbeat")]
        self.assertEqual(len(beats), 2)
        self.assertIn("rx_missed 7", beats[0])
        self.assertIn("max RX 8 Mb/s", beats[0])


class TopK(unittest.TestCase):
    def test_the_top_k_matches_a_brute_force_top_k(self):
        rng = random.Random(1)
        samples = []
        rx = tx = 0
        for n in range(5000):
            rx += rng.randrange(0, 300_000)
            tx += rng.randrange(0, 200_000)
            samples.append((n / 1000.0, rx, n, tx))
        out = Out()
        log = nbl.BurstLog(out, stamp, every_s=1.0, first_missed=0)
        for s in samples:
            log.add(*s)
        rates = []
        for (t0, b0, p0, x0), (t1, b1, p1, x1) in zip(samples, samples[1:]):
            dt = t1 - t0
            rates.append((t1, (b1 - b0) * 8 / dt / 1e6, p1 - p0, (x1 - x0) * 8 / dt / 1e6))
        want_rx = sorted(rates, key=lambda r: r[1], reverse=True)[: nbl.TOP_K]
        want_both = sorted(rates, key=lambda r: r[1] + r[3], reverse=True)[: nbl.TOP_K]
        self.assertEqual(log.busiest_rx(), want_rx)
        self.assertEqual(log.busiest_both(), want_both)
        log.finish(5.0, 3)
        text = out.getvalue()
        self.assertIn("# busiest RX milliseconds", text)
        self.assertIn("# busiest RX+TX milliseconds", text)
        self.assertIn("rx_missed 0 -> 3", text)


if __name__ == "__main__":
    unittest.main()
