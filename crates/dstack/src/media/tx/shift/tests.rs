use super::*;

use crate::clock::ReAnchor;

const RATE: u32 = 96_000;
const BLOCK: u64 = 32;
const DATE_STEP_NS: i64 = 1_600_000_000;
const DATE_STEP: i64 = 153_600;

fn stepped(epoch: u32, step_ns: i64) -> Anchor {
    Anchor {
        epoch,
        re_anchor: Some(ReAnchor { epoch, step_ns }),
    }
}

#[test]
fn the_engine_line_moves_once_an_epoch_by_the_step_on_the_block_grid() {
    let line = EngineLine::default();
    assert_eq!(
        line.shift_at(Anchor::from(4), RATE, BLOCK),
        0,
        "the first epoch"
    );
    assert_eq!(line.shift_at(Anchor::from(4), RATE, BLOCK), 0);
    // The first reader of a known step moves the line, the second reads it.
    assert_eq!(
        line.shift_at(stepped(5, DATE_STEP_NS), RATE, BLOCK),
        DATE_STEP
    );
    assert_eq!(
        line.shift_at(stepped(5, DATE_STEP_NS), RATE, BLOCK),
        DATE_STEP
    );
    // An unknown step moves nothing (the schedules start again anyway).
    assert_eq!(line.shift_at(Anchor::from(6), RATE, BLOCK), DATE_STEP);
    // A step under half a block: on the grid, nothing.
    assert_eq!(line.shift_at(stepped(7, 50_000), RATE, BLOCK), DATE_STEP);
    // A thread a re-anchor behind reads the line and moves nothing back: the
    // next step still counts from epoch 7.
    assert_eq!(
        line.shift_at(stepped(6, DATE_STEP_NS), RATE, BLOCK),
        DATE_STEP
    );
    assert_eq!(line.shift_at(Anchor::from(2), RATE, BLOCK), DATE_STEP);
    assert_eq!(
        line.shift_at(stepped(8, -2 * DATE_STEP_NS), RATE, BLOCK),
        -DATE_STEP
    );
    // A step off the block grid: the nearest whole blocks (200 ms at
    // 44.1 kHz is 8820 samples, 275.6 blocks of 32).
    assert_eq!(
        line.shift_at(stepped(9, 200_000_000), 44_100, BLOCK),
        -DATE_STEP + 8_832
    );
    // Two re-anchors between two looks: no step known.
    assert_eq!(
        line.shift_at(stepped(11, DATE_STEP_NS), RATE, BLOCK),
        -DATE_STEP + 8_832
    );
}

#[test]
fn only_the_engine_s_ring_is_read_on_its_line() {
    let stamp = 1_000_000;
    assert_eq!(source_position(&Source::Engine, stamp, 100), stamp - 100);
    assert_eq!(source_position(&Source::Engine, stamp, -100), stamp + 100);
    assert_eq!(source_position(&Source::Engine, stamp, 0), stamp);
    let others = [
        Source::Silence,
        Source::Counter,
        Source::Sine {
            hz: 1_000.0,
            dbfs: -30.0,
        },
        Source::Echo { rx: 0, offset: 10 },
    ];
    for source in others {
        assert_eq!(source_position(&source, stamp, 100), stamp, "{source:?}");
    }
}
