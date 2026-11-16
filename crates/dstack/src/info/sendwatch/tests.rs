use super::*;

/// WSAEADDRNOTAVAIL, as every send failed on dsg.
fn addr_not_avail() -> io::Error {
    io::Error::from_raw_os_error(10049)
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

#[test]
fn the_first_failure_is_reported_at_once_the_rest_once_a_minute_with_their_count() {
    let t0 = Instant::now();
    let mut w = SendWatch::default();
    let first = w.failed(t0, &addr_not_avail());
    let r = first.report.expect("the first failure at once");
    assert_eq!(r.failures, 1);
    assert_eq!(r.error, addr_not_avail().to_string());
    for k in 1..5 {
        assert_eq!(w.failed(t0 + secs(k), &addr_not_avail()).report, None);
    }
    assert_eq!(w.flush(t0 + secs(59)), None, "not before the minute");
    let r = w.flush(t0 + REPORT_EVERY).expect("due at the minute");
    assert_eq!(r.failures, 4, "the failures since the last report");
    assert_eq!(w.flush(t0 + secs(200)), None, "nothing left to report");
    // A failure past the next minute is reported at once again.
    let r = w
        .failed(t0 + REPORT_EVERY + REPORT_EVERY, &addr_not_avail())
        .report
        .expect("due again");
    assert_eq!(r.failures, 1);
}

#[test]
fn a_failure_within_the_minute_waits_for_the_flush() {
    let t0 = Instant::now();
    let mut w = SendWatch::default();
    w.failed(t0, &addr_not_avail());
    let other = io::Error::from(io::ErrorKind::PermissionDenied);
    assert_eq!(w.failed(t0 + secs(59), &other).report, None);
    let r = w.failed(t0 + secs(60), &addr_not_avail()).report.unwrap();
    assert_eq!(r.failures, 2, "both failures since the first report");
}

#[test]
fn the_last_error_is_the_one_reported() {
    let t0 = Instant::now();
    let mut w = SendWatch::default();
    w.failed(t0, &addr_not_avail());
    let other = io::Error::from(io::ErrorKind::PermissionDenied);
    w.failed(t0 + secs(1), &other);
    assert_eq!(w.flush(t0 + secs(60)).unwrap().error, other.to_string());
}

#[test]
fn failures_in_a_row_ask_a_reapply_then_a_reopen() {
    let t0 = Instant::now();
    let mut w = SendWatch::default();
    let asked: Vec<Option<Recover>> = (0..REOPEN_AT)
        .map(|k| {
            w.failed(t0 + Duration::from_millis(k.into()), &addr_not_avail())
                .recover
        })
        .collect();
    let mut want = vec![None; REOPEN_AT as usize];
    want[REAPPLY_AT as usize - 1] = Some(Recover::Reapply);
    want[REOPEN_AT as usize - 1] = Some(Recover::Reopen);
    assert_eq!(asked, want);
}

#[test]
fn the_recovery_period_doubles_with_each_reopen_up_to_an_hour() {
    let periods: Vec<u64> = [0, 1, 2, 5, 6, 7, 31, 32, 40]
        .iter()
        .map(|&n| recover_period(n).as_secs())
        .collect();
    assert_eq!(periods, [60, 120, 240, 1920, 3600, 3600, 3600, 3600, 3600]);
}

#[test]
fn each_recovery_waits_its_period_and_comes_again_while_failures_go_on() {
    let t0 = Instant::now();
    let mut w = SendWatch::default();
    for _ in 0..REOPEN_AT {
        w.failed(t0, &addr_not_avail());
    }
    // After the first reopen the period is two minutes: the run grows
    // again, nothing more is asked before it, a minute included.
    for k in 1..=119 {
        assert_eq!(
            w.failed(t0 + secs(k), &addr_not_avail()).recover,
            None,
            "failure {k} s after the reopen"
        );
    }
    let at = t0 + recover_period(1);
    assert_eq!(
        w.failed(at, &addr_not_avail()).recover,
        Some(Recover::Reopen),
        "at the period's end: the run is past REOPEN_AT"
    );
    // Two reopens: the period is four minutes for both kinds. The new
    // run's reapply waits until four minutes after the last one (t0).
    for _ in 0..REAPPLY_AT {
        assert_eq!(w.failed(at, &addr_not_avail()).recover, None);
    }
    assert_eq!(
        w.failed(t0 + secs(239), &addr_not_avail()).recover,
        None,
        "not before four minutes"
    );
    assert_eq!(
        w.failed(t0 + recover_period(2), &addr_not_avail()).recover,
        Some(Recover::Reapply)
    );
    // And the reopen four minutes after the last reopen.
    for _ in 0..REOPEN_AT {
        assert_eq!(w.failed(at + secs(239), &addr_not_avail()).recover, None);
    }
    assert_eq!(
        w.failed(at + recover_period(2), &addr_not_avail()).recover,
        Some(Recover::Reopen)
    );
}

#[test]
fn a_send_that_goes_out_starts_the_period_again() {
    let t0 = Instant::now();
    let mut w = SendWatch::default();
    for _ in 0..REOPEN_AT {
        w.failed(t0, &addr_not_avail());
    }
    assert!(w.sent().is_some());
    // A new outage a minute later: the minute period again, not two.
    let at = t0 + RECOVER_EVERY;
    let asked: Vec<Option<Recover>> = (0..REOPEN_AT)
        .map(|_| w.failed(at, &addr_not_avail()).recover)
        .collect();
    assert_eq!(asked[REAPPLY_AT as usize - 1], Some(Recover::Reapply));
    assert_eq!(asked[REOPEN_AT as usize - 1], Some(Recover::Reopen));
}

#[test]
fn a_send_that_goes_out_ends_the_run() {
    let t0 = Instant::now();
    let mut w = SendWatch::default();
    for _ in 1..REAPPLY_AT {
        w.failed(t0, &addr_not_avail());
    }
    assert_eq!(w.sent(), None, "no recovery asked: nothing to log");
    for _ in 1..REAPPLY_AT {
        assert_eq!(w.failed(t0, &addr_not_avail()).recover, None);
    }
    assert_eq!(
        w.failed(t0, &addr_not_avail()).recover,
        Some(Recover::Reapply),
        "the third in a row"
    );
    // The first outage's unreported failure waits for the minute's report,
    // with this outage's three.
    assert_eq!(w.flush(t0 + REPORT_EVERY).unwrap().failures, 4);
}

#[test]
fn a_send_that_goes_out_after_recoveries_names_the_outage() {
    let t0 = Instant::now();
    let mut w = SendWatch::default();
    // A reapply and a reopen in one outage.
    for _ in 0..REOPEN_AT + 2 {
        w.failed(t0, &addr_not_avail());
    }
    assert_eq!(w.sent(), Some(u64::from(REOPEN_AT + 2)));
    assert_eq!(w.sent(), None, "once");
    assert_eq!(
        w.flush(t0 + REPORT_EVERY),
        None,
        "the end of the outage took the failures not reported"
    );
    // An outage with one reapply.
    for _ in 0..REAPPLY_AT {
        w.failed(t0 + RECOVER_EVERY, &addr_not_avail());
    }
    assert_eq!(w.sent(), Some(u64::from(REAPPLY_AT)));
}

#[test]
fn the_index_form_is_an_address_in_0_slash_8() {
    assert_eq!(index_form(6), Some(Ipv4Addr::new(0, 0, 0, 6)));
    assert_eq!(index_form(0x0001_0203), Some(Ipv4Addr::new(0, 1, 2, 3)));
    assert_eq!(
        index_form((1 << 24) - 1),
        Some(Ipv4Addr::new(0, 255, 255, 255))
    );
    assert_eq!(index_form(1 << 24), None);
}

const IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 10);

fn links_with(index: Option<u32>) -> io::Result<Vec<Link>> {
    Ok(vec![
        Link {
            name: "other".into(),
            ip: Ipv4Addr::new(169, 254, 1, 2),
            index: Some(14),
        },
        Link {
            name: "dante".into(),
            ip: IP,
            index,
        },
    ])
}

fn no_links() -> io::Result<Vec<Link>> {
    panic!("the interface list read with no index form asked")
}

#[test]
fn the_address_names_the_interface_when_it_can() {
    let mut set = Vec::new();
    let named = interface_by_index(
        IP,
        true,
        |a| {
            set.push(a);
            Ok(())
        },
        no_links,
    );
    assert_eq!(named.unwrap(), Named::ByAddress);
    assert_eq!(set, [IP]);
}

#[test]
fn off_windows_a_refused_address_is_the_error() {
    let named = interface_by_index(IP, false, |_| Err(addr_not_avail()), no_links);
    assert_eq!(named.unwrap_err().raw_os_error(), Some(10049));
}

#[test]
fn on_windows_a_refused_address_falls_back_to_its_interface_s_index_now() {
    let mut set = Vec::new();
    let named = interface_by_index(
        IP,
        true,
        |a| {
            set.push(a);
            if a == IP {
                Err(addr_not_avail())
            } else {
                Ok(())
            }
        },
        || links_with(Some(6)),
    );
    assert_eq!(named.unwrap(), Named::ByIndex(6));
    assert_eq!(set, [IP, Ipv4Addr::new(0, 0, 0, 6)]);
}

#[test]
fn no_index_for_the_address_leaves_the_address_s_error() {
    let refused = |_| Err(addr_not_avail());
    let no_index = interface_by_index(IP, true, refused, || links_with(None));
    assert_eq!(no_index.unwrap_err().raw_os_error(), Some(10049));
    let gone = interface_by_index(IP, true, refused, || {
        links_with(Some(6)).map(|l| l.into_iter().take(1).collect())
    });
    assert_eq!(gone.unwrap_err().raw_os_error(), Some(10049));
}

#[test]
fn a_failed_read_or_index_is_its_own_error() {
    let refused = |_| Err(addr_not_avail());
    let unread = interface_by_index(IP, true, refused, || {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    });
    assert_eq!(unread.unwrap_err().kind(), io::ErrorKind::Unsupported);
    let past = interface_by_index(IP, true, refused, || links_with(Some(1 << 24)));
    assert_eq!(past.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    let by_index_refused = interface_by_index(
        IP,
        true,
        |a| {
            if a == IP {
                Err(addr_not_avail())
            } else {
                Err(io::Error::from(io::ErrorKind::AddrInUse))
            }
        },
        || links_with(Some(6)),
    );
    assert_eq!(
        by_index_refused.unwrap_err().kind(),
        io::ErrorKind::AddrInUse
    );
}
