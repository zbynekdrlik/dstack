---
paths:
  - "crates/dstack-resample/**"
  - "crates/dlink/src/rated.rs"
  - "crates/dlink/src/rated/**"
  - "crates/dlink/tests/rated_*.rs"
---

# The converter (dstack-resample)

- **Two engines, one filter** (#462): a fixed ratio (`Converter::new`, `low_delay` off 2:1/1:2) runs `poly.rs`'s table of kernels; a steered one (`drifting`, `drifting_with_taps`) and a ratio over `poly::MAX_PHASES` run rubato's `Async`. `Converter::drifting` at its nominal ratio IS the rubato stage, so it is the reference a table change is checked against sample for sample (`tests/polyphase.rs`, within 1e-9: rubato's own float position error reached 3.6e-10 at 22.05 -> 96 kHz). Its per-call counts differ from the table's only on a whole boundary (`(T - 2) * r` integer, rubato's float `floor`/`ceil`): compare the fixed side per call and the other side's running sum within one frame. A float-position change shows as THD+N: the table reads 222-231 dB where rubato reads 194-219 dB at the same settings.
- **The cost is memory layout, not the kernel** (#462, dev1 Zen3): rubato already blends one kernel a frame for all channels; its 64 per-channel windows of 257 x 8 B (128 KB) a frame stream from L2. Rows interleaving all 64 channels (512 B stride) put one output's window on 8 L1 sets (conflict misses, 1.5x only); contiguous groups of 16 channels (one window 32 KB) gave 2x. Groups of 32 and two partial sums per 16-group measured no better. Computing two outputs at once in one loop (`dot_pair`, four zipped slices or indexed rows) ran 3x SLOWER: check a hot-loop change with a release bench before keeping it.
- **Benching on dev1:** load is 20-100 from other accounts. Run a release bench binary under `chrt -f 10` and compare minima/medians of the same run, A/B in one invocation; a plain run's mean is the run queue's. `crates/dlink/tests/rated_cost.rs` (dasio's shape, release only, CI Linux) read a median of 689 µs plain at load 80 and 587-601 µs under `chrt -f 10` (bound 667). Build the bench as a throwaway `tests/zz_*.rs` and move it out before the commit.
- **SIMD without a target-cpu:** the workspace builds for baseline x86_64 (SSE2). The table's loop is plain Rust over fixed-size arrays (`as_chunks::<N>()`; clippy refuses `chunks_exact` with a const size), compiled twice: an `#[target_feature(enable = "fma")]` copy chosen once by `is_x86_feature_detected!("fma")` and a portable one. `f64::mul_add` outside an FMA-enabled function is a libm call: use it only under `FMA`. The portable copy never runs on dev1 or CI: a unit test calls both copies against a plain sum (`poly/tests.rs`).
- **Capacities and the gate:** a buffer's room or a "compact now?" decision never shows in the output, so its mutants survive; keep them as pure functions pinned with numbers (`poly::room_for`, `poly::must_compact`, as `rated::rooms`).
