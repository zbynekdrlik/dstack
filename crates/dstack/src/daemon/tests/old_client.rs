// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #323: the serving daemon names a local client below the network
//! floor on its ARC record, announcing again once per change and never
//! otherwise.

use super::*;

use crate::announce::{ARC_SERVICE, OLD_CLIENT_KEY};
use crate::network_floor::Version;

/// The announced ARC record's `dstack_old_client` entry, if any.
fn announced(published: &Mutex<Vec<ServiceRecord>>) -> Option<String> {
    let recs = published.lock().unwrap();
    let arc = recs.iter().find(|r| r.service == ARC_SERVICE)?;
    arc.txt
        .iter()
        .find(|t| t.starts_with(&format!("{OLD_CLIENT_KEY}=")))
        .cloned()
}

/// How many times the ARC record was registered.
fn arc_registrations(log: &Log) -> usize {
    log.calls()
        .iter()
        .filter(|c| c.starts_with("+test-device._netaudio-arc."))
        .count()
}

async fn until(what: &str, f: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + BOUND;
    while !f() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} never happened"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn an_old_local_client_is_announced_once_and_taken_off_once_upgraded() {
    let dir = tempfile::tempdir().unwrap();
    let shared = device_in(dir.path());
    let log = Log::new(usize::MAX);
    let mut announcer = Announcer::new(log.clone());
    let published = announcer.published();
    let lo = loopback().await;
    let media = media_of(&shared);
    let asker = Arc::clone(&media);
    let watch = log.clone();
    let seen = Arc::clone(&published);
    let control = async move {
        // At start: no old client, no key, one ARC registration.
        until("the first announcement", || arc_registrations(&watch) == 1).await;
        tokio::time::sleep(TICK * 2).await;
        assert_eq!(announced(&seen), None);
        assert_eq!(
            arc_registrations(&watch),
            1,
            "nothing while nothing changed"
        );
        // A dtray of 0.6.20 asks: the key comes, in one announcement.
        asker.client_asked("dtray", Version::new(0, 6, 20));
        until("the key announced", || announced(&seen).is_some()).await;
        assert_eq!(
            announced(&seen).as_deref(),
            Some("dstack_old_client=0.6.20")
        );
        // A current client beside it changes nothing.
        asker.client_asked("dtray-cli", Version::new(0, 6, 146));
        tokio::time::sleep(TICK * 4).await;
        assert_eq!(arc_registrations(&watch), 2, "once per change");
        // Upgraded under its name: the key goes, in one announcement.
        asker.client_asked("dtray", Version::new(0, 6, 146));
        until("the key withdrawn", || announced(&seen).is_none()).await;
        tokio::time::sleep(TICK * 4).await;
        assert_eq!(arc_registrations(&watch), 3, "once per change");
        Err(io::Error::other("the test's end"))
    };
    let served = tokio::time::timeout(
        BOUND * 2,
        serve(
            lo.sockets,
            shared,
            &mut announcer,
            net(),
            lo.multicast,
            Arc::clone(&media),
            control,
            std::future::pending(),
        ),
    )
    .await
    .expect("within the bound");
    assert!(matches!(served, Err(ServeError::Control(_))), "{served:?}");
    // Only the ARC record changed: the others were registered once.
    let calls = log.calls();
    assert_eq!(calls.len(), 3 + 16 + 2, "{calls:?}");
    assert!(calls.iter().all(|c| !c.starts_with('-')), "{calls:?}");
}
