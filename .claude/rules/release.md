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
- **Tell the consumers** over peer (iemmixer, dasio, songplayer as it applies) with the tag and what they must change.
- **New dependencies** must be published at least two weeks ago (`curl https://crates.io/api/v1/crates/<c>/<v>` gives `created_at`); pin older with `cargo update -p <c> --precise <v>`. Seed a new lockfile (fuzz) from the main `Cargo.lock` so tested versions carry over.
- **Public repo:** no site addresses or real device names in code, commits, issues or PR text; they live in `~/.config/dstack/`.
- **A changed public signature:** check the fuzz crate too, before the push (`cargo check --manifest-path fuzz/Cargo.toml --bin <target>` for each target in `fuzz/fuzz_targets/`). Only the fuzz job builds it, so a missed call site shows only there (v0.4.5: `cmc::handle_cmc` took the process id).
- **License:** every crate is MIT OR Apache-2.0 (owner 3.11.2026: as permissive as possible; never ask the owner about licenses). Third-party code may inform us as a protocol reference (facts: layouts, opcodes, timeouts) but is never translated in; its own license would follow it (v0.6.12, #102: inferno-shaped functions rewritten). New files carry `SPDX-License-Identifier: MIT OR Apache-2.0`. When searching for an attribution, search phrases that cannot break across lines (`grep -rlE "Adapted from|Teo and the"`): a header split across lines hid 4 files from the first audit.
