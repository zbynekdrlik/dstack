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


def peer(rise, late, st, watch=()):
    """A peer's `void` entry: its rises, its status, its watch lines."""
    return (rise, late, st, list(watch))


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
            "a": peer(0, 0, status((B_STALL, 5800, False))),
            "b": peer(1, 1, status((B_STALL, 6250, True))),
        }
        self.assertEqual(gate.void(seen, FROM, TO, peers)[0], True)

    def test_a_rebootstrap_the_witness_did_not_see_stands(self):
        far = [(B_STALL - 500, B_STALL - 400)]
        peers = {"a": peer(0, 0, status()), "b": peer(1, 1, status((B_STALL, 6250, True)))}
        is_void, reason = gate.void(far, FROM, TO, peers)
        self.assertFalse(is_void)
        self.assertIn("not seen by the witness", reason)

    def test_the_slack_is_two_ms_each_side(self):
        peers = {"b": peer(1, 1, status((B_STALL, 1000, True)))}
        # The pump stalled [B_STALL - 1, B_STALL].
        self.assertTrue(gate.void([(B_STALL + 2, B_STALL + 5)], FROM, TO, peers)[0])
        self.assertFalse(gate.void([(B_STALL + 2.5, B_STALL + 5)], FROM, TO, peers)[0])
        self.assertTrue(gate.void([(B_STALL - 9, B_STALL - 3)], FROM, TO, peers)[0])
        self.assertFalse(gate.void([(B_STALL - 9, B_STALL - 3.5)], FROM, TO, peers)[0])

    def test_a_rise_without_a_rebootstrap_stands(self):
        seen = [(B_STALL - 7, B_STALL + 1)]
        peers = {"a": peer(0, 0, status()), "b": peer(0, 0, status((B_STALL, 6250, True)))}
        is_void, reason = gate.void(seen, FROM, TO, peers)
        self.assertFalse(is_void)
        self.assertIn("no peer re-bootstrapped", reason)

    def test_a_rebootstrap_that_was_not_a_late_wake_stands(self):
        # A clock-back (or re-anchor, re-time) restart is never the runner's,
        # even beside a witnessed late one from before the window.
        seen = [(FROM - 1007, FROM - 999)]
        peers = {"b": peer(1, 0, status((FROM - 1000, 6250, True)))}
        is_void, reason = gate.void(seen, FROM, TO, peers)
        self.assertFalse(is_void)
        self.assertIn("1 of 1 re-bootstraps were not late wakes", reason)

    def test_more_rebootstraps_than_the_recent_wakes_hold_stand(self):
        seen = [(B_STALL - 7, B_STALL + 1)]
        peers = {"b": peer(2, 2, status((B_STALL, 6250, True)))}
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
            peers = {"b": peer(1, 1, status((B_STALL, 6250, True), (t, 6500, True)))}
            self.assertEqual(gate.void(seen, FROM, TO, peers)[0], not inside, t)

    def test_a_late_wake_without_a_rebootstrap_is_not_checked(self):
        seen = [(B_STALL - 7, B_STALL + 1)]
        peers = {"b": peer(1, 1, status((B_STALL, 6250, True), (B_STALL + 3000, 3000, False)))}
        self.assertTrue(gate.void(seen, FROM, TO, peers)[0])


# Main CI run 43961883405 (v0.6.157, attempt 1, #339): b re-bootstrapped
# alone, 6125 us late; the witness's nearest stall was 5609 us at -3.4 s.
# Its pump watch line (v0.6.157: no oversleep field), verbatim.
C_WAKE = 1794865075821
C_FROM, C_TO = 1794865070000, 1794865094500
C_LINE = (
    "2026-11-16T21:37:56.722460Z  WARN dstack::stream::watch: engine pump lost or overran "
    'blocks this second device="ci-peer-b" rebootstraps=1 missed=3 overruns=0 '
    "writes_after_send=0 max_process_us=14 media=true block=128 margin=288 playout=480 "
    "offset=896 cause=late behind_max_us=6125"
)
C_SEEN = gate.stalls(
    [
        "1794865069766 2834",
        "1794865072415 5609",
        "1794865078223 2917",
        "1794865079736 3292",
        "1794865080796 3023",
    ]
)


def split(line, oversleep_us):
    """The line as v0.6.158 logs it, with the sleep's oversleep (#354)."""
    return (
        f"{line} oversleep_max_us={oversleep_us} drain_max_us=40 drain_max_datagrams=12 "
        "max_process_cpu_us=14"
    )


def case(*lines, behind_us=6125):
    """The main-run case's window, b's wake `behind_us` late, its log."""
    peers = {
        "a": peer(0, 0, status((1794865072415, 3041, False))),
        "b": peer(1, 1, status((C_WAKE, behind_us, True)), gate.watch_lines(lines)),
    }
    return gate.void(C_SEEN, C_FROM, C_TO, peers)


class WatchLines(unittest.TestCase):
    def test_the_case_s_line_parses_without_an_oversleep(self):
        (w,) = gate.watch_lines([C_LINE + "\n", "2026-11-16T21:37:57.0Z  INFO other\n"])
        self.assertEqual(
            w,
            {
                "unix_ms": C_WAKE + 901,
                "rebootstraps": 1,
                "cause": "late",
                "behind_max_us": 6125,
                "oversleep_max_us": None,
                "margin": 288,
            },
        )

    def test_a_split_line_and_a_coloured_one_carry_the_oversleep(self):
        (w,) = gate.watch_lines([split(C_LINE, 6090)])
        self.assertEqual(w["oversleep_max_us"], 6090)
        coloured = split(C_LINE, 6090).replace(
            "oversleep_max_us=6090", "\x1b[3moversleep_max_us\x1b[0m\x1b[2m=\x1b[0m6090"
        )
        self.assertEqual(gate.watch_lines([coloured]), [w])
        # Any count of fraction digits is whole ms.
        (w,) = gate.watch_lines([C_LINE.replace("56.722460Z", "56.7Z")])
        self.assertEqual(w["unix_ms"], C_WAKE + 879)
        (w,) = gate.watch_lines([C_LINE.replace("56.722460Z", "56Z")])
        self.assertEqual(w["unix_ms"], C_WAKE + 179)


class Oversleep(unittest.TestCase):
    def test_the_case_stands_its_log_had_no_split(self):
        # v0.6.157 logged no oversleep: the witness alone decides, as then.
        is_void, reason = case(C_LINE)
        self.assertFalse(is_void)
        self.assertIn("b: re-bootstrap at 1794865075821 ms (6125 us behind) not seen", reason)
        self.assertIn("no pump watch line shows its sleep returned that late", reason)

    def test_an_oversleep_about_the_whole_lateness_past_w_is_the_runner(self):
        is_void, reason = case(split(C_LINE, 6090))
        self.assertTrue(is_void)
        self.assertIn("6125 us behind, its sleep returned 6090 us late", reason)
        # The slack: 500 us short of the lateness.
        self.assertFalse(case(split(C_LINE, 5625))[0])  # under W (6000 us)
        line = split(C_LINE, 7500).replace("behind_max_us=6125", "behind_max_us=8000")
        self.assertTrue(case(line, behind_us=8000)[0])
        line = split(C_LINE, 7499).replace("behind_max_us=6125", "behind_max_us=8000")
        self.assertFalse(case(line, behind_us=8000)[0])

    def test_an_oversleep_within_w_never_explains_a_rebootstrap(self):
        # W = margin 288 samples = 6000 us: a sleep back within W did not
        # re-bootstrap; the rest of the lateness was the pump's own.
        self.assertFalse(case(split(C_LINE, 6000))[0])
        self.assertTrue(case(split(C_LINE, 6001))[0])
        self.assertFalse(case(split(C_LINE, 2000))[0])

    def test_the_line_must_be_this_rebootstrap_s_alone(self):
        good = split(C_LINE, 6090)
        for bad in [
            good.replace("rebootstraps=1", "rebootstraps=2"),
            good.replace("cause=late", "cause=late+retime"),
            good.replace("behind_max_us=6125", "behind_max_us=6200"),
            # Logged before the wake, or past WATCH_AFTER_MS after it.
            good.replace("21:37:56.722460Z", "21:37:55.821000Z"),
            good.replace("21:37:56.722460Z", "21:37:57.822000Z"),
        ]:
            self.assertFalse(case(bad)[0], bad)
        self.assertTrue(case(good.replace("21:37:56.722460Z", "21:37:57.821000Z"))[0])

    def test_a_witnessed_and_an_overslept_rebootstrap_void_together(self):
        peers = {
            "a": peer(1, 1, status((B_STALL, 6250, True))),
            "b": peer(1, 1, status((C_WAKE, 6125, True)), gate.watch_lines([split(C_LINE, 6090)])),
        }
        seen = [(B_STALL - 7, B_STALL + 1)]
        is_void, reason = gate.void(seen, FROM, C_TO, peers)
        self.assertTrue(is_void, reason)
        self.assertIn(f"a: re-bootstrap at {B_STALL} ms in a witnessed stall", reason)
        self.assertIn("b: re-bootstrap at 1794865075821 ms, 6125 us behind", reason)


class Main(unittest.TestCase):
    def test_a_peer_s_log_is_read_for_its_watch_lines(self):
        with tempfile.TemporaryDirectory() as d:
            w = os.path.join(d, "w.txt")
            with open(w, "w", encoding="ascii") as f:
                f.write("1794865072415 5609\n")
            b = os.path.join(d, "b.json")
            with open(b, "w", encoding="utf-8") as f:
                json.dump(status((C_WAKE, 6125, True)), f)
            log = os.path.join(d, "b.log")
            args = ["gate", "void", w, str(C_FROM), str(C_TO)]
            with open(log, "w", encoding="utf-8") as f:
                f.write(C_LINE + "\n")
            self.assertEqual(gate.main(args + [f"b=1,1:{b}:{log}"]), 1)
            with open(log, "w", encoding="utf-8") as f:
                f.write(split(C_LINE, 6090) + "\n")
            self.assertEqual(gate.main(args + [f"b=1,1:{b}:{log}"]), 0)
            # Without its log a peer has only the witness.
            self.assertEqual(gate.main(args + [f"b=1,1:{b}"]), 1)
            self.assertEqual(gate.main(args + [f"b=1,1:{b}:{log}:x"]), 2)
            self.assertEqual(gate.main(args + [f"b=1,1:{b}:{d}/none.log"]), 2)


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
