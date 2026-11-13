//! A channel's record answered while the network resolver looks it up
//! (dstack #269): the lookup either finds the record or keeps the channel
//! from the ARC for its query's grace, never neither.

use super::*;

use crate::announce::responder::answers;
use crate::subscribe::instance::{host_query, query};
use crate::subscribe::resolve::instance;

/// Lookups of one channel, its record forgotten (a goodbye) after each one,
/// while another thread keeps answering it.
const ROUNDS: usize = 20_000;

/// The rack's channel record and its host's address, as its responder
/// answers the two direct queries.
fn rack_answers() -> Vec<Vec<u8>> {
    let mut rack = rec(
        CHAN_SERVICE,
        "TX 3@rack",
        "rack",
        Ipv4Addr::new(10, 0, 0, 12),
        4455,
    );
    rack.txt = [
        "id=3",
        "rate=96000",
        "enc=24",
        "dbcp1=0x1102",
        "fpp=32,2",
        "latency_ns=1000000",
        "nchan=8",
    ]
    .map(String::from)
    .to_vec();
    let mut out = answers(
        &query(1, &instance("rack", "TX 3")).unwrap(),
        &[rack.clone()],
    );
    out.extend(answers(&host_query(2, "rack.local.").unwrap(), &[rack]));
    assert_eq!(out.len(), 2, "the responder answers both");
    out
}

#[test]
fn a_record_answered_during_the_lookup_never_sends_the_channel_to_the_arc() {
    // The query thread runs (a channel is kept from the ARC only then); its
    // queries go to a socket that never answers.
    let silent = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    let std::net::SocketAddr::V4(sink) = silent.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let t = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut network = Network {
        instances: Instances::with_clock(Arc::clone(&t)),
        arc: ArcDirectory::default(),
    };
    assert!(network.instances.start_to(LOOPBACK, sink).unwrap());
    let datagrams = rack_answers();
    let network = &network;
    let stop = std::sync::atomic::AtomicBool::new(false);
    let mut found = 0;
    std::thread::scope(|s| {
        // The answers arrive at any moment, as the query thread's do.
        s.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                for d in &datagrams {
                    network.instances.answer(d);
                }
            }
        });
        for round in 0..ROUNDS {
            // Within the grace (the clock stays at 0): the record, or
            // nothing while its query may still answer.
            found += usize::from(network.lookup("rack", "TX 3").is_some());
            if network.arc.asked() {
                stop.store(true, Ordering::Relaxed);
                panic!("round {round}: the ARC was asked for a channel whose record was answering");
            }
            network.instances.forget("TX 3@rack", CHAN_SERVICE);
        }
        stop.store(true, Ordering::Relaxed);
    });
    // The answers did land during the lookups (the race was run).
    assert!(found > 0, "no lookup found the record");
}
