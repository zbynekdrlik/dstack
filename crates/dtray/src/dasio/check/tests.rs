// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// An instance config (made-up names): buffer 64, app reserve 128.
const TEXT: &str = r#"name = "test-asio"
manufacturer = "dasio"
model = "dasio"
sample_rate = 96000
encoding = 24
latency_ns = 2000000
block = 64
work_margin = 128 # the reserve
state_file = "C:/ProgramData/dasio/instance-1/state.toml"
rx = ["In 1", "In 2"]
tx = ["Out 1", "Out 2"]
"#;

fn fields(text: &str) -> Fields {
    Fields::read(text).expect("the test config reads")
}

fn normal() -> String {
    format!(
        "timing = \"normal\"\n{}",
        TEXT.replace("2000000", "5000000")
            .replace("work_margin = 128", "work_margin = 192")
    )
}

#[test]
fn the_least_reserve_is_one_buffer_or_2_ms_under_normal() {
    let mut f = fields(TEXT);
    assert!(!f.normal);
    assert_eq!(least_work_margin(&f), 64);
    f.block = 256;
    assert_eq!(least_work_margin(&f), 256);
    let mut n = fields(&normal());
    assert!(n.normal);
    // 2 ms at 96 kHz is 192 samples.
    assert_eq!(least_work_margin(&n), 192);
    n.block = 256;
    assert_eq!(least_work_margin(&n), 256);
    n.sample_rate = 48_000;
    n.block = 64;
    assert_eq!(least_work_margin(&n), 96);
}

#[test]
fn a_bigger_buffer_raises_the_reserve_to_what_dstack_takes() {
    let mut f = fields(TEXT);
    assert_eq!(reserve_for(&f), 128);
    f.block = 128;
    assert_eq!(reserve_for(&f), 128);
    f.block = 256;
    assert_eq!(reserve_for(&f), 256);
    let mut n = fields(&normal());
    n.work_margin = 64;
    assert_eq!(reserve_for(&n), 192);
}

#[test]
fn a_refused_file_is_said_before_apply() {
    let old = fields(TEXT);
    assert_eq!(file_problem(TEXT, &old, &old), None);
    // A live change only (the RX latency): no file is written.
    let mut live = old.clone();
    live.latency_ns = 1_000_000;
    assert_eq!(file_problem(TEXT, &old, &live), None);
    let mut fine = old.clone();
    fine.work_margin = 256;
    assert_eq!(file_problem(TEXT, &old, &fine), None);
    // A buffer over the reserve: the config refuses it.
    let mut under = old.clone();
    under.block = 256;
    let e = file_problem(TEXT, &old, &under).expect("refused");
    assert!(e.contains("work_margin: 256 to 4096 samples"), "{e}");
    // With a live latency changed beside it, the file keeps its own.
    under.latency_ns = 1_000_000;
    under.work_margin = 256;
    assert_eq!(file_problem(TEXT, &old, &under), None);
    // A text that does not read is its reason.
    assert!(
        file_problem("name = ", &old, &fine)
            .expect("refused")
            .starts_with("not TOML")
    );
}
