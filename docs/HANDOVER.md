# Handover: dstack (2026-10-30)

## Status

Empty repository, created by airuleset (ticket airuleset#1270). No code yet. Step 1 of the approved design is yours: create the stack from dasio's copy of `iem-dante`, which has the Windows port, and take iemmixer's later changes. The two copies are 19 commits apart; the code is about 27 000 lines.

Consumers today: dasio (MVP passed the owner's listening test on 2026-10-30, production is back on VB-Matrix) and iemmixer (Gen 3 on its Linux box, in production for the band).

## Machines

- dev1: this project's account. No local Dante network access is assumed.
- The FOH PC (MCP `win-ableton-foh`), the SNV Resolume PC with Dante Virtual Soundcard (MCP `win-resolume-snv`), the IEM PC and the Gen 3 box. Addresses are in `~/.config/dstack/SITE.md`.

## What else acts on the machines

- dantesync runs on every PC as the only clock owner. Clients follow it through the DSYX contract.
- On the FOH PC: Ableton (master and band), VB-Matrix, the console's Yamaha card, and the dasio device (scheduled task `dante-asio-1`, instance 1).
- Windows updates or a replugged USB NIC can leave DVS stamping 61 ms late while PTP looks locked: restart `dvs.manager` and check latency after every DVS start.
- fohmixer runs on the same FOH PC. iemmixer owns the IEM PC and the Gen 3 box.

## How to run a test

CI first: GitHub-hosted Linux and Windows. For a real Dante test, subscribe our own device to itself and read the receiver's Latency tab in Dante Controller, or `netaudio` on the CLI. Routing status green says nothing about stalls: the 5.4 ms TX stall of 2026-10-30 was late on about 1 % of packets with green routing and our counters at 0. Never change another device's subscriptions without the owner.

## Site facts

Private, never in this repository: `~/.config/dstack/SITE.md`, with the tools beside it.

## Runners

Needed: one self-hosted runner on the Dante network, for the real-hardware gate (design point 3, step 5). Not registered yet. Ask airuleset for it, with the box you chose: the controller registers runners, a project token cannot.

## Backlog

Owner-requested:
- One stack for all Dante applications, with no project solving a problem twice (2026-10-30).
- Design order 1-5 in `docs/design/2026-10-30-shared-dstack.md`, approved by the owner.
- Improve dantesync only for a measured need, "nie len tak pre dobry pocit" (2026-10-26).

Proposed by a session, not yet asked for by the owner: nothing.

## Knowledge left in other projects

- dasio: `docs/HANDOFF.md`, `docs/design/`, issue #1 (TX stalls, DVS refusal, RX hands).
- iemmixer: `.claude/rules/gen3.md`, `gen3-new-box.md`, `dante-labels.md`, `asio-spike.md`, and its memory notes on Dante. Ask the iemmixer session over the peer channel, then copy what you need here.
- dantesync: its own repository; the iemmixer session has maintained it so far.
