use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, EthernetRepr, Icmpv4Packet, Icmpv4Repr,
    IpProtocol, Ipv4Packet, Ipv4Repr, TcpControl, TcpPacket, TcpRepr, TcpSeqNumber, UdpPacket,
    UdpRepr,
};
use std::net::{Ipv4Addr, SocketAddrV4};

use crate::config::{
    DEFAULT_HOP_LIMIT, ETHERNET_HEADER_LEN, ICMP_HEADER_LEN, IP_HEADER_SIZE, TCP_HEADER_SIZE,
    UDP_HEADER_LEN,
};

pub(crate) fn craft_udp_response(
    payload: &[u8],
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Vec<u8> {
    let udp_len = UDP_HEADER_LEN + payload.len();
    let ip_len = IP_HEADER_SIZE + udp_len;
    let total_len = ETHERNET_HEADER_LEN + ip_len;

    let mut frame = vec![0u8; total_len];

    let eth_repr = EthernetRepr {
        src_addr: gateway_mac,
        dst_addr: guest_mac,
        ethertype: EthernetProtocol::Ipv4,
    };
    let mut eth_frame = EthernetFrame::new_unchecked(&mut frame[..]);
    eth_repr.emit(&mut eth_frame);

    let ip_repr = Ipv4Repr {
        src_addr: *src_addr.ip(),
        dst_addr: *dst_addr.ip(),
        next_header: IpProtocol::Udp,
        payload_len: udp_len,
        hop_limit: DEFAULT_HOP_LIMIT,
    };

    let mut ip_packet = Ipv4Packet::new_unchecked(&mut frame[ETHERNET_HEADER_LEN..]);
    let checksum_caps = ChecksumCapabilities::default();
    ip_repr.emit(&mut ip_packet, &checksum_caps);

    let udp_repr = UdpRepr {
        src_port: src_addr.port(),
        dst_port: dst_addr.port(),
    };

    let mut udp_packet =
        UdpPacket::new_unchecked(&mut frame[ETHERNET_HEADER_LEN + IP_HEADER_SIZE..]);
    udp_repr.emit(
        &mut udp_packet,
        &ip_repr.src_addr.into(),
        &ip_repr.dst_addr.into(),
        payload.len(),
        |buf| buf.copy_from_slice(payload),
        &checksum_caps,
    );

    frame
}

/// Craft a TCP RST frame to send back to guest.
pub(crate) fn craft_tcp_rst(
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    seq_num: u32,
    ack_num: u32,
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Vec<u8> {
    let tcp_len = TCP_HEADER_SIZE;
    let ip_len = IP_HEADER_SIZE + tcp_len;
    let total_len = ETHERNET_HEADER_LEN + ip_len;

    let mut frame = vec![0u8; total_len];

    let eth_repr = EthernetRepr {
        src_addr: gateway_mac,
        dst_addr: guest_mac,
        ethertype: EthernetProtocol::Ipv4,
    };
    let mut eth_frame = EthernetFrame::new_unchecked(&mut frame[..]);
    eth_repr.emit(&mut eth_frame);

    let ip_repr = Ipv4Repr {
        src_addr: *src_addr.ip(),
        dst_addr: *dst_addr.ip(),
        next_header: IpProtocol::Tcp,
        payload_len: tcp_len,
        hop_limit: DEFAULT_HOP_LIMIT,
    };

    let checksum_caps = ChecksumCapabilities::default();
    let mut ip_packet = Ipv4Packet::new_unchecked(&mut frame[ETHERNET_HEADER_LEN..]);
    ip_repr.emit(&mut ip_packet, &checksum_caps);

    let tcp_repr = TcpRepr {
        src_port: src_addr.port(),
        dst_port: dst_addr.port(),
        seq_number: TcpSeqNumber(seq_num as i32),
        ack_number: Some(TcpSeqNumber(ack_num as i32)),
        window_len: 0,
        window_scale: None,
        control: TcpControl::Rst,
        max_seg_size: None,
        sack_permitted: false,
        sack_ranges: [None, None, None],
        timestamp: None,
        payload: &[],
    };

    let mut tcp_packet =
        TcpPacket::new_unchecked(&mut frame[ETHERNET_HEADER_LEN + IP_HEADER_SIZE..]);
    tcp_repr.emit(
        &mut tcp_packet,
        &ip_repr.src_addr.into(),
        &ip_repr.dst_addr.into(),
        &checksum_caps,
    );

    frame
}

/// Craft an ICMP echo reply ethernet frame to send back to guest.
pub(crate) fn craft_icmp_echo_reply(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    identifier: u16,
    icmp_data: &[u8],
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    if icmp_data.len() < ICMP_HEADER_LEN {
        return None;
    }

    let msg_type = icmp_data[0];
    if msg_type != 0 {
        return None;
    }

    let sequence = u16::from_be_bytes([icmp_data[6], icmp_data[7]]);
    let payload = &icmp_data[ICMP_HEADER_LEN..];

    let icmp_repr = Icmpv4Repr::EchoReply {
        ident: identifier,
        seq_no: sequence,
        data: payload,
    };

    let icmp_len = icmp_repr.buffer_len();
    let ip_len = IP_HEADER_SIZE + icmp_len;
    let total_len = ETHERNET_HEADER_LEN + ip_len;

    let mut frame = vec![0u8; total_len];

    let eth_repr = EthernetRepr {
        src_addr: gateway_mac,
        dst_addr: guest_mac,
        ethertype: EthernetProtocol::Ipv4,
    };
    let mut eth_frame = EthernetFrame::new_unchecked(&mut frame[..]);
    eth_repr.emit(&mut eth_frame);

    let ip_repr = Ipv4Repr {
        src_addr: src_ip,
        dst_addr: dst_ip,
        next_header: IpProtocol::Icmp,
        payload_len: icmp_len,
        hop_limit: DEFAULT_HOP_LIMIT,
    };

    let checksum_caps = ChecksumCapabilities::default();
    let mut ip_packet = Ipv4Packet::new_unchecked(&mut frame[ETHERNET_HEADER_LEN..]);
    ip_repr.emit(&mut ip_packet, &checksum_caps);

    let icmp_start = ETHERNET_HEADER_LEN + IP_HEADER_SIZE;
    let mut icmp_packet = Icmpv4Packet::new_unchecked(&mut frame[icmp_start..]);
    icmp_repr.emit(&mut icmp_packet, &checksum_caps);

    Some(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::wire::Icmpv4Message;

    #[test]
    fn test_craft_udp_response() {
        let payload = b"hello";
        let src = SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 53);
        let dst = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 15), 12345);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        let frame = craft_udp_response(payload, src, dst, gateway_mac, guest_mac);

        let eth = EthernetFrame::new_checked(&frame).unwrap();
        assert_eq!(eth.src_addr(), gateway_mac);
        assert_eq!(eth.dst_addr(), guest_mac);
        assert_eq!(eth.ethertype(), EthernetProtocol::Ipv4);

        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert_eq!(ip.src_addr(), *src.ip());
        assert_eq!(ip.dst_addr(), *dst.ip());

        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        assert_eq!(udp.src_port(), src.port());
        assert_eq!(udp.dst_port(), dst.port());
        assert_eq!(udp.payload(), payload);
    }

    #[test]
    fn test_craft_icmp_echo_reply() {
        let src_ip = Ipv4Addr::new(8, 8, 8, 8);
        let dst_ip = Ipv4Addr::new(10, 0, 2, 15);
        let identifier = 1234;
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        let icmp_data = vec![
            0, 0, // Type = echo reply, Code = 0
            0x00, 0x00, // Checksum (ignored for test)
            0x04, 0xD2, // Identifier = 1234
            0x00, 0x01, // Sequence = 1
            b'p', b'i', b'n', b'g', // Payload
        ];

        let frame = craft_icmp_echo_reply(
            src_ip,
            dst_ip,
            identifier,
            &icmp_data,
            gateway_mac,
            guest_mac,
        );

        assert!(frame.is_some());
        let frame = frame.unwrap();

        let eth = EthernetFrame::new_checked(&frame).unwrap();
        assert_eq!(eth.src_addr(), gateway_mac);
        assert_eq!(eth.dst_addr(), guest_mac);
        assert_eq!(eth.ethertype(), EthernetProtocol::Ipv4);

        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert_eq!(ip.src_addr(), src_ip);
        assert_eq!(ip.dst_addr(), dst_ip);
        assert_eq!(ip.next_header(), IpProtocol::Icmp);

        let icmp = Icmpv4Packet::new_checked(ip.payload()).unwrap();
        assert_eq!(icmp.msg_type(), Icmpv4Message::EchoReply);
        assert_eq!(icmp.msg_code(), 0);

        assert_eq!(icmp.echo_ident(), identifier);
        assert_eq!(icmp.echo_seq_no(), 1);
        assert_eq!(icmp.data(), b"ping");
    }

    #[test]
    fn test_craft_icmp_echo_reply_rejects_non_reply() {
        let src_ip = Ipv4Addr::new(8, 8, 8, 8);
        let dst_ip = Ipv4Addr::new(10, 0, 2, 15);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        let icmp_data = vec![
            8, 0, // Type = echo request (not reply)
            0x00, 0x00, 0x04, 0xD2, 0x00, 0x01,
        ];

        let frame = craft_icmp_echo_reply(src_ip, dst_ip, 1234, &icmp_data, gateway_mac, guest_mac);
        assert!(frame.is_none());
    }

    #[test]
    fn test_craft_icmp_echo_reply_rejects_short_data() {
        let src_ip = Ipv4Addr::new(8, 8, 8, 8);
        let dst_ip = Ipv4Addr::new(10, 0, 2, 15);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        let icmp_data = vec![0, 0, 0, 0];

        let frame = craft_icmp_echo_reply(src_ip, dst_ip, 1234, &icmp_data, gateway_mac, guest_mac);
        assert!(frame.is_none());
    }

    #[test]
    fn test_smoltcp_icmp_echo_parsing() {
        let payload = b"test";
        let repr = Icmpv4Repr::EchoRequest {
            ident: 0x1234,
            seq_no: 5,
            data: payload,
        };
        let mut icmp_bytes = vec![0u8; repr.buffer_len()];
        let mut pkt = Icmpv4Packet::new_unchecked(&mut icmp_bytes);
        repr.emit(&mut pkt, &ChecksumCapabilities::default());

        let icmp = Icmpv4Packet::new_checked(&icmp_bytes).expect("Failed to parse ICMP");

        assert_eq!(icmp.msg_type(), Icmpv4Message::EchoRequest);
        assert_eq!(icmp.echo_ident(), 0x1234);
        assert_eq!(icmp.echo_seq_no(), 5);

        assert_eq!(icmp.data(), b"test");
        assert_eq!(icmp.data().len(), 4);
    }

    #[test]
    fn test_craft_tcp_rst_produces_valid_rst() {
        let src = SocketAddrV4::new(Ipv4Addr::new(93, 184, 216, 34), 80);
        let dst = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 15), 45678);
        let seq_num: u32 = 0;
        let ack_num: u32 = 12346;
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        let frame = craft_tcp_rst(src, dst, seq_num, ack_num, gateway_mac, guest_mac);

        let eth = EthernetFrame::new_checked(&frame).unwrap();
        assert_eq!(eth.src_addr(), gateway_mac);
        assert_eq!(eth.dst_addr(), guest_mac);
        assert_eq!(eth.ethertype(), EthernetProtocol::Ipv4);

        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert_eq!(ip.src_addr(), *src.ip());
        assert_eq!(ip.dst_addr(), *dst.ip());
        assert_eq!(ip.next_header(), IpProtocol::Tcp);

        let tcp = TcpPacket::new_checked(ip.payload()).unwrap();
        assert_eq!(tcp.src_port(), src.port());
        assert_eq!(tcp.dst_port(), dst.port());
        assert!(tcp.rst());
        assert_eq!(tcp.seq_number(), TcpSeqNumber(seq_num as i32));
        assert_eq!(tcp.ack_number(), TcpSeqNumber(ack_num as i32));
        assert_eq!(tcp.window_len(), 0);
        assert!(tcp.payload().is_empty());
    }

    #[test]
    fn test_icmp_request_parsing_from_ip_packet() {
        let identifier: u16 = 0xABCD;
        let sequence: u16 = 42;
        let payload = b"hello world!";

        let repr = Icmpv4Repr::EchoRequest {
            ident: identifier,
            seq_no: sequence,
            data: payload,
        };
        let mut icmp_bytes = vec![0u8; repr.buffer_len()];
        let mut pkt = Icmpv4Packet::new_unchecked(&mut icmp_bytes);
        repr.emit(&mut pkt, &ChecksumCapabilities::default());

        let src_ip = Ipv4Addr::new(10, 0, 2, 15);
        let dst_ip = Ipv4Addr::new(8, 8, 8, 8);
        let ip_repr = Ipv4Repr {
            src_addr: src_ip,
            dst_addr: dst_ip,
            next_header: IpProtocol::Icmp,
            payload_len: icmp_bytes.len(),
            hop_limit: 64,
        };

        let mut ip_bytes = vec![0u8; 20 + icmp_bytes.len()];
        let mut ip_packet = Ipv4Packet::new_unchecked(&mut ip_bytes);
        ip_repr.emit(&mut ip_packet, &ChecksumCapabilities::default());
        ip_packet.payload_mut().copy_from_slice(&icmp_bytes);

        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let eth_repr = EthernetRepr {
            src_addr: guest_mac,
            dst_addr: gateway_mac,
            ethertype: EthernetProtocol::Ipv4,
        };

        let mut frame = vec![0u8; 14 + ip_bytes.len()];
        let mut eth_frame = EthernetFrame::new_unchecked(&mut frame);
        eth_repr.emit(&mut eth_frame);
        eth_frame.payload_mut().copy_from_slice(&ip_bytes);

        let eth = EthernetFrame::new_checked(&frame).unwrap();
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        let icmp = Icmpv4Packet::new_checked(ip.payload()).unwrap();

        assert_eq!(icmp.msg_type(), Icmpv4Message::EchoRequest);
        assert_eq!(icmp.echo_ident(), identifier);
        assert_eq!(icmp.echo_seq_no(), sequence);
        assert_eq!(icmp.data(), payload);
        assert_eq!(icmp.data().len(), payload.len());
    }
}
