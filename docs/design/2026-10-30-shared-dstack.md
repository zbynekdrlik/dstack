# One Dante stack for all our applications (proposal, 2026-10-30)

Owner's goal: a single, robust, CI-tested Dante stack, so no project solves a Dante problem another project already solved.

## Today

`iem-dante` lives in iemmixer (about 27 000 lines) and as a copy here, 19 commits apart. dsync owns the clock. Every further Dante application would be another copy.

## Shape

- One repository, `dstack`, is the only place Dante code lives: discovery, ARC, CMC, flows, media TX/RX, clock client, health. Public, because iemmixer is public and its CI must fetch it; no site data, same hygiene as iemmixer.
- Applications depend on a tagged release by Cargo git tag. No copies, no local patches. Each application's CI fails on Dante protocol code outside the dependency.
- One owner: the Dante session owns dstack and dasio. Other projects file tickets there; they do not fix Dante locally.
- dsync stays its own project; the stack uses it through the DSYX contract, tested in the stack's CI.

## What makes it robust

1. **Quirk catalog.** Each behaviour met in the field (DVS replying from another port, hardware flow limits, 0x0311, 0x0315, DVS late stamps) is a recorded capture plus a replay test. A field bug is not closed without one.
2. **Conformance CI.** Linux and Windows, parser fuzzing, mutation gate on the diff, no-allocation and rtsan proofs on the hot path.
3. **Real-hardware gate.** A self-hosted runner on the Dante network subscribes both ways with hardware Dante and DVS, and gates a release on late packets, measured by a receiver and not only by us.
4. **Telemetry that sees what Dante Controller sees.** Arrival minus stamp per flow, in both directions. The 5 ms stall went unseen by our counters: that is a test gap, and the first catalog entry.
5. **Releases.** Semantic versions and a changelog naming each fixed quirk. Moving an application is a version bump plus its own tests.

## Order

1. Create dstack from dasio's copy, which has the Windows port, and take iemmixer's later changes.
2. dasio consumes it (done when its CI is green with no local copy).
3. Fix the open TX and RX problems in the stack, each with a catalog test.
4. iemmixer moves to the tag in a dev window, proven by its box tests, before a Thursday event.
5. Real-hardware gate.
