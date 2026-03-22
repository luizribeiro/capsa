use smoltcp::wire::{EthernetFrame, EthernetProtocol, Ipv4Packet};

#[allow(clippy::type_complexity)]
pub(crate) fn parse_ipv4_frame(frame: &[u8]) -> Option<(EthernetFrame<&[u8]>, Ipv4Packet<&[u8]>)> {
    let eth = EthernetFrame::new_checked(frame).ok()?;
    if eth.ethertype() != EthernetProtocol::Ipv4 {
        return None;
    }
    let ip = Ipv4Packet::new_checked(eth.payload()).ok()?;
    Some((eth, ip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::phy::ChecksumCapabilities;
    use smoltcp::wire::{EthernetAddress, EthernetRepr, IpProtocol, Ipv4Repr};

    fn make_ipv4_frame() -> Vec<u8> {
        let ip_payload_len = 0;
        let ip_len = 20 + ip_payload_len;
        let total_len = 14 + ip_len;
        let mut frame = vec![0u8; total_len];

        let eth_repr = EthernetRepr {
            src_addr: EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]),
            dst_addr: EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]),
            ethertype: EthernetProtocol::Ipv4,
        };
        let mut eth_frame = EthernetFrame::new_unchecked(&mut frame[..]);
        eth_repr.emit(&mut eth_frame);

        let ip_repr = Ipv4Repr {
            src_addr: smoltcp::wire::Ipv4Address::new(10, 0, 2, 15),
            dst_addr: smoltcp::wire::Ipv4Address::new(10, 0, 2, 1),
            next_header: IpProtocol::Udp,
            payload_len: ip_payload_len,
            hop_limit: 64,
        };
        let mut ip_packet = Ipv4Packet::new_unchecked(&mut frame[14..]);
        ip_repr.emit(&mut ip_packet, &ChecksumCapabilities::default());

        frame
    }

    #[test]
    fn valid_ipv4_frame_returns_some() {
        let frame = make_ipv4_frame();
        let result = parse_ipv4_frame(&frame);
        assert!(result.is_some());
        let (eth, ip) = result.unwrap();
        assert_eq!(eth.ethertype(), EthernetProtocol::Ipv4);
        assert_eq!(ip.src_addr(), smoltcp::wire::Ipv4Address::new(10, 0, 2, 15));
    }

    #[test]
    fn arp_frame_returns_none() {
        let mut frame = vec![0u8; 42]; // minimum ARP frame size
        let eth_repr = EthernetRepr {
            src_addr: EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]),
            dst_addr: EthernetAddress([0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
            ethertype: EthernetProtocol::Arp,
        };
        let mut eth_frame = EthernetFrame::new_unchecked(&mut frame[..]);
        eth_repr.emit(&mut eth_frame);

        assert!(parse_ipv4_frame(&frame).is_none());
    }

    #[test]
    fn truncated_frame_returns_none() {
        assert!(parse_ipv4_frame(&[]).is_none());
        assert!(parse_ipv4_frame(&[0u8; 5]).is_none());
        // Valid ethernet header but truncated IP
        let frame = make_ipv4_frame();
        assert!(parse_ipv4_frame(&frame[..16]).is_none());
    }
}
