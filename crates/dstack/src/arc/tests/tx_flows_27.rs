//! Our own 0x2200 read back by the ARC 2.7 reader (dstack #271,
//! [`flows::v27::list_tx_flows_27`]): every flow once, across pages and an
//! empty slot, with our writer's socket tag (`08 02` since v0.6.123, dstack #274).

use super::*;

use crate::arc::client::ReadError;
use crate::arc::control::Call;
use crate::arc::flows::v27::list_tx_flows_27;

/// Our device answering the reader's calls.
struct Ours(Dev);

impl Call for Ours {
    fn call(&mut self, op1: u16, body: &[u8]) -> Result<(u16, Vec<u8>), ReadError> {
        let r = self.0.ask(1, op1, body);
        Ok((r.header.op2, r.body))
    }
}

#[test]
fn our_paged_0x2200_with_an_empty_slot_reads_back_every_flow_once() {
    let mut dev = Dev::new(config());
    // Flows in slots 0 to 16 (ids 1 to 17), then slot 3 (id 4) freed: two
    // pages (the soft limit), the first passing over the empty slot.
    for port in 0..17u16 {
        let rx_at = SocketAddrV4::new(PEER_IP, 40_000 + port);
        dev.tx
            .add(&tx_request("p", "f", &[Some(1)], rx_at), 0)
            .unwrap();
    }
    let gone = flow_handle(3, dev.tx.flow(3).unwrap().cookie);
    dev.tx.stop(&gone).unwrap();
    let mut ours = Ours(dev);
    let read = list_tx_flows_27(&mut ours).unwrap();
    let ids: Vec<u16> = read.flows.iter().map(|f| f.id).collect();
    let want: Vec<u16> = (1..=17).filter(|&id| id != 4).collect();
    assert_eq!((ids, read.unread), (want, 0));
    let cfg = config();
    for f in &read.flows {
        assert_eq!(f.flags, 0x11, "unicast");
        assert_eq!(f.channels, vec![1]);
        assert_eq!(u32::from(f.bits), u32::from(cfg.encoding));
        assert_eq!(
            f.destinations,
            vec![SocketAddrV4::new(PEER_IP, 40_000 + f.id - 1)]
        );
    }
}
