//! Test for the UDP client.

// use std::io::{self, Read, Write};
// use std::time::Duration;
use std::net::UdpSocket;

use crate::udp;

const OS_VERSION: &[u8] = b"\0\0\0\0\x05\0\0\0\x08\0\0\0\x09\0\0\0\x02\0\0\0T\0e\0s\0t\0\0\0";

fn udp_replier(sock: UdpSocket) {
    let mut buf = [0; 2048];
    let my_addr = crate::AmsAddr::new(crate::AmsNetId::from([1, 2, 3, 4, 5, 6]), 10000);
    loop {
        let (n, sender) = sock.recv_from(&mut buf).unwrap();
        let mut reply = udp::Message::new(udp::ServiceId::Identify, my_addr);
        if udp::Message::parse(&buf[..n], udp::ServiceId::Identify, false).is_ok() {
            reply.set_service(udp::ServiceId::Identify, true);
            reply.add_str(udp::Tag::ComputerName, "box");
            reply.add_bytes(udp::Tag::OSVersion, OS_VERSION);
            reply.add_bytes(udp::Tag::TCVersion, b"\x04\x01\x07\x00");
        } else if let Ok(msg) = udp::Message::parse(&buf[..n], udp::ServiceId::AddRoute, false) {
            reply.set_service(udp::ServiceId::AddRoute, true);
            let status = msg.get_str(udp::Tag::RouteName) != Some("route");
            reply.add_u32(udp::Tag::Status, status as u32);
        } else {
            panic!("received invalid UDP packet");
        };
        sock.send_to(reply.as_bytes(), sender).unwrap();
    }
}

#[test]
fn test_udp() {
    let serversock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = serversock.local_addr().unwrap().port();
    let tgt_netid = crate::AmsNetId::from([1, 2, 3, 4, 5, 6]);

    std::thread::spawn(move || udp_replier(serversock));

    let netid = udp::get_netid(("127.0.0.1", port)).unwrap();
    assert_eq!(netid, tgt_netid);

    let info = udp::get_info(("127.0.0.1", port)).unwrap();
    assert_eq!(info.hostname, "box");
    assert_eq!(info.netid, tgt_netid);
    assert_eq!(info.twincat_version, (4, 1, 7));
    assert_eq!(info.os_version, ("Windows NT", 5, 8, 9, "Test".into()));

    udp::add_route(("127.0.0.1", port), tgt_netid, "a", Some("route"), None, None, false).unwrap();
    assert!(udp::add_route(("127.0.0.1", port), tgt_netid, "a", None, None, None, false).is_err());
}

fn identify_reply() -> Vec<u8> {
    let mut msg = udp::Message::new(udp::ServiceId::Identify, crate::AmsAddr::default());
    msg.set_service(udp::ServiceId::Identify, true);
    msg.as_bytes().to_vec()
}

#[test]
fn test_parse_rejects_item_longer_than_packet() {
    let mut packet = identify_reply();
    packet.extend([1, 0, 0xFF, 0]);
    let Err(err) = udp::Message::parse(&packet, udp::ServiceId::Identify, true) else {
        panic!("parsed a packet with a truncated item");
    };
    assert!(
        matches!(err, crate::Error::Reply(_, "item longer than packet", 255)),
        "{:?}",
        err
    );
}

#[test]
fn test_get_str_of_empty_item_is_none() {
    let mut packet = identify_reply();
    packet.extend([5, 0, 0, 0]);
    let Ok(msg) = udp::Message::parse(&packet, udp::ServiceId::Identify, true) else {
        panic!("empty item rejected");
    };
    assert_eq!(msg.get_str(udp::Tag::ComputerName), None);
}

#[test]
fn test_parse_does_not_trust_item_count() {
    let mut packet = identify_reply();
    packet[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(udp::Message::parse(&packet, udp::ServiceId::Identify, true).is_ok());
}
