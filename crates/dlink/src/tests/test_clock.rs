//! dstack #305: the TEST clock skew an app's CI sets
//! (`Config::test_clock_ppm`): it rides in the device config and starts the
//! device on the TEST system clock; any other clock refuses it.

use super::start::{ABSENT, Quiet, own_state, rx_only};
use super::*;

#[test]
fn a_test_clock_skew_selects_the_system_clock() {
    let (_dir, cfg) = own_state();
    assert_eq!(cfg.clock(ClockSource::Linked), ClockSource::Linked);
    assert_eq!(cfg.clock(ClockSource::Dsync), ClockSource::Dsync);
    let skewed = cfg.test_clock_ppm(-6.0);
    assert_eq!(skewed.clock(ClockSource::Linked), ClockSource::System);
    assert_eq!(skewed.clock(ClockSource::Dsync), ClockSource::System);
}

#[test]
fn the_skew_rides_in_the_device_config_and_is_checked() {
    let (_dir, cfg) = own_state();
    assert_eq!(cfg.device().unwrap().test_clock_ppm, None);
    let device = cfg.clone().test_clock_ppm(6.5).device().unwrap();
    assert_eq!(device.test_clock_ppm, Some(TestClockPpm(6.5)));
    let problems = cfg.test_clock_ppm(1500.0).device().unwrap_err();
    assert_eq!(problems, ["test_clock_ppm: a number from -1000 to 1000"]);
}

/// `Link::start` with a skew starts on the system clock: past the clock's
/// check, to the place (not there: a run error); `Link::start_on` with the
/// linked clock refuses the skew before the place (a setup error).
#[test]
fn link_start_takes_the_skew_on_the_system_clock_and_start_on_refuses_another() {
    let (_dir, cfg) = own_state();
    let cfg = cfg.test_clock_ppm(6.0);
    match Link::start(&cfg, &Place::Ip(ABSENT), rx_only(), Quiet) {
        Err(HostError::Run(why)) => {
            assert_eq!(why, "no interface of this box holds the address");
        }
        other => panic!("{:?}", other.map(drop)),
    }
    match Link::start_on(&cfg, &Place::Ip(ABSENT), rx_only(), Quiet, Clock::Linked) {
        Err(HostError::Setup(why)) => {
            assert!(
                why.starts_with("test_clock_ppm 6 is for the TEST system clock"),
                "{why}"
            );
        }
        other => panic!("{:?}", other.map(drop)),
    }
}
