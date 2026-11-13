---
paths:
  - "crates/dstack-asio/**"
---

# The ASIO host (dstack-asio, #243)

- **Lifted from SongPlayer, byte for byte where it writes:** `format::Sample::encode` is SongPlayer's `asio_format` encode on `f64`; `an_f32_widened_to_f64_writes_songplayers_bytes` pins it against SongPlayer's formula written out in the test. Never change the rounding or the full scales: SongPlayer moves onto this crate with no behaviour change.
- **Every decision is pure and Linux-tested; `win.rs` only calls the driver** and is excluded from the mutation gate (`.cargo/mutants.toml`). A new Windows decision goes into `state`, `device`, `engine` or `runner` with a test, never into `win.rs`. Times are ns (SongPlayer's are 100 ns).
- **The buffer switch is `Engine`:** decode the used inputs, run the app's `Process` under `catch_unwind`, encode every driver output (unmapped ones zeroed). It allocates nothing; a panic plays silence from then on and the `Runner` closes the run (`faults > 0`).
- **A process that loads an ASIO driver spawns no child** (SoundGrid resyncs on any spawn from that process, songplayer #233). dasio's instance process must keep to it.
- **Bridge targets** (#247): the ASIO buffer + one pump block by default (256 at 96 kHz: 4.0 ms). The simulated-clock tests pin ±50 ppm at the default and ±100 ppm at a set 5 ms; at ±100 ppm the default drops out twice at the start (5 ppm/s slew, 60 s rate lock). Never loosen the asserts to make a default hold more.
- **The bridge simulation tests take ~17 s** (two sinc converters at 96 kHz for 180 s each): a crate test run passes the harness's 180 s background limit only with `timeout 170`; run a throwaway probe of other drifts/targets as its own `cargo test <name>` in the background and wait on its output file (never end the turn while it runs as a subagent).
- **azo 0.4.0 cross-compiles** for `x86_64-pc-windows-gnu` on dev1 (its build script runs windows-bindgen on the host): check `cargo clippy --target x86_64-pc-windows-gnu -p dstack-asio` before a push.
