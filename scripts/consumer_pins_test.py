# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests of the weekly consumer version sync's reading (dstack #399)."""
import json
import os
import re
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import consumer_pins as cp  # noqa: E402

LOCK = """
[[package]]
name = "dlink"
version = "0.6.118"
source = "git+https://github.com/zbynekdrlik/dstack?tag=v0.6.118#a6fe1509becfa3d1dc17b1b0c4a64711b6b9bde6"

[[package]]
name = "dstack"
version = "0.6.118"
source = "git+https://github.com/zbynekdrlik/dstack?tag=v0.6.118#a6fe1509becfa3d1dc17b1b0c4a64711b6b9bde6"

[[package]]
name = "intercom-vban"
source = "git+https://github.com/zbynekdrlik/intercom?tag=v1.9.23#0000000000000000000000000000000000000000"
"""


class PinOf(unittest.TestCase):
    def test_a_lock_pins_its_dstack_tag(self):
        self.assertEqual(cp.pin_of(LOCK), (0, 6, 118))

    def test_two_tags_read_as_the_older(self):
        lock = LOCK + 'source = "git+https://github.com/zbynekdrlik/dstack?tag=v0.6.120#abc"\n'
        self.assertEqual(cp.pin_of(lock), (0, 6, 118))

    def test_another_repos_tag_is_no_dstack_pin(self):
        lock = 'source = "git+https://github.com/zbynekdrlik/intercom?tag=v1.9.23#abc"\n'
        self.assertIsNone(cp.pin_of(lock))

    def test_a_dstack_git_url_is_read_too(self):
        lock = 'source = "git+https://github.com/zbynekdrlik/dstack.git?tag=v0.6.150#abc"\n'
        self.assertEqual(cp.pin_of(lock), (0, 6, 150))

    def test_a_rev_or_branch_pin_is_untagged(self):
        self.assertTrue(cp.untagged('source = "git+https://github.com/zbynekdrlik/dstack?rev=abc#abc"\n'))
        self.assertTrue(cp.untagged('source = "git+https://github.com/zbynekdrlik/dstack.git?branch=dev#abc"\n'))
        self.assertFalse(cp.untagged(LOCK))

    def test_versions_compare_as_numbers(self):
        lock = (
            'source = "git+https://github.com/zbynekdrlik/dstack?tag=v0.6.99#abc"\n'
            'source = "git+https://github.com/zbynekdrlik/dstack?tag=v0.6.100#def"\n'
        )
        self.assertEqual(cp.pin_of(lock), (0, 6, 99))


class Versions(unittest.TestCase):
    def test_a_tag_parses_with_or_without_its_v(self):
        self.assertEqual(cp.parse_version("v0.6.178"), (0, 6, 178))
        self.assertEqual(cp.parse_version(" 0.6.178\n"), (0, 6, 178))

    def test_anything_else_exits(self):
        with self.assertRaises(SystemExit):
            cp.parse_version("v0.6")

    def test_show(self):
        self.assertEqual(cp.show((0, 6, 7)), "v0.6.7")


def changed_after_150(pin, latest):
    """A fake library history: the code last changed in 0.6.151."""
    return pin < (0, 6, 151)


class Sync(unittest.TestCase):
    LATEST = (0, 6, 178)

    def rows(self):
        locks = {
            "intercom": LOCK,
            "dasio": 'source = "git+https://github.com/zbynekdrlik/dstack?tag=v0.6.178#abc"\n',
            "odd": "no dstack here\n",
            "rev": 'source = "git+https://github.com/zbynekdrlik/dstack?rev=abc#abc"\n',
        }
        names = ("intercom", "dasio", "odd", "rev")
        consumers = [{"name": n, "lock": "Cargo.lock"} for n in names]
        seen = []

        def fetch(repo, path):
            seen.append((repo, path))
            return locks[repo]

        rows = cp.collect(consumers, fetch)
        self.assertEqual(seen, [(n, "Cargo.lock") for n in names])
        return rows

    def test_collect_reads_each_consumers_lock(self):
        self.assertEqual(
            self.rows(),
            [("intercom", (0, 6, 118), False), ("dasio", (0, 6, 178), False),
             ("odd", None, False), ("rev", None, True)],
        )

    def test_behind_and_unread_consumers_lag_a_current_one_not(self):
        self.assertEqual(cp.lagging(self.rows(), self.LATEST, changed_after_150),
                         ["intercom", "odd", "rev"])
        self.assertEqual(cp.lagging([("x", (0, 6, 179), False)], self.LATEST, changed_after_150), [])

    def test_an_older_pin_with_the_same_library_code_is_current(self):
        rows = [("old", (0, 6, 151), False), ("older", (0, 6, 150), False)]
        self.assertEqual(cp.lagging(rows, self.LATEST, changed_after_150), ["older"])
        body = cp.render(rows, self.LATEST, "2026-11-25", changed_after_150)
        self.assertIn("| old | v0.6.151 | current (no library change to v0.6.178) |", body)
        self.assertIn("| older | v0.6.150 | behind |", body)
        self.assertNotIn("- [ ] old on", body)

    def test_the_root_manifest_counts_less_its_version(self):
        a = '[workspace.package]\nversion = "0.6.178"\nedition = "2024"\n'
        self.assertEqual(cp.without_version(a), cp.without_version(a.replace("178", "179")))
        self.assertNotEqual(cp.without_version(a), cp.without_version(a.replace("2024", "2027")))

    def test_an_unknown_pin_tag_reads_as_changed(self):
        self.assertFalse(cp.tag_exists((0, 0, 9999)))
        self.assertTrue(cp.library_changed((0, 0, 9999), (0, 6, 179)))

    def test_the_library_diff_reads_this_repo(self):
        # Two tags of this repo with no library change between them
        # (v0.6.179 changed only a workflow and scripts), and two with one.
        self.assertFalse(cp.library_changed((0, 6, 178), (0, 6, 179)))
        self.assertTrue(cp.library_changed((0, 6, 177), (0, 6, 178)))

    def test_the_body_lists_every_consumer_and_a_line_per_lagging_one(self):
        body = cp.render(self.rows(), self.LATEST, "2026-11-25", changed_after_150)
        self.assertIn("Wednesday read 2026-11-25: the latest dstack release is v0.6.178; "
                      "3 of 4 consumers are behind it", body)
        self.assertIn("| intercom | v0.6.118 | behind |", body)
        self.assertIn("| dasio | v0.6.178 | current |", body)
        self.assertIn("| odd | no dstack tag found | read its lock |", body)
        self.assertIn("| rev | a commit or branch, no tag | move it to a tag |", body)
        self.assertIn("- [ ] intercom on v0.6.178", body)
        self.assertIn("- [ ] odd on v0.6.178", body)
        self.assertNotIn("- [ ] dasio", body)


# Shaped like the sync issue's body on 25.11.2026 (dstack #405; fewer
# consumers, a current one added), before any marker: the generated block,
# then the two lines the dstack session added by hand, verbatim.
OLD_405 = """Wednesday read 2026-11-25: the latest dstack release is v0.6.215; 7 of 9 consumers are behind it (dstack #399).

| Consumer | Pin | |
|---|---|---|
| intercom | v0.6.207 | behind |
| songplayer | v0.6.214 | behind |
| dasio | v0.6.212 | behind |
| dsync | v0.6.215 | current |

Work it: send each consumer behind its upgrade notes from its own pin (the CHANGELOG's API and port lines, checked against its code), then close this issue once every consumer runs the week's release.

- [ ] intercom on v0.6.215
- [ ] songplayer on v0.6.215
- [ ] dasio on v0.6.215


- [ ] After the 25.11 sync: tell dsync which of its boxes run dstack >= v0.6.193 (#440 fixed), box by box, so it can roll v1.35.16 there (dsync#212, parked until then); until then, no dsync service restart on a box with an older dstack device
- [ ] dasio: v0.6.215 on the FOH PC instance-1 for #485 (its late lines carry `pass_max_us` / `pass_max_cpu_us` / `oversleep_max_us`)
"""

HAND = (
    ("- [ ] After the 25.11 sync: tell dsync which of its boxes run dstack >= v0.6.193 (#440 fixed), "
     "box by box, so it can roll v1.35.16 there (dsync#212, parked until then); until then, no dsync "
     "service restart on a box with an older dstack device"),
    ("- [ ] dasio: v0.6.215 on the FOH PC instance-1 for #485 (its late lines carry `pass_max_us` / "
     "`pass_max_cpu_us` / `oversleep_max_us`)"),
)

NAMES = ["intercom", "songplayer", "dasio", "dsync"]


def every_changed(pin, latest):
    return True


class KeepHandLines(unittest.TestCase):
    """dstack #488: a run replaced the whole body and lost the hand lines."""

    def new_body(self, latest=(0, 6, 216), date="2026-12-02"):
        rows = [("intercom", (0, 6, 207), False), ("songplayer", (0, 6, 214), False),
                ("dasio", (0, 6, 212), False), ("dsync", (0, 6, 216), False)]
        return cp.render(rows, latest, date, every_changed)

    def test_the_generated_block_ends_with_the_marker(self):
        self.assertTrue(self.new_body().rstrip("\n").endswith(cp.MARKER))

    def test_an_old_body_without_the_marker_keeps_its_hand_lines(self):
        body = cp.merge(OLD_405, self.new_body(), NAMES)
        self.assertTrue(body.startswith("Wednesday read 2026-12-02:"))
        self.assertIn("- [ ] songplayer on v0.6.216", body)
        self.assertNotIn("v0.6.215; 7 of 9", body)
        self.assertNotIn("- [ ] intercom on v0.6.215", body)
        tail = body.split(cp.MARKER, 1)[1]
        self.assertEqual(tail, "\n\n" + "\n".join(HAND) + "\n")

    def test_the_lines_below_the_marker_are_carried_into_every_next_body(self):
        first = cp.merge(OLD_405, self.new_body(), NAMES)
        later = first + "- [ ] a third hand line\n\n  with a blank line before its second part\n"
        second = cp.merge(later, self.new_body(date="2026-12-09"), NAMES)
        self.assertTrue(second.startswith("Wednesday read 2026-12-09:"))
        self.assertEqual(second.count(cp.MARKER), 1)
        self.assertTrue(second.endswith(
            "\n".join(HAND) + "\n- [ ] a third hand line\n\n  with a blank line before its second part\n"))
        self.assertEqual(cp.merge(second, self.new_body(date="2026-12-09"), NAMES), second)

    def test_a_generated_line_ticked_by_hand_stays_ticked_for_the_same_release(self):
        first = cp.merge(OLD_405, self.new_body(), NAMES)
        ticked = first.replace("- [ ] dasio on v0.6.216", "- [x] dasio on v0.6.216")
        again = cp.merge(ticked, self.new_body(), NAMES)
        self.assertIn("- [x] dasio on v0.6.216", again)
        self.assertIn("- [ ] intercom on v0.6.216", again)
        newer = cp.merge(ticked, self.new_body(latest=(0, 6, 217)), NAMES)
        self.assertIn("- [ ] dasio on v0.6.217", newer)

    def test_no_old_body_gives_the_generated_block(self):
        self.assertEqual(cp.merge(None, self.new_body(), NAMES), self.new_body())
        self.assertEqual(cp.merge("", self.new_body(), NAMES), self.new_body())


def issue(number, state, body, comments=(), closed_at=None, title=cp.TITLE):
    return {"number": number, "title": title, "state": state, "body": body,
            "closedAt": closed_at, "comments": [{"body": c} for c in comments]}


class OncePerWednesday(unittest.TestCase):
    """dstack #488: the 06:00 schedule did not run on 25.11; the later crons
    read only when that day's read is not on the sync issue yet."""

    DATE = "2026-12-02"

    def test_today_s_read_comment_on_the_open_issue_skips(self):
        issues = [issue(405, "OPEN", "Wednesday read 2026-11-25: ...",
                        ["Wednesday read 2026-12-02: 3 consumer(s) behind v0.6.216."])]
        self.assertTrue(cp.already_read(issues, self.DATE))

    def test_an_issue_closed_today_by_the_read_skips(self):
        issues = [issue(405, "CLOSED", "Wednesday read 2026-12-02: ...",
                        ["Wednesday read 2026-12-02: every consumer runs v0.6.216."],
                        closed_at="2026-12-02T06:01:10Z")]
        self.assertTrue(cp.already_read(issues, self.DATE))

    def test_an_issue_created_today_by_the_read_skips(self):
        self.assertTrue(cp.already_read([issue(500, "OPEN", "Wednesday read 2026-12-02: the latest")],
                                        self.DATE))

    def test_last_week_s_read_does_not_skip(self):
        issues = [issue(405, "OPEN", "Wednesday read 2026-11-25: ...",
                        ["Wednesday read 2026-11-25: 7 consumer(s) behind v0.6.215.",
                         "a hand comment naming Wednesday read 2026-12-02: later"]),
                  issue(399, "CLOSED", "Wednesday read 2026-12-02: ...", closed_at="2026-11-25T13:00:00Z"),
                  issue(401, "OPEN", "Wednesday read 2026-12-02: ...", title="Not the sync issue")]
        self.assertFalse(cp.already_read(issues, self.DATE))
        self.assertFalse(cp.already_read([], self.DATE))

    def test_the_open_sync_issue_is_found_by_its_exact_title(self):
        issues = [issue(401, "OPEN", "x", title="Weekly consumer version sync (old)"),
                  issue(405, "OPEN", "the body"), issue(399, "CLOSED", "old")]
        self.assertEqual(cp.sync_issue(issues), (405, "the body"))
        self.assertIsNone(cp.sync_issue(issues[2:]))

    def test_the_read_s_comment_is_what_the_check_looks_for(self):
        line = cp.read_comment(self.DATE, 3, (0, 6, 216))
        self.assertEqual(line, "Wednesday read 2026-12-02: 3 consumer(s) behind v0.6.216.")
        self.assertTrue(cp.already_read([issue(405, "OPEN", "old", [line])], self.DATE))
        closing = cp.read_comment(self.DATE, 0, (0, 6, 216))
        self.assertEqual(closing, "Wednesday read 2026-12-02: every consumer runs v0.6.216.")

    def run_main(self, issues, *flags):
        d = tempfile.mkdtemp()
        with open(os.path.join(d, "issues.json"), "w", encoding="utf-8") as f:
            json.dump(issues, f)
        with open(os.path.join(d, "consumers.toml"), "w", encoding="utf-8") as f:
            f.write('[[consumer]]\nname = "intercom"\nlock = "Cargo.lock"\n')
        out, gh_out = os.path.join(d, "body.md"), os.path.join(d, "out")
        argv = ["consumer_pins.py", "--latest", "v0.6.179", "--date", self.DATE,
                "--consumers", os.path.join(d, "consumers.toml"), "--issues",
                os.path.join(d, "issues.json"), "--out", out, "--github-output", gh_out, *flags]
        # Only the network read is replaced: the lock pins v0.6.178, and the
        # library diff to v0.6.179 is read from this repo's tags (none).
        lock = 'source = "git+https://github.com/zbynekdrlik/dstack?tag=v0.6.178#abc"\n'
        with mock.patch.object(sys, "argv", argv), \
                mock.patch.object(cp, "fetch_raw", lambda owner: lambda repo, path: lock):
            cp.main()
        with open(gh_out, encoding="utf-8") as f:
            outputs = dict(l.split("=", 1) for l in f.read().splitlines())
        body = None
        if os.path.exists(out):
            with open(out, encoding="utf-8") as f:
                body = f.read()
        return outputs, body

    def test_main_skips_a_day_already_read_only_when_asked(self):
        issues = [issue(405, "OPEN", "Wednesday read 2026-12-02: x\n" + cp.MARKER + "\n\nhand\n")]
        outputs, body = self.run_main(issues, "--once-a-day")
        self.assertEqual(outputs, {"date": self.DATE, "title": cp.TITLE, "skip": "1"})
        self.assertIsNone(body)
        outputs, body = self.run_main(issues)
        self.assertEqual(outputs, {"date": self.DATE, "title": cp.TITLE, "skip": "0", "lagging": "0",
                                   "latest": "v0.6.179", "issue": "405",
                                   "comment": "Wednesday read 2026-12-02: every consumer runs v0.6.179."})
        self.assertIn("| intercom | v0.6.178 | current (no library change to v0.6.179) |", body)
        self.assertTrue(body.endswith(cp.MARKER + "\n\nhand\n"))

    def test_the_workflow_runs_three_times_on_wednesday(self):
        wf = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".github", "workflows",
                          "consumer-pins.yml")
        with open(wf, encoding="utf-8") as f:
            text = f.read()
        self.assertEqual(re.findall(r'cron: "([^"]+)"', text), ["0 6 * * 3", "0 9 * * 3", "0 12 * * 3"])
        self.assertIn("--once-a-day", text)
        self.assertIn("--issues", text)


if __name__ == "__main__":
    unittest.main()
