# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests of the weekly consumer version sync's reading (dstack #399)."""
import os
import sys
import unittest

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


if __name__ == "__main__":
    unittest.main()
