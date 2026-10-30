#![no_main]

use ads::udp::{Message, ServiceId, Tag};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = Message::parse(data, ServiceId::Identify, true) {
        for tag in [
            Tag::Status,
            Tag::Password,
            Tag::TCVersion,
            Tag::OSVersion,
            Tag::ComputerName,
            Tag::NetID,
        ] {
            let _ = (message.get_bytes(tag), message.get_str(tag), message.get_u32(tag));
        }
        let _ = message.get_source();
    }
});
