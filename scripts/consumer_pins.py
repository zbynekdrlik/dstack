#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The weekly consumer version sync (dstack #399, owner 18.11.2026).

Reads each consumer's pinned dstack tag from the Cargo.lock on its default
branch (`scripts/consumers.toml`, every repo public), compares it with the
latest dstack release and writes the sync issue's body. The workflow
`consumer-pins.yml` runs it Wednesday mornings and opens or updates the one
"Weekly consumer version sync" issue when a consumer is behind; the dstack
session then sends each lagging consumer its upgrade notes from its own pin.
Consumers are never asked to move per micro-release.
"""
import argparse
import re
import subprocess
import sys
import tomllib
import urllib.request
from typing import Callable, Optional

TITLE = "Weekly consumer version sync"

# A dstack git source in a Cargo.lock: `git+https://github.com/<owner>/dstack?tag=v0.6.178#<sha>`
# (`dstack.git` too).
SOURCE = re.compile(r'git\+https://github\.com/[^/"]+/dstack(?:\.git)?\?tag=v(\d+)\.(\d+)\.(\d+)#')
# A dstack git source pinned by a commit or a branch instead of a tag.
UNTAGGED = re.compile(r'git\+https://github\.com/[^/"]+/dstack(?:\.git)?\?(?:rev|branch)=')

Version = tuple[int, int, int]
Row = tuple[str, Optional[Version], bool]

# What a consumer compiles from dstack: the library crates and the vendored
# mDNS crate. A release that touches none of them (a tool, a workflow, a
# binary like dtray or dstack-dev) gives a consumer nothing to move for.
LIBRARY = (
    "crates/dstack",
    "crates/dlink",
    "crates/dstack-audio-io",
    "crates/dstack-resample",
    "crates/dstack-endpoint",
    "crates/dstack-asio",
    "vendor/mdns-sd",
)


def parse_version(text: str) -> Version:
    """`v0.6.178` or `0.6.178` as a tuple; anything else exits."""
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", text.strip())
    if not m:
        sys.exit(f"not a version: {text!r}")
    return (int(m[1]), int(m[2]), int(m[3]))


def show(v: Version) -> str:
    return f"v{v[0]}.{v[1]}.{v[2]}"


def pin_of(lock: str) -> Optional[Version]:
    """The oldest dstack tag a Cargo.lock pins (every dstack crate comes
    from one tag; two tags in one lock is a half-done move, judged by the
    older); None when the lock pins no dstack tag."""
    found = {tuple(int(x) for x in m.groups()) for m in SOURCE.finditer(lock)}
    return min(found) if found else None


def untagged(lock: str) -> bool:
    """The lock takes dstack by a commit or a branch, not a tag."""
    return UNTAGGED.search(lock) is not None


def collect(consumers: list[dict], fetch: Callable[[str, str], str]) -> list[tuple[str, Optional[Version], bool]]:
    """Each consumer's name, its pin and whether it takes dstack untagged,
    `fetch(repo, path)` giving the lock's text."""
    rows = []
    for c in consumers:
        lock = fetch(c["name"], c["lock"])
        rows.append((c["name"], pin_of(lock), untagged(lock)))
    return rows


def state(pin: Optional[Version], by_rev: bool, latest: Version,
          changed: Callable[[Version, Version], bool]) -> tuple[bool, str]:
    """Whether a consumer must move, and its row's label. An older pin whose
    library code equals `latest`'s (`changed(pin, latest)` false) is current:
    the owner wants one move a week, never one per release (#399)."""
    if pin is None and by_rev:
        return True, "a commit or branch, no tag | move it to a tag"
    if pin is None:
        return True, "no dstack tag found | read its lock"
    if pin >= latest:
        return False, f"{show(pin)} | current"
    if not changed(pin, latest):
        return False, f"{show(pin)} | current (no library change to {show(latest)})"
    return True, f"{show(pin)} | behind"


def lagging(rows: list[Row], latest: Version, changed: Callable[[Version, Version], bool]) -> list[str]:
    """The consumers that must move to `latest` (no dstack tag found counts:
    an untagged pin or a changed lock needs a look)."""
    return [name for name, pin, by_rev in rows if state(pin, by_rev, latest, changed)[0]]


def render(rows: list[Row], latest: Version, date: str,
           changed: Callable[[Version, Version], bool]) -> str:
    """The sync issue's body."""
    behind = lagging(rows, latest, changed)
    lines = [
        f"Wednesday read {date}: the latest dstack release is {show(latest)}; "
        f"{len(behind)} of {len(rows)} consumers are behind it (dstack #399).",
        "",
        "| Consumer | Pin | |",
        "|---|---|---|",
    ]
    for name, pin, by_rev in rows:
        lines.append(f"| {name} | {state(pin, by_rev, latest, changed)[1]} |")
    lines += [
        "",
        "Work it: send each consumer behind its upgrade notes from its own pin "
        "(the CHANGELOG's API and port lines, checked against its code), then "
        "close this issue once every consumer runs the week's release.",
        "",
    ]
    lines += [f"- [ ] {name} on {show(latest)}" for name in behind]
    return "\n".join(lines) + "\n"


def without_version(manifest: str) -> str:
    """The root Cargo.toml less its `version = "..."` lines: every release
    bumps the workspace version, while the rest (the edition, the profiles
    every crate inherits) is library-relevant."""
    return "\n".join(l for l in manifest.splitlines() if not l.startswith("version = "))


def tag_exists(v: Version) -> bool:
    """Whether this checkout knows the tag (a git failure reads as no)."""
    r = subprocess.run(["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{show(v)}"],
                       capture_output=True, text=True)
    return r.returncode == 0


def library_changed(pin: Version, latest: Version) -> bool:
    """Whether the library code differs between two dstack tags, read from
    this checkout's git history (the workflow fetches every tag): the
    library paths, and the root Cargo.toml less its version. A pin this
    checkout does not know reads as changed (the consumer is asked to move,
    never the whole read failed); any other git failure exits."""
    if not tag_exists(pin):
        return True
    r = subprocess.run(
        ["git", "diff", "--quiet", show(pin), show(latest), "--", *LIBRARY],
        capture_output=True, text=True,
    )
    if r.returncode not in (0, 1):
        sys.exit(f"git diff {show(pin)} {show(latest)} failed: {r.stderr.strip()}")
    if r.returncode == 1:
        return True
    roots = []
    for v in (pin, latest):
        s = subprocess.run(["git", "show", f"{show(v)}:Cargo.toml"], capture_output=True, text=True)
        if s.returncode != 0:
            sys.exit(f"git show {show(v)}:Cargo.toml failed: {s.stderr.strip()}")
        roots.append(without_version(s.stdout))
    return roots[0] != roots[1]


def fetch_raw(owner: str) -> Callable[[str, str], str]:
    """Reads a public repo's file on its default branch; any failure exits."""
    def fetch(repo: str, path: str) -> str:
        url = f"https://raw.githubusercontent.com/{owner}/{repo}/HEAD/{path}"
        with urllib.request.urlopen(url, timeout=30) as r:
            return r.read().decode("utf-8")
    return fetch


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--latest", required=True, help="the latest dstack release tag")
    ap.add_argument("--date", required=True, help="the read's date, YYYY-MM-DD")
    ap.add_argument("--owner", default="zbynekdrlik")
    ap.add_argument("--consumers", default="scripts/consumers.toml")
    ap.add_argument("--out", required=True, help="where the issue body goes")
    ap.add_argument("--github-output", help="GITHUB_OUTPUT file for `lagging`")
    a = ap.parse_args()
    latest = parse_version(a.latest)
    with open(a.consumers, "rb") as f:
        consumers = tomllib.load(f)["consumer"]
    rows = collect(consumers, fetch_raw(a.owner))
    body = render(rows, latest, a.date, library_changed)
    with open(a.out, "w", encoding="utf-8") as f:
        f.write(body)
    behind = lagging(rows, latest, library_changed)
    for name, pin, by_rev in rows:
        print(f"{name}: {show(pin) if pin else ('untagged' if by_rev else 'no dstack tag')}")
    print(f"{len(behind)} behind {show(latest)}")
    if a.github_output:
        with open(a.github_output, "a", encoding="utf-8") as f:
            f.write(f"lagging={len(behind)}\nlatest={show(latest)}\n")


if __name__ == "__main__":
    main()
