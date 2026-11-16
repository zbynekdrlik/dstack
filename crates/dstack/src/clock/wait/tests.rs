// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// What the fake OS saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ev {
    Escape,
    Block(i64),
    Yield,
    Pause,
}

/// A fake OS: its clock (the deadline's) moves by `step` at each read and
/// by a block's length; the app's clock is it plus `app_offset`, read
/// without moving it; `escape` is what an escape take answers.
struct Fake {
    now: i64,
    step: i64,
    app_offset: i64,
    escape: bool,
    log: Vec<Ev>,
}

impl Fake {
    fn at(now: i64, step: i64) -> Self {
        Fake {
            now,
            step,
            app_offset: 0,
            escape: false,
            log: Vec::new(),
        }
    }
}

impl WaitOs for Fake {
    fn now_ns(&mut self) -> i64 {
        let t = self.now;
        self.now += self.step;
        t
    }

    fn app_ns(&mut self) -> i64 {
        self.now + self.app_offset
    }

    fn take_escape(&mut self) -> bool {
        self.log.push(Ev::Escape);
        self.escape
    }

    fn block_for(&mut self, ns: i64) {
        self.log.push(Ev::Block(ns));
        self.now += ns;
    }

    fn spin_turn(&mut self, turn: Turn) {
        self.log.push(match turn {
            Turn::Yield => Ev::Yield,
            Turn::Pause => Ev::Pause,
        });
    }
}

/// A plan with no no-yield tail and no cap: the spin and the escape alone.
fn plan(spin_tail_ns: i64, escape: bool) -> Plan {
    Plan {
        spin_tail_ns,
        escape,
        escape_block_ns: 500_000,
        no_yield_ns: 0,
        give_cap_ns: i64::MAX,
    }
}

/// The wait with a fresh thread's gives.
fn wait(os: &mut Fake, target: i64, plan: Plan) {
    until(os, target, plan, &mut Gives::new());
}

mod guard;

/// The timer's wait (no tail): one block for the whole wait on the clock
/// it is given, no spin (dstack #328: the clock is the caller's, the app's
/// for the test clock).
#[test]
fn a_timer_wait_blocks_the_whole_wait_on_its_clock() {
    let mut os = Fake::at(1_000_000, 0);
    wait(&mut os, 1_010_000, plan(0, false));
    assert_eq!(os.log, [Ev::Block(10_000)]);
}

/// A tail: the timer to the tail, then yields until the clock reaches the
/// target, and not one turn more (each read moves the clock 1 µs).
#[test]
fn a_spin_wait_blocks_to_its_tail_then_yields_until_the_target() {
    let mut os = Fake::at(0, 1_000);
    wait(&mut os, 10_000, plan(3_000, false));
    // Read at 0 (left 10 000): block 7 000 to 8 000. Reads at 8 000 and
    // 9 000 are short; the read at 10 000 ends it.
    assert_eq!(os.log, [Ev::Block(7_000), Ev::Yield, Ev::Yield]);
    assert_eq!(os.now, 11_000, "three reads after the block");
}

/// A wait of exactly the tail is all spin: no zero-length block.
#[test]
fn a_wait_of_exactly_the_tail_never_blocks() {
    let mut os = Fake::at(0, 1_000);
    wait(&mut os, 2_000, plan(2_000, false));
    assert_eq!(os.log, [Ev::Yield]);
}

/// A target already reached: no block, no turn.
#[test]
fn a_past_target_returns_at_once() {
    let mut os = Fake::at(5_000, 1_000);
    wait(&mut os, 5_000, plan(3_000, false));
    assert!(os.log.is_empty(), "{:?}", os.log);
    let mut os = Fake::at(5_000, 0);
    wait(&mut os, 4_000, plan(0, false));
    assert!(os.log.is_empty(), "{:?}", os.log);
}

/// The bounded escape: taken first, a block of its own when it says so,
/// and the spin pauses instead of yielding; without the escape nobody asks.
#[test]
fn the_escape_is_taken_first_and_its_spin_pauses() {
    let mut os = Fake::at(0, 1_000);
    os.escape = true;
    wait(&mut os, 503_000, plan(2_000, true));
    // The escape's block moves the clock to 500 000: 3 000 left, 1 000
    // over the tail.
    assert_eq!(
        os.log,
        [Ev::Escape, Ev::Block(500_000), Ev::Block(1_000), Ev::Pause]
    );
    let mut os = Fake::at(0, 1_000);
    wait(&mut os, 3_000, plan(2_000, true));
    assert_eq!(os.log, [Ev::Escape, Ev::Block(1_000), Ev::Pause]);
    let mut os = Fake::at(0, 1_000);
    os.escape = true;
    wait(&mut os, 3_000, plan(2_000, false));
    assert_eq!(os.log, [Ev::Block(1_000), Ev::Yield]);
}
