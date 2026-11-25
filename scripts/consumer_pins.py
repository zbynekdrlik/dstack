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

An update keeps the lines the dstack session writes below the generated
block (`MARKER`), and the workflow's later Wednesday crons read only when
that day's read is not on the sync issue yet (`--once-a-day`, dstack #488).
"""
import argparse
import json
import re
import subprocess
import sys
import tomllib
import urllib.request
from typing import Callable, Optional

TITLE = "Weekly consumer version sync"

# The last line of the generated block: the lines below it are the dstack
# session's own and every run carries them into the new body (#488).
MARKER = "<!-- consumer-pins: the generated block ends here; lines below it are kept by every run -->"

WORK_IT = ("Work it: send each consumer behind its upgrade notes from its own pin "
           "(the CHANGELOG's API and port lines, checked against its code), then "
           "close this issue once every consumer runs the week's release.")

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
    lines += ["", WORK_IT, ""]
    if behind:
        lines += [f"- [ ] {name} on {show(latest)}" for name in behind] + [""]
    lines.append(MARKER)
    return "\n".join(lines) + "\n"


def read_prefix(date: str) -> str:
    """How every read's mark on the sync issue starts: the body's first
    line and the read's comment."""
    return f"Wednesday read {date}:"


def read_comment(date: str, behind: int, latest: Version) -> str:
    """The comment a read leaves on the sync issue it updates or closes."""
    if behind == 0:
        return f"{read_prefix(date)} every consumer runs {show(latest)}."
    return f"{read_prefix(date)} {behind} consumer(s) behind {show(latest)}."


def generated(line: str, names: list[str]) -> bool:
    """Whether a line of a body written before the marker existed is one
    `render` writes (the read's header, the table, the fixed paragraph, a
    known consumer's checklist line); every other line is a hand line."""
    if re.match(r"Wednesday read \d{4}-\d{2}-\d{2}: ", line):
        return True
    if line in ("| Consumer | Pin | |", "|---|---|---|", WORK_IT):
        return True
    for name in names:
        if line.startswith(f"| {name} | ") and line.endswith(" |"):
            return True
        if re.fullmatch(rf"- \[[ xX]\] {re.escape(name)} on v\d+\.\d+\.\d+", line):
            return True
    return False


def hand_lines(old: str, names: list[str]) -> list[str]:
    """The old body's lines the generated block does not own: everything
    below the marker as written, or, in a body without it, every line that
    is not a generated one (blank runs collapsed); no leading or trailing
    blank line."""
    lines = old.splitlines()
    if MARKER in lines:
        kept = lines[lines.index(MARKER) + 1:]
    else:
        kept = []
        for line in lines:
            if generated(line, names):
                continue
            if line.strip() == "" and (not kept or kept[-1].strip() == ""):
                continue
            kept.append(line)
    while kept and kept[0].strip() == "":
        kept.pop(0)
    while kept and kept[-1].strip() == "":
        kept.pop()
    return kept


def merge(old: Optional[str], new: str, names: list[str]) -> str:
    """The new body: the generated block `new`, its checklist lines ticked
    where the old body ticked the same line (the same consumer and release),
    then the old body's hand lines (#488: a run used to overwrite them)."""
    if not old:
        return new
    ticked = {line[6:] for line in old.splitlines() if re.match(r"- \[[xX]\] ", line)}
    lines = [f"- [x] {line[6:]}" if line.startswith("- [ ] ") and line[6:] in ticked else line
             for line in new.splitlines()]
    body = "\n".join(lines) + "\n"
    kept = hand_lines(old, names)
    if kept:
        body += "\n" + "\n".join(kept) + "\n"
    return body


def sync_issue(issues: list[dict]) -> Optional[tuple[int, str]]:
    """The open sync issue's number and body (`gh issue list --json`
    rows), the newest when there are several; None when none is open."""
    open_ = [i for i in issues if i["title"] == TITLE and i["state"] == "OPEN"]
    if not open_:
        return None
    newest = max(open_, key=lambda i: i["number"])
    return newest["number"], newest["body"]


def already_read(issues: list[dict], date: str) -> bool:
    """Whether the read of `date` already ran: a sync issue open, or closed
    that day, whose body starts with the day's read (written by an update,
    a close and a create alike) or that has the day's read comment."""
    prefix = read_prefix(date)
    for i in issues:
        if i["title"] != TITLE:
            continue
        if i["state"] != "OPEN" and (i.get("closedAt") or "")[:10] != date:
            continue
        if (i.get("body") or "").startswith(prefix):
            return True
        if any((c.get("body") or "").startswith(prefix) for c in i.get("comments") or []):
            return True
    return False


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
    ap.add_argument("--github-output", help="GITHUB_OUTPUT file for the step's outputs")
    ap.add_argument("--issues", help="the sync issues as `gh issue list --json "
                    "number,title,state,body,closedAt,comments` wrote them")
    ap.add_argument("--once-a-day", action="store_true",
                    help="read nothing (skip=1) when the day's read is already on the sync issue")
    a = ap.parse_args()
    latest = parse_version(a.latest)
    issues = []
    if a.issues:
        with open(a.issues, encoding="utf-8") as f:
            issues = json.load(f)
    outputs = {"date": a.date}
    if a.once_a_day and already_read(issues, a.date):
        print(f"the read of {a.date} already ran: nothing to do")
        outputs["skip"] = "1"
    else:
        with open(a.consumers, "rb") as f:
            consumers = tomllib.load(f)["consumer"]
        rows = collect(consumers, fetch_raw(a.owner))
        found = sync_issue(issues)
        names = [c["name"] for c in consumers]
        body = merge(found[1] if found else None, render(rows, latest, a.date, library_changed), names)
        with open(a.out, "w", encoding="utf-8") as f:
            f.write(body)
        behind = lagging(rows, latest, library_changed)
        for name, pin, by_rev in rows:
            print(f"{name}: {show(pin) if pin else ('untagged' if by_rev else 'no dstack tag')}")
        print(f"{len(behind)} behind {show(latest)}; sync issue: {f'#{found[0]}' if found else 'none open'}")
        outputs.update(skip="0", lagging=str(len(behind)), latest=show(latest),
                       issue=str(found[0]) if found else "",
                       comment=read_comment(a.date, len(behind), latest))
    if a.github_output:
        with open(a.github_output, "a", encoding="utf-8") as f:
            f.write("".join(f"{k}={v}\n" for k, v in outputs.items()))


if __name__ == "__main__":
    main()
