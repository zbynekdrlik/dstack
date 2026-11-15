// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The local endpoint's status request with its client (dstack #321).

use super::*;

/// dstack #321: a status request may state its client's name and dstack
/// version, and nothing else past `status`.
#[test]
fn a_status_request_states_its_client_or_nothing() {
    use crate::network_floor::Version;
    assert_eq!(status_request("status"), Some(Ok(None)));
    assert_eq!(status_request(" status \r"), Some(Ok(None)));
    assert_eq!(
        status_request("status dtray 0.6.144"),
        Some(Ok(Some(("dtray".to_string(), Version::new(0, 6, 144)))))
    );
    for bad in [
        "status dtray",
        "status dtray 0.6.144 more",
        "status dtray latest",
        "status d\"tray 0.6.144",
    ] {
        assert_eq!(status_request(bad), Some(Err(BAD_CLIENT)), "{bad:?}");
    }
    assert_eq!(status_request("statusx"), None);
    assert_eq!(status_request("app status"), None);
    assert_eq!(status_request(""), None);
}

/// dstack #321: the local endpoint lists the clients that state
/// themselves, warns once about one below the network floor, and refuses
/// a request with a client it cannot read (listing nothing).
#[test]
fn the_local_endpoint_lists_its_clients_and_warns_about_an_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let media = media_on(&device_at(dir.path()), Arc::new(Unready));
    assert!(!local::reply("status", &media).contains("network"));
    assert_eq!(
        local::reply("status dtray latest", &media),
        format!("error: {BAD_CLIENT}")
    );
    assert!(media.clients().is_empty());
    let s = local::reply("status dtray-cli 0.6.144", &media);
    assert!(s.starts_with("{\"clock\":{"), "{s}");
    assert!(
        s.contains(
            "\"network\":{\"floor\":\"0.6.58\",\"below_floor\":[],\
             \"clients\":[{\"name\":\"dtray-cli\",\"version\":\"0.6.144\"}]}"
        ),
        "{s}"
    );
    let text = crate::test_log::capture_warnings(|| {
        for _ in 0..3 {
            local::reply("status ctl-test-old 0.6.20", &media);
        }
    });
    assert_eq!(
        text.matches("a dstack device below the network floor")
            .count(),
        1,
        "{text}"
    );
    assert!(text.contains("ctl-test-old"), "{text}");
    let s = local::reply("status", &media);
    assert!(
        s.contains(
            "\"below_floor\":[{\"kind\":\"client\",\"name\":\"ctl-test-old\",\
             \"version\":\"0.6.20\"}]"
        ),
        "{s}"
    );
    assert_eq!(media.clients().len(), 2);
}
