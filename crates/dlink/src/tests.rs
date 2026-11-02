use super::*;

fn config() -> Config {
    Config::new("app-link", "/tmp/never-written.toml")
        .rx(["In 1", "In 2"])
        .tx(["Out 1", "Out 2"])
}

#[test]
fn the_defaults_are_10_ms_normal_timing_and_half_the_latency_as_pump_margin() {
    let cfg = config().device().unwrap();
    assert_eq!(cfg.latency_ns, 10_000_000);
    assert_eq!(cfg.timing, TimingProfile::Normal);
    assert_eq!(cfg.sample_rate, 48_000);
    // 5 ms at 48 kHz.
    assert_eq!(cfg.work_margin, 240);
    assert_eq!(cfg.encoding, 24);
    assert_eq!(cfg.name, "app-link");
    assert_eq!(cfg.rx, ["In 1", "In 2"]);
    assert_eq!(cfg.tx, ["Out 1", "Out 2"]);
    assert_eq!(cfg.manufacturer, "dlink");
    assert_eq!(cfg.model, "dlink");
    // No CPU pinning: the app's threads stay where the OS puts them.
    assert_eq!(cfg.pump_cpu, None);
    assert_eq!(cfg.io_cpu, None);
}

#[test]
fn the_app_s_rate_latency_and_product_are_taken_and_the_margin_follows() {
    let cfg = config()
        .sample_rate(96_000)
        .latency_ns(20_000_000)
        .product("SongPlayer", "sp")
        .device()
        .unwrap();
    assert_eq!(cfg.sample_rate, 96_000);
    assert_eq!(cfg.latency_ns, 20_000_000);
    assert_eq!(cfg.work_margin, 960);
    assert_eq!(
        (cfg.manufacturer.as_str(), cfg.model.as_str()),
        ("SongPlayer", "sp")
    );
}

#[test]
fn a_latency_under_4_ms_and_a_bad_name_are_refused_with_every_problem() {
    let problems = Config::new("-bad", "/tmp/x.toml")
        .tx(["Out"])
        .latency_ns(3_000_000)
        .device()
        .unwrap_err();
    assert!(
        problems
            .iter()
            .any(|p| p.starts_with("latency_ns: at least 4000000")),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|p| p.starts_with("name")),
        "{problems:?}"
    );
}

#[test]
fn a_config_the_device_refuses_is_a_setup_error_before_anything_runs() {
    struct Silent;
    impl Process for Silent {
        fn process(&mut self, _block: &mut Block<'_>) {}
    }
    let bad = config().latency_ns(1_000_000);
    let e = Link::start_on(
        &bad,
        &Place::Ip(std::net::Ipv4Addr::new(192, 0, 2, 1)),
        Map {
            rx: vec![],
            tx: vec![],
        },
        Silent,
        Clock::System,
    )
    .err()
    .expect("refused");
    match e {
        HostError::Setup(why) => {
            assert!(why.starts_with("dlink config: "), "{why}");
            assert!(why.contains("latency_ns: at least 4000000"), "{why}");
        }
        other => panic!("{other:?}"),
    }
}
