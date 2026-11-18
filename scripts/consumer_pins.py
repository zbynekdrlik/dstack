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


def lagging(rows: list[tuple[str, Optional[Version], bool]], latest: Version) -> list[str]:
    """The consumers behind `latest` (no dstack tag found counts: an untagged
    pin or a changed lock needs a look)."""
    return [name for name, pin, _ in rows if pin is None or pin < latest]


def render(rows: list[tuple[str, Optional[Version], bool]], latest: Version, date: str) -> str:
    """The sync issue's body."""
    behind = lagging(rows, latest)
    lines = [
        f"Wednesday read {date}: the latest dstack release is {show(latest)}; "
        f"{len(behind)} of {len(rows)} consumers are behind it (dstack #399).",
        "",
        "| Consumer | Pin | |",
        "|---|---|---|",
    ]
    for name, pin, by_rev in rows:
        if pin is None and by_rev:
            lines.append(f"| {name} | a commit or branch, no tag | move it to a tag |")
        elif pin is None:
            lines.append(f"| {name} | no dstack tag found | read its lock |")
        elif pin < latest:
            lines.append(f"| {name} | {show(pin)} | behind |")
        else:
            lines.append(f"| {name} | {show(pin)} | current |")
    lines += [
        "",
        "Work it: send each consumer behind its upgrade notes from its own pin "
        "(the CHANGELOG's API and port lines, checked against its code), then "
        "close this issue once every consumer runs the week's release.",
        "",
    ]
    lines += [f"- [ ] {name} on {show(latest)}" for name in behind]
    return "\n".join(lines) + "\n"


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
    body = render(rows, latest, a.date)
    with open(a.out, "w", encoding="utf-8") as f:
        f.write(body)
    behind = lagging(rows, latest)
    for name, pin, by_rev in rows:
        print(f"{name}: {show(pin) if pin else ('untagged' if by_rev else 'no dstack tag')}")
    print(f"{len(behind)} behind {show(latest)}")
    if a.github_output:
        with open(a.github_output, "a", encoding="utf-8") as f:
            f.write(f"lagging={len(behind)}\nlatest={show(latest)}\n")


if __name__ == "__main__":
    main()
