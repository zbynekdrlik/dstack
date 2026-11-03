use super::*;

fn clock() -> PassClock {
    PassClock {
        now_ns: 5_000_000_000,
        pass: Instant::now(),
        rate: 96_000,
        margin_ns: 250_000,
        latency_ns: 1_000_000,
    }
}

#[test]
fn a_pass_s_floor_is_the_flow_s_latency_floor() {
    let c = clock();
    // A short packet: the advertised latency.
    assert_eq!(c.floor(16), 1_000_000);
    // A packet longer than the latency allows: its own frames + margin.
    let long = flow_latency_ns(1_000_000, 256, 96_000, 250_000);
    assert!(long > 1_000_000, "{long}");
    assert_eq!(c.floor(256), long);
}

#[test]
fn a_pass_s_now_is_its_time_plus_the_time_since() {
    let c = clock();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let now = c.now();
    assert!(now >= c.now_ns + 2_000_000, "{now}");
    assert!(now < c.now_ns + 2_000_000_000, "{now}");
}
