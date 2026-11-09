// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Our own records answered from the device's own address (dstack #1
//! problem E). mdns-sd binds its sockets to 0.0.0.0:5353 and only names
//! the interface for a send, so on Windows its answers leave from the
//! interface's preferred source, never from a SkipAsSource address (the
//! extra address dasio and SongPlayer run on beside the PC's own); Dante
//! Controller read no such device, and read one on its own address in
//! full (dev1, 31.10.2026).
//!
//! - [`answer`] (pure): the reply to one query for the records we publish
//!   (PTR of a service type, SRV, TXT or ANY of an instance, A of our
//!   host), with the instance's SRV, TXT and A as additional records; a
//!   query from port 5353 is answered to the group, a legacy one (another
//!   port) to its sender.
//! - [`Responder`]: a socket bound to `<device address>:5353` that joined
//!   224.0.0.251 there, and a thread answering every query with the
//!   records the announcer published last ([`super::Announcer::published`]).
//!   Started on Windows only (`host`): Linux sends from the device's
//!   address already, and a socket bound to a unicast address receives no
//!   multicast there.
//!
//! mdns-sd keeps announcing, answering and browsing; Dante Controller then
//! gets our answers twice, once from the device's address. Logs carry
//! counts only.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use simple_dns::rdata::{A, PTR, RData, SRV, TXT};
use simple_dns::{CLASS, Name, Packet, QTYPE, ResourceRecord, TYPE};
use tracing::{debug, trace};

use super::ServiceRecord;

/// mDNS's group and port.
pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const PORT: u16 = 5353;
/// The TTL of a host's or an instance's own records (SRV, TXT, A) and of
/// a service type's PTR, as mdns-sd gives them.
const HOST_TTL: u32 = 120;
const OTHER_TTL: u32 = 4500;
/// The largest reply datagram: mdns-sd's own (`MAX_PKT_DEFAULT`), one
/// Ethernet frame with no IP fragments, as Audinate devices send their
/// answers (dstack #141).
pub const MAX_DATAGRAM: usize = 1452;
/// The thread's receive wait (it looks at the stop between two).
const WAIT: Duration = Duration::from_millis(250);

/// A name as compared: lower case, no dot at the end.
fn key(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// `name` as DNS carries it (Dante names hold spaces and an `@`).
fn dns(name: &str) -> Name<'_> {
    Name::new_unchecked(name.trim_end_matches('.'))
}

/// An instance's full name.
fn full(rec: &ServiceRecord) -> String {
    format!("{}.{}", rec.instance, rec.service)
}

/// Where a reply goes: the group for a query from port 5353, else its
/// sender (a legacy unicast query).
pub fn reply_to(from: SocketAddrV4) -> SocketAddrV4 {
    if from.port() == PORT {
        SocketAddrV4::new(GROUP, PORT)
    } else {
        from
    }
}

/// The reply to the query `bytes` for the records `recs`, its first
/// datagram; `None` when it asks for none of them (or is no query). See
/// [`answers`] for all of it.
pub fn answer(bytes: &[u8], recs: &[ServiceRecord]) -> Option<Vec<u8>> {
    answers(bytes, recs).into_iter().next()
}

/// One answer and the records that go along with it in its datagram.
struct Group<'a> {
    answers: Vec<ResourceRecord<'a>>,
    along: Vec<ResourceRecord<'a>>,
    /// Its records name our host: the datagram carries the host's A.
    needs_a: bool,
}

/// The reply to the query `bytes` for the records `recs` as datagrams of
/// at most [`MAX_DATAGRAM`] bytes; empty when it asks for none of them (or
/// is no query). Each PTR of a service type travels with its instance's
/// SRV and TXT, and every datagram whose records name our host carries its
/// A, so each datagram reads on its own. A single group too big for one
/// datagram is sent alone (RFC 6762 §17 allows it; ours never are).
pub fn answers(bytes: &[u8], recs: &[ServiceRecord]) -> Vec<Vec<u8>> {
    let Ok(query) = Packet::parse(bytes) else {
        return Vec::new();
    };
    if query.has_flags(simple_dns::PacketFlag::RESPONSE) {
        return Vec::new();
    }
    let Some(first) = recs.first() else {
        return Vec::new();
    };
    let fulls: Vec<String> = recs.iter().map(full).collect();
    let mut groups: Vec<Group> = Vec::new();
    let mut a_answered = false;
    for q in &query.questions {
        let asked = key(&q.qname.to_string());
        let t = match q.qtype {
            QTYPE::TYPE(t) => Some(t),
            QTYPE::ANY => None,
            _ => continue,
        };
        let wants = |want: TYPE| t.is_none_or(|t| t == want);
        for (i, rec) in recs.iter().enumerate() {
            if asked == key(rec.service) && wants(TYPE::PTR) {
                let Some(txt) = txt(rec, &fulls[i]) else {
                    return Vec::new();
                };
                groups.push(Group {
                    answers: vec![ResourceRecord::new(
                        dns(rec.service),
                        CLASS::IN,
                        OTHER_TTL,
                        RData::PTR(PTR(dns(&fulls[i]))),
                    )],
                    along: vec![srv(rec, &fulls[i]), txt],
                    needs_a: true,
                });
            }
            if asked == key(&fulls[i]) {
                let mut answers = Vec::new();
                let mut needs_a = false;
                if wants(TYPE::SRV) {
                    answers.push(srv(rec, &fulls[i]));
                    needs_a = true;
                }
                if wants(TYPE::TXT) {
                    let Some(txt) = txt(rec, &fulls[i]) else {
                        return Vec::new();
                    };
                    answers.push(txt);
                }
                if !answers.is_empty() {
                    groups.push(Group {
                        answers,
                        along: Vec::new(),
                        needs_a,
                    });
                }
            }
            if asked == key(&rec.host) && wants(TYPE::A) && !a_answered {
                groups.push(Group {
                    answers: vec![a(rec)],
                    along: Vec::new(),
                    needs_a: false,
                });
                a_answered = true;
            }
        }
    }
    pack(&groups, first, a_answered)
}

/// `groups` packed in order into datagrams of at most [`MAX_DATAGRAM`]
/// bytes, our host's A (`first`'s) added to each whose records name the
/// host unless the reply answers it already (`a_answered`).
fn pack(groups: &[Group], first: &ServiceRecord, a_answered: bool) -> Vec<Vec<u8>> {
    let build = |gs: &[&Group]| -> Option<Vec<u8>> {
        let mut reply = Packet::new_reply(0);
        for g in gs {
            reply.answers.extend(g.answers.iter().cloned());
            reply.additional_records.extend(g.along.iter().cloned());
        }
        if !a_answered && gs.iter().any(|g| g.needs_a) {
            reply.additional_records.push(a(first));
        }
        // Names compressed (RFC 1035 §4.1.4), as every mDNS sender does: a
        // channel's PTR, SRV and TXT repeat its long instance name.
        reply.build_bytes_vec_compressed().ok()
    };
    let mut out = Vec::new();
    let mut current: Vec<&Group> = Vec::new();
    let mut built: Option<Vec<u8>> = None;
    for g in groups {
        current.push(g);
        match build(&current) {
            Some(b) if b.len() <= MAX_DATAGRAM || current.len() == 1 => built = Some(b),
            _ => {
                // Too big with this group: the datagram so far goes, the
                // group starts the next one.
                current.pop();
                out.extend(built.take());
                current = vec![g];
                built = build(&current);
            }
        }
    }
    out.extend(built);
    out
}

fn srv<'a>(rec: &'a ServiceRecord, full: &'a str) -> ResourceRecord<'a> {
    let mut rr = ResourceRecord::new(
        dns(full),
        CLASS::IN,
        HOST_TTL,
        RData::SRV(SRV {
            priority: 0,
            weight: 0,
            port: rec.port,
            target: dns(&rec.host),
        }),
    );
    rr.cache_flush = true;
    rr
}

fn txt<'a>(rec: &'a ServiceRecord, full: &'a str) -> Option<ResourceRecord<'a>> {
    let mut txt = TXT::new();
    for s in &rec.txt {
        txt.add_string(s).ok()?;
    }
    let mut rr = ResourceRecord::new(dns(full), CLASS::IN, OTHER_TTL, RData::TXT(txt));
    rr.cache_flush = true;
    Some(rr)
}

fn a(rec: &ServiceRecord) -> ResourceRecord<'_> {
    let mut rr = ResourceRecord::new(
        dns(&rec.host),
        CLASS::IN,
        HOST_TTL,
        RData::A(A {
            address: u32::from(rec.ip),
        }),
    );
    rr.cache_flush = true;
    rr
}

/// Our records answered from the device's address (see the module).
#[derive(Debug)]
pub struct Responder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Responder {
    /// The socket on `ip`:5353, joined to the group there, and the thread
    /// answering from `published`.
    pub fn start(ip: Ipv4Addr, published: Arc<Mutex<Vec<ServiceRecord>>>) -> io::Result<Self> {
        Self::start_on(SocketAddrV4::new(ip, PORT), true, published)
    }

    /// [`Responder::start`] on `at`, joined to the group only with
    /// `multicast` (a test's unicast responder on loopback).
    pub fn start_on(
        at: SocketAddrV4,
        multicast: bool,
        published: Arc<Mutex<Vec<ServiceRecord>>>,
    ) -> io::Result<Self> {
        let sock = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        sock.set_reuse_address(true)?;
        sock.bind(&at.into())?;
        if multicast {
            sock.join_multicast_v4(&GROUP, at.ip())?;
            sock.set_multicast_if_v4(at.ip())?;
            sock.set_multicast_ttl_v4(255)?;
        }
        let sock: UdpSocket = sock.into();
        sock.set_read_timeout(Some(WAIT))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("dstack-mdns-own".to_string())
                .spawn(move || serve(&sock, &published, &stop))?
        };
        Ok(Responder {
            stop,
            thread: Some(thread),
        })
    }
}

impl Responder {
    /// Its stop was asked.
    #[cfg(test)]
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ended = t.join();
        }
    }
}

/// The thread: each query answered, until the stop.
fn serve(sock: &UdpSocket, published: &Mutex<Vec<ServiceRecord>>, stop: &AtomicBool) {
    let mut buf = vec![0u8; 9000];
    while !stop.load(Ordering::Relaxed) {
        let (n, from) = match sock.recv_from(&mut buf) {
            Ok((n, SocketAddr::V4(from))) => (n, from),
            Ok(_) => continue,
            Err(e) => {
                trace!(kind = ?e.kind(), "no mDNS query this wait");
                continue;
            }
        };
        let replies = {
            let recs = published.lock().unwrap_or_else(PoisonError::into_inner);
            answers(&buf[..n], &recs)
        };
        for r in replies {
            if let Err(e) = sock.send_to(&r, reply_to(from)) {
                debug!(%e, "mDNS answer from the device's address not sent");
            }
        }
    }
}

#[cfg(test)]
mod tests;
