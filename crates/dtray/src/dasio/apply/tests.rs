// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

fn want() -> Fields {
    Fields {
        name: "test-asio".into(),
        iface: None,
        block: 128,
        latency_ns: 2_000_000,
        tx_latency_ns: None,
        sample_rate: 96_000,
        rx: vec!["1".into(), "2".into()],
        tx: vec!["1".into(), "2".into()],
        cpus: Default::default(),
        bridge: None,
    }
}

fn good() -> ReadBack {
    ReadBack {
        block: Some(128),
        rx: Some(2),
        tx: Some(2),
        latency_ns: Some(2_000_000),
        iface: None,
        driver: None,
    }
}

/// A PC whose DAW holds the driver for `held` polls and whose device
/// states nothing for `silent` reads.
#[derive(Default)]
struct Fake {
    held: u32,
    /// The status does not say (`None`) for this many reads.
    unknown: u32,
    silent: u32,
    back: Option<ReadBack>,
    latency_err: bool,
    install_err: bool,
    log: Vec<String>,
}

impl Ops for Fake {
    fn set_latency(&mut self, ns: u32) -> Result<(), String> {
        self.log.push(format!("latency {ns}"));
        if self.latency_err {
            Err("refused".into())
        } else {
            Ok(())
        }
    }
    fn daw_attached(&mut self) -> Option<bool> {
        self.log.push("daw".into());
        if self.unknown > 0 {
            self.unknown -= 1;
            return None;
        }
        Some(self.held > 0)
    }
    fn wait(&mut self) {
        self.log.push("wait".into());
        self.held = self.held.saturating_sub(1);
    }
    fn install(&mut self, config: &str) -> Result<String, String> {
        self.log.push(format!("install {config}"));
        if self.install_err {
            Err("declined".into())
        } else {
            Ok("written".into())
        }
    }
    fn read_back(&mut self) -> ReadBack {
        self.log.push("read".into());
        if self.silent > 0 {
            self.silent -= 1;
            return ReadBack::default();
        }
        self.back.clone().unwrap_or_else(good)
    }
}

fn plan(latency: Option<u32>, config: bool, release: bool) -> Plan {
    Plan {
        latency,
        config: config.then(|| "text".to_string()),
        release,
    }
}

#[test]
fn a_live_change_writes_the_latency_and_reads_back_without_a_restart() {
    let mut f = Fake::default();
    let mut said = Vec::new();
    let out = run(&plan(Some(2_000_000), false, false), &want(), &mut f, |s| {
        said.push(s.to_string())
    });
    assert!(out.expect("done").starts_with("Applied: "));
    assert_eq!(f.log, ["latency 2000000", "read"]);
    assert_eq!(said, ["Setting the RX latency…"]);
}

#[test]
fn a_reshape_waits_for_the_release_then_installs_and_reads_back() {
    let mut f = Fake {
        held: 2,
        silent: 2,
        ..Fake::default()
    };
    let mut said = Vec::new();
    let out = run(&plan(None, true, true), &want(), &mut f, |s| {
        said.push(s.to_string())
    });
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(
        f.log,
        [
            "daw",
            "wait",
            "daw",
            "wait",
            "daw",
            "install text",
            "read",
            "wait",
            "read",
            "wait",
            "read"
        ]
    );
    assert_eq!(said.len(), 3);
    assert!(said[0].starts_with("Waiting for the DAW"));
    assert!(said[1].starts_with("Writing the settings"));
    assert!(said[2].starts_with("Restarted"));
}

#[test]
fn a_restart_of_the_same_shape_does_not_wait_for_the_daw() {
    let mut f = Fake {
        held: 5,
        ..Fake::default()
    };
    assert!(run(&plan(None, true, false), &want(), &mut f, |_| {}).is_ok());
    assert_eq!(f.log, ["install text", "read"]);
}

#[test]
fn a_daw_that_never_releases_writes_nothing() {
    let mut f = Fake {
        held: u32::MAX,
        ..Fake::default()
    };
    let err = run(&plan(None, true, true), &want(), &mut f, |_| {}).expect_err("held");
    assert_eq!(
        err,
        "the DAW did not release the driver within 10 minutes; nothing written"
    );
    assert_eq!(
        f.log.iter().filter(|l| *l == "wait").count(),
        RELEASE_POLLS as usize
    );
    assert!(!f.log.iter().any(|l| l.starts_with("install")));
}

#[test]
fn a_refused_step_stops_the_apply() {
    let mut f = Fake {
        latency_err: true,
        ..Fake::default()
    };
    assert_eq!(
        run(&plan(Some(1), true, false), &want(), &mut f, |_| {}),
        Err("the RX latency was not set: refused".to_string())
    );
    assert_eq!(f.log, ["latency 1"]);
    let mut f = Fake {
        install_err: true,
        ..Fake::default()
    };
    assert_eq!(
        run(&plan(None, true, false), &want(), &mut f, |_| {}),
        Err("declined".to_string())
    );
    assert!(!f.log.iter().any(|l| l == "read"));
}

#[test]
fn a_device_that_never_states_its_values_fails_after_the_polls() {
    let mut f = Fake {
        silent: u32::MAX,
        ..Fake::default()
    };
    let err = run(&plan(None, true, false), &want(), &mut f, |_| {}).expect_err("silent");
    assert!(err.contains("not read back"), "{err}");
    assert_eq!(
        f.log.iter().filter(|l| *l == "read").count(),
        READ_POLLS as usize + 1
    );
}

#[test]
fn a_wrong_read_back_is_the_verdict() {
    let mut back = good();
    back.block = Some(256);
    let mut f = Fake {
        back: Some(back),
        ..Fake::default()
    };
    assert_eq!(
        run(&plan(None, true, false), &want(), &mut f, |_| {}),
        Err("Buffer: 128 applied, 256 read back".to_string())
    );
}

#[test]
fn complete_needs_every_value_and_the_interface_only_when_asked() {
    let w = want();
    assert!(complete(&w, &good()));
    for drop in 0..4 {
        let mut b = good();
        match drop {
            0 => b.block = None,
            1 => b.rx = None,
            2 => b.tx = None,
            _ => b.latency_ns = None,
        }
        assert!(!complete(&w, &b), "{drop}");
    }
    let mut iface = w.clone();
    iface.iface = Some("E".into());
    assert!(!complete(&iface, &good()));
    let mut b = good();
    b.iface = Some("E".into());
    assert!(complete(&iface, &b));
}

/// A bridge instance's status states its driver and no block (dasio's
/// `bridge_json`, dstack #292).
#[test]
fn a_bridge_is_complete_with_its_driver_and_no_block() {
    let mut w = want();
    w.bridge = Some(crate::dasio::Bridge {
        driver: "D".into(),
        to_asio: vec![1, 2],
        from_asio: vec![1, 2],
        app_buffer_ns: None,
    });
    let mut b = good();
    b.block = None;
    assert!(!complete(&w, &b));
    b.driver = Some("D".into());
    assert!(complete(&w, &b));
    b.rx = None;
    assert!(!complete(&w, &b));
    // The DAW kind still needs its block, a driver or not.
    let mut daw = good();
    daw.block = None;
    daw.driver = Some("D".into());
    assert!(!complete(&want(), &daw));
}

#[test]
fn an_unread_status_is_no_release() {
    // Two reads say nothing, then the DAW is gone: the install waits.
    let mut f = Fake {
        unknown: 2,
        ..Fake::default()
    };
    assert!(run(&plan(None, true, true), &want(), &mut f, |_| {}).is_ok());
    assert_eq!(
        f.log,
        ["daw", "wait", "daw", "wait", "daw", "install text", "read"]
    );
    // A status that never says writes nothing, and says so.
    let mut f = Fake {
        unknown: u32::MAX,
        ..Fake::default()
    };
    let err = run(&plan(Some(1), true, true), &want(), &mut f, |_| {}).expect_err("unknown");
    assert_eq!(
        err,
        "the device's status did not say for 10 minutes whether the DAW holds the driver; \
         the RX latency was set, the file was not written"
    );
    assert!(!f.log.iter().any(|l| l.starts_with("install")));
}

#[test]
fn a_restart_says_its_step_before_the_helper_and_none_without_one() {
    let mut said = Vec::new();
    let mut f = Fake::default();
    assert!(
        run(&plan(None, true, false), &want(), &mut f, |s| said
            .push(s.to_string()))
        .is_ok()
    );
    assert_eq!(said[0], RESTART_STEP);
    assert_eq!(said.len(), 2);
    let mut live = Vec::new();
    let mut f = Fake::default();
    assert!(
        run(&plan(Some(2_000_000), false, false), &want(), &mut f, |s| {
            live.push(s.to_string())
        })
        .is_ok()
    );
    assert!(!live.iter().any(|s| s == RESTART_STEP));
}
