# Autopilot log

One line per ticket: issue, commits, tests (RED -> GREEN where a bug), decisions, PR.

- #234 item 1 (v0.6.101, PR 235): RX kernel receive stamps (SO_TIMESTAMPNS + recvmsg) split wire lateness from RX-thread wake; new `wire_late`, `wake_delay_us`, `wake_delay_peak_us` (status, Linux only; Windows absent). Commits 363a1ac (bump), 7921ab1 (feature + tests), a413052 (review fixes: pure `wire_late`, whole-cmsg check, `stamps` test switch), 4381d59 (tests rule). Tests: `media::rx::socket::tests::*` (loopback 30 ms sit -> late, not wire_late; 1 s late -> wire_late; on-time -> nothing; multicast stamped; stamps off -> as Windows), `ctl::status::tests::the_receive_stamp_keys_show_only_while_the_sockets_stamp`. Decision: software stamps over NIC hardware stamps; issue stays open for the field read.
