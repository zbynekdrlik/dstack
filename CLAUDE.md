# dstack

The one Dante expert project. All Dante code lives here: discovery, ARC, CMC, flows, media TX/RX, clock client, health. Applications depend on a tagged release by Cargo git tag; nobody keeps a copy or a local patch.

Repo `zbynekdrlik/dstack` (renamed from dante-stack on 31.10.2026). The crates keep their names (`dante-stack`, `dante-audio-io`, binary `dante-stack-dev`): consumers write `git = "https://github.com/zbynekdrlik/dstack"`.

Start from `docs/HANDOVER.md`, then `docs/design/2026-10-30-shared-dante-stack.md`.

- Your job: own every Dante problem and make the stack robust, so no project solves a Dante problem twice.
- A Dante bug reported by a consumer is fixed here, with a catalog test, and released; the consumer only moves the version.
- A field bug is closed only with a recorded capture and a replay test (quirk catalog).
- Branches: `dev` and `main` only; PRs dev → main; merge commits.
- Public repo: no site data. Addresses and real device or channel names live only in `~/.config/dstack/`.
- dantesync is the only clock owner on a PC; the stack never opens UDP 319/320.
- dsync (dantesync, repo zbynekdrlik/dsync) is its own project, and its only peer is dstack. Every request to dsync from another project comes through dstack, and dstack decides it. Audio stability outranks video (owner 1.11.2026, airuleset#1280).
- Never change another device's subscriptions without the owner's approval. Tests subscribe our own device to itself.
- Every message to the owner in Slovak; code, commits and docs in English.

## Playbook router
- releases, versions, CHANGELOG, dependencies → `.claude/rules/release.md` (auto-loads on its `paths:`)
- tests, whole-device tests, the mutation gate → `.claude/rules/tests.md` (auto-loads on its `paths:`)
