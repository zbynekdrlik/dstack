---
paths:
  - "crates/dlink/src/bridge.rs"
  - "crates/dlink/src/bridge/**"
  - "crates/dsend/src/**"
  - "crates/dlisten/src/**"
  - "crates/dstack-endpoint/src/**"
---

# The app-clock bridge (dlink::bridge: Fifo, Player, AppBuffer)

- **The target is a fixed device term** (`app_buffer_ns`, #247, owner 13.11.2026): the app's chunk + one pump block at the device rate, or the configured value verbatim. Never measured, never grown after a dropout. A dropout is counted (`Stats::underruns`) and re-primes to the same target. Do not bring back a cushion or a fixed constant.
- **Chunked producer vs chunked consumer:** the depth is a sawtooth of the wider swing (dsend's 10 ms packets, OBS's 1024-frame ticks on the producer side; dlisten's 10 ms output callback on the consumer side). Priming adds half the wider swing (`max(callback_in, chunk_in)`), so the MEAN sits at the target. Without it a producer-chunked bridge at a target of chunk + block starts at the sawtooth's top and runs under the floor: a 48 kHz capture on a 96 kHz network dropped out every 20 ms (#247).
- **The floor and the excess follow the target:** floor = min(5 ms, target/4) (the sawtooth reaches target - chunk/2); hard excess = max(2 x target, 2 x chunk). A 5 ms floor or a 2 x target excess alone cut the small default targets.
- **Drift costs depth at the start:** the ASRC correction slews 5 ppm/s (`asrc::MAX_SLEW_PPM_PER_S`), so a clock 200 ppm off costs about 4 to 6 ms of depth over its first ~40 s. At the default (10 ms + block) ±100 ppm holds; 200 ppm drops out once or twice on one sign. The simulated-clock tests pin ±100 ppm at the default and ±200 ppm at a set 20 ms; never "fix" them by loosening the asserts.
- **A test whose bridge carries a burst** (a 49 ms block in one call, a 100 ms push) needs a target whose excess bound covers it (a set 40 ms), or the player skips the burst as an excess by design.
- **`recentres` mixes floor deficits and excess skips:** a test cannot tell them apart from the count. A short target (under chunk/2 + block, `AppBuffer::short`) re-centres on the floor every chunk whatever the excess bound, so test the excess bound with the pure `hard_excess_s`.
- **An app on its own clock stamps by `Block::play_at()`** (#295, v0.6.132): the block's media start + P + (c) in the app's clock (`clock::app_monotonic_ns`: Linux CLOCK_MONOTONIC, Windows RAW QPC ns, not the steered `clock::monotonic_ns`). Stamping "first arrival + sample count" drifts by the media-vs-local rate (~6 ppm = ~21 ms/h on stream-snv) and forced obs's hourly re-anchor step. The pair is read app, media, app once per pump wake and its offset filtered (#295 step 2, v0.6.135, `stream::app_offset`: >50 us off rejected and counted `play_at_rejected`, 100 ppm slew, a step taken on its 3rd alike wake with a WARN; a re-anchor shifts the filter by its known step or restarts it); a test that drives a fake media clock faster than real time sees the filter hold an early offset, so bound real-clock stamps by the media time run; P + B on the sample grid can read 1 ns above dlink's logged RX path (15 000 000 vs 14 999 999 ns), so compare with a 1 ns tolerance.
