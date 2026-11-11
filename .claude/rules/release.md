---
paths:
  - "Cargo.toml"
  - "Cargo.lock"
  - "CHANGELOG.md"
  - "fuzz/Cargo.toml"
  - "fuzz/Cargo.lock"
  - ".github/workflows/**"
---

# Releasing dstack

- **Bump first.** `[workspace.package].version` in `Cargo.toml` is the first commit of every PR (`scripts/check_version.py` fails a PR not above main). Rebuild `fuzz/` too: its lockfile records the stack's version and CI checks it with `cargo metadata --locked`.
- **Merge** a green, clean PR with `gh api -X PUT repos/zbynekdrlik/dstack/pulls/<N>/merge -f merge_method=merge`. `gh pr merge` once failed in GraphQL after the merge commit landed, leaving the PR open with merged=false.
- **Tag** the merge commit on main, annotated: `git tag -a vX.Y.Z -m "..." origin/main`, push the tag, then fast-forward `dev` to main and push it. Watch the main and tag CI runs to green.
- **CHANGELOG** names every change an application can notice, not only new API: `stream::BLOCKS` widening broke iemmixer's RT tests once without a changelog line. A removed or changed public item, a JSON field, a default: listed, with what a consumer ports.
- **A new state-file key** (`DeviceState` is `deny_unknown_fields`): give it `serde(default, skip_serializing_if = ...)` so old files load and a state without it is written as before, and say in the CHANGELOG that an older dstack refuses a file holding it, with how to downgrade (#207, `[[multicast]]`).
- **dlink latency:** a release notice states whether it moves a dlink app's latency (the budget, its split, the block or the converter's delay; v0.6.22, #68), with the numbers before and after. Say "no latency change" when it does not.
- **Tell the consumers** over peer (iemmixer, dasio, songplayer as it applies) with the tag and what they must change.
- **New dependencies** must be published at least two weeks ago (`curl https://crates.io/api/v1/crates/<c>/<v>` gives `created_at`); pin older with `cargo update -p <c> --precise <v>`. Seed a new lockfile (fuzz) from the main `Cargo.lock` so tested versions carry over.
- **Public repo:** no site addresses or real device names in code, commits, issues or PR text; they live in `~/.config/dstack/`.
- **A changed public signature:** check the fuzz crate too, before the push (`cargo check --manifest-path fuzz/Cargo.toml --bin <target>` for each target in `fuzz/fuzz_targets/`). Only the fuzz job builds it, so a missed call site shows only there (v0.4.5: `cmc::handle_cmc` took the process id).
- **License:** every crate is MIT OR Apache-2.0 (owner 3.11.2026: as permissive as possible; never ask the owner about licenses). Third-party code may inform us as a protocol reference (facts: layouts, opcodes, timeouts) but is never translated in; its own license would follow it (v0.6.12, #102: inferno-shaped functions rewritten). New files carry `SPDX-License-Identifier: MIT OR Apache-2.0`. When searching for an attribution, search phrases that cannot break across lines (`grep -rlE "Adapted from|Teo and the"`): a header split across lines hid 4 files from the first audit.
- **A push that GitHub answered with 500** can land on the branch without firing CI or moving the open PR's head (v0.6.23: the PR stayed on the old commit, no run). Check `gh api repos/zbynekdrlik/dstack/pulls/<N> --jq .head.sha` against `git ls-remote origin refs/heads/dev`; if they differ, close and reopen the PR (`gh api -X PATCH .../pulls/<N> -f state=closed`, then `state=open`) and cancel the run for the stale head. The local `gh` is old: `gh pr checks` has no `--json`; poll `gh run view <id> --json status` instead.
- **A PR body never starts a field-bug line with "Fix #N"** (or Fixes/Closes/Resolves): GitHub closes the issue at the merge to main even when every commit says `Refs #N`. Write "Refs #N" or "(#N)". v0.6.81 closed #180 before dcontroller's live check.
