// SPDX-License-Identifier: MIT OR Apache-2.0

use std::net::{Ipv4Addr, SocketAddrV4};

use dstack::arc::control::Settings;

use super::{Counts, Known, SETTINGS_EVERY, Shape, needs_rebuild, shapes_of, write_needed};
use crate::discover::Local;
use crate::latency::Choice;

// Documentation addresses (RFC 5737) only.
const A: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 10), 4440);
const B: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 10), 4441);

fn settings(configured: u32) -> Settings {
    Settings {
        latency_min_ns: Some(1_000_000),
        latency_max_ns: Some(2_000_000),
        latency_configured_ns: Some(configured),
        ..Settings::default()
    }
}

fn local(name: &str, arc: SocketAddrV4) -> Local {
    Local {
        name: name.to_string(),
        port: 5000,
        arc,
    }
}

fn choice(ns: u32, checked: bool) -> Choice {
    Choice {
        ns,
        label: crate::latency::label(ns),
        checked,
    }
}

#[test]
fn a_new_device_is_read_at_once_then_every_settings_every_ticks() {
    let mut k = Known::default();
    assert_eq!(k.due(&[A, B], 5), vec![A, B]);
    k.record(A, Some(settings(1_000_000)), 5);
    assert_eq!(k.due(&[A, B], 5), vec![B]);
    assert_eq!(
        k.due(&[A], 5 + SETTINGS_EVERY - 1),
        Vec::<SocketAddrV4>::new()
    );
    assert_eq!(k.due(&[A], 5 + SETTINGS_EVERY), vec![A]);
    // Ticks wrap.
    k.record(B, None, u32::MAX);
    assert!(k.due(&[B], SETTINGS_EVERY - 2).is_empty());
    assert_eq!(k.due(&[B], SETTINGS_EVERY - 1), vec![B]);
}

#[test]
fn a_failed_read_keeps_the_settings_and_waits_its_turn() {
    let mut k = Known::default();
    k.record(A, Some(settings(1_000_000)), 0);
    k.record(A, None, SETTINGS_EVERY);
    assert_eq!(k.get(&A), Some(&settings(1_000_000)));
    assert!(
        k.due(&[A], SETTINGS_EVERY + 1).is_empty(),
        "not again next tick"
    );
    // A device never answered: no settings, but tried.
    k.record(B, None, 3);
    assert_eq!(k.get(&B), None);
    assert!(k.due(&[B], 4).is_empty());
    // A later answer replaces them.
    k.record(A, Some(settings(2_000_000)), 30);
    assert_eq!(k.get(&A), Some(&settings(2_000_000)));
}

#[test]
fn gone_devices_are_forgotten() {
    let mut k = Known::default();
    k.record(A, Some(settings(1_000_000)), 0);
    k.record(B, Some(settings(1_000_000)), 0);
    k.forget_gone(&[B]);
    assert_eq!(k.get(&A), None);
    assert!(k.get(&B).is_some());
    assert_eq!(k.due(&[A, B], 1), vec![A]);
}

#[test]
fn shapes_carry_each_device_s_latency_menu() {
    let mut k = Known::default();
    k.record(A, Some(settings(2_000_000)), 0);
    let shapes = shapes_of(&[local("a", A), local("b", B)], &k);
    assert_eq!(
        shapes,
        vec![
            Shape {
                arc: A,
                choices: vec![choice(1_000_000, false), choice(2_000_000, true)],
            },
            Shape {
                arc: B,
                choices: vec![],
            },
        ]
    );
    assert!(shapes_of(&[], &k).is_empty());
}

#[test]
fn the_menu_is_built_again_only_when_its_shape_changes() {
    let one = vec![Shape {
        arc: A,
        choices: vec![choice(1_000_000, true)],
    }];
    let built = Counts {
        devices: 1,
        missing: 1,
    };
    assert!(!needs_rebuild(false, built, &one, &one, 1, 2));
    assert!(needs_rebuild(true, built, &one, &one, 1, 2), "forced");
    let moved = vec![Shape {
        arc: A,
        choices: vec![choice(1_000_000, false)],
    }];
    assert!(
        needs_rebuild(false, built, &one, &moved, 1, 2),
        "a check moved"
    );
    assert!(
        needs_rebuild(false, built, &one, &one, 1, 1),
        "a missing line gone"
    );
    assert!(
        needs_rebuild(false, built, &one, &one, 1, 3),
        "a missing line more"
    );
    assert!(
        needs_rebuild(false, built, &one, &one, 2, 2),
        "a device more"
    );
    assert!(
        needs_rebuild(false, built, &one, &one, 0, 2),
        "a device fewer"
    );
}

#[test]
fn choosing_the_checked_value_writes_nothing() {
    let shapes = vec![
        Shape {
            arc: A,
            choices: vec![choice(1_000_000, false), choice(2_000_000, true)],
        },
        Shape {
            arc: B,
            choices: vec![choice(1_000_000, true)],
        },
    ];
    assert!(!write_needed(&shapes, A, 2_000_000));
    assert!(write_needed(&shapes, A, 1_000_000));
    assert!(!write_needed(&shapes, B, 1_000_000));
    assert!(write_needed(&shapes, B, 2_000_000));
    assert!(write_needed(&[], A, 1_000_000));
}
