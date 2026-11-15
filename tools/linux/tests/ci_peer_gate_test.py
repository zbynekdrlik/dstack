#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests of the CI peer gate's stall witness (dstack #305):
`python3 -m unittest tools/linux/tests/ci_peer_gate_test.py`."""

import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ci_peer_gate as gate  # noqa: E402


def status(*wakes):
    """A peer status holding these (unix_ms, behind_us, rebootstrap) wakes."""
    return {
        "device": {
            "engine": {
                "late_wakes_recent": [
                    {"unix_ms": t, "behind_us": b, "rebootstrap": r} for t, b, r in wakes
                ]
            }
        }
    }


# Main CI run 43716942516 (v0.6.138): b re-bootstrapped at 1794747844589,
# 6250 us late; a did not.
B_STALL = 1794747844589
FROM, TO = 1794747840000, 1794747860000


class Step(unittest.TestCase):
    def test_a_wake_two_ms_late_or_more_is_logged(self):
        def run(late_ns):
            times = iter([0, gate.WITNESS_SLEEP_US * 1000 + late_ns])
            slept = []
            out = gate.step(slept.append, lambda: next(times))
            self.assertEqual(slept, [0.001])
            return out

        self.assertEqual(run(2_000_000), 2000)
        self.assertEqual(run(3_500_999), 3500)
        self.assertIsNone(run(1_999_999))
        self.assertIsNone(run(0))


class Stalls(unittest.TestCase):
    def test_a_line_is_a_wake_and_its_lateness(self):
        self.assertEqual(
            gate.stalls(["1000 2500\n", "\n", "2000 4000\n"]),
            [(997.5, 1000), (1996.0, 2000)],
        )


class Void(unittest.TestCase):
    def test_a_rebootstrap_inside_a_witnessed_stall_is_void(self):
        seen = [(B_STALL - 6.0, B_STALL + 1)]
        peers = {
            "a": (0, 0, status((B_STALL, 5800, False))),
            "b": (1, 1, status((B_STALL, 6250, True))),
        }
        self.assertEqual(gate.void(seen, FROM, TO, peers)[0], True)

    def test_a_rebootstrap_the_witness_did_not_see_stands(self):
        far = [(B_STALL - 500, B_STALL - 400)]
        peers = {"a": (0, 0, status()), "b": (1, 1, status((B_STALL, 6250, True)))}
        is_void, reason = gate.void(far, FROM, TO, peers)
        self.assertFalse(is_void)
        self.assertIn("not seen by the witness", reason)

    def test_the_slack_is_two_ms_each_side(self):
        peers = {"b": (1, 1, status((B_STALL, 1000, True)))}
        # The pump stalled [B_STALL - 1, B_STALL].
        self.assertTrue(gate.void([(B_STALL + 2, B_STALL + 5)], FROM, TO, peers)[0])
        self.assertFalse(gate.void([(B_STALL + 2.5, B_STALL + 5)], FROM, TO, peers)[0])
        self.assertTrue(gate.void([(B_STALL - 9, B_STALL - 3)], FROM, TO, peers)[0])
        self.assertFalse(gate.void([(B_STALL - 9, B_STALL - 3.5)], FROM, TO, peers)[0])

    def test_a_rise_without_a_rebootstrap_stands(self):
        seen = [(B_STALL - 7, B_STALL + 1)]
        peers = {"a": (0, 0, status()), "b": (0, 0, status((B_STALL, 6250, True)))}
        is_void, reason = gate.void(seen, FROM, TO, peers)
        self.assertFalse(is_void)
        self.assertIn("no peer re-bootstrapped", reason)

    def test_a_rebootstrap_that_was_not_a_late_wake_stands(self):
        # A clock-back (or re-anchor, re-time) restart is never the runner's,
        # even beside a witnessed late one from before the window.
        seen = [(FROM - 1007, FROM - 999)]
        peers = {"b": (1, 0, status((FROM - 1000, 6250, True)))}
        is_void, reason = gate.void(seen, FROM, TO, peers)
        self.assertFalse(is_void)
        self.assertIn("1 of 1 re-bootstraps were not late wakes", reason)

    def test_more_rebootstraps_than_the_recent_wakes_hold_stand(self):
        seen = [(B_STALL - 7, B_STALL + 1)]
        peers = {"b": (2, 2, status((B_STALL, 6250, True)))}
        is_void, reason = gate.void(seen, FROM, TO, peers)
        self.assertFalse(is_void)
        self.assertIn("2 re-bootstraps rose, 1", reason)

    def test_the_window_bounds_which_wakes_count(self):
        # An unwitnessed restart before the window (past the status's age)
        # or after it does not count; one inside the status's age does.
        seen = [(B_STALL - 7, B_STALL + 1)]
        for t, inside in [
            (FROM - gate.STATUS_AGE_MS - 1, False),
            (FROM - gate.STATUS_AGE_MS, True),
            (TO, True),
            (TO + 1, False),
        ]:
            peers = {"b": (1, 1, status((B_STALL, 6250, True), (t, 6500, True)))}
            self.assertEqual(gate.void(seen, FROM, TO, peers)[0], not inside, t)

    def test_a_late_wake_without_a_rebootstrap_is_not_checked(self):
        seen = [(B_STALL - 7, B_STALL + 1)]
        peers = {"b": (1, 1, status((B_STALL, 6250, True), (B_STALL + 3000, 3000, False)))}
        self.assertTrue(gate.void(seen, FROM, TO, peers)[0])


class Main(unittest.TestCase):
    def test_void_exits_0_and_a_standing_rise_1(self):
        with tempfile.TemporaryDirectory() as d:
            w = os.path.join(d, "w.txt")
            with open(w, "w", encoding="ascii") as f:
                f.write(f"{B_STALL + 1} 7000\n")
            b = os.path.join(d, "b.json")
            with open(b, "w", encoding="utf-8") as f:
                json.dump(status((B_STALL, 6250, True)), f)
            args = ["gate", "void", w, str(FROM), str(TO)]
            self.assertEqual(gate.main(args + [f"b=1,1:{b}"]), 0)
            self.assertEqual(gate.main(args + [f"b=0,0:{b}"]), 1)
            self.assertEqual(gate.main(args + [f"b=1,0:{b}"]), 1)
            self.assertEqual(gate.main(["gate"]), 2)
            # A status of another shape or a missing file is an error, 2.
            with open(b, "w", encoding="utf-8") as f:
                json.dump({"device": {"engine": {"late_wakes_recent": None}}}, f)
            self.assertEqual(gate.main(args + [f"b=1,1:{b}"]), 2)
            self.assertEqual(gate.main(args + [f"b=1,1:{d}/none.json"]), 2)
            self.assertEqual(gate.main(args + [f"b=x,1:{b}"]), 2)


if __name__ == "__main__":
    unittest.main()
