//! Test for the TCP client.

use std::convert::TryFrom;
use std::io::{Read, Write};
use std::time::Duration;

use crate::client::{
    AddNotifRequest, AdsHeader, Command, DelNotifRequest, ReadRequest, WriteReadRequest, WriteRequest,
};
use crate::test::{config_test_server, frame, scripted_server, ServerOpts};
use crate::{AmsAddr, AmsNetId, Client, Device, Error, Source, Timeouts};

fn run_test(opts: ServerOpts, f: impl Fn(Device)) {
    let timeouts = if let Some(tmo) = opts.timeout { Timeouts::new(tmo) } else { Timeouts::none() };
    let port = config_test_server(opts);
    let client = Client::new(("127.0.0.1", port), timeouts, Source::Auto).unwrap();
    f(client.device(AmsAddr::new(AmsNetId::new(1, 2, 3, 4, 5, 6), 851)));
}

#[test]
fn test_garbage_packet() {
    run_test(ServerOpts { garbage_header: true, ..Default::default() }, |device| {
        let _ = device.get_info().unwrap_err();
    })
}

#[test]
fn test_timeout() {
    run_test(
        ServerOpts { timeout: Some(Duration::from_millis(1)), ..Default::default() },
        |device| {
            let err = device.get_info().unwrap_err();
            assert!(matches!(err, Error::Timeout(_)), "unexpected error from timeout: {}", err);
        },
    )
}

#[test]
fn test_wrong_invokeid() {
    run_test(ServerOpts { ignore_invokeid: true, ..Default::default() }, |device| {
        assert!(matches!(
            device.get_info().unwrap_err(),
            Error::Reply(_, "invalid invoke id received from server, aborting connection", _)
        ));
    })
}

#[test]
fn test_devinfo() {
    run_test(ServerOpts::default(), |device| {
        let info = device.get_info().unwrap();
        assert_eq!(info.version, 4024);
        assert_eq!(info.name, "Nice device");
    })
}

#[test]
fn test_state() {
    run_test(ServerOpts::default(), |device| {
        device.write_control(crate::AdsState::Config, 42).unwrap();
        assert_eq!(device.get_state().unwrap(), (crate::AdsState::Config, 42));
        assert!(device.write_control(crate::AdsState::Invalid, 42).is_err());
    })
}

#[test]
fn test_readwrite() {
    run_test(ServerOpts::default(), |device| {
        let data = [1, 6, 8, 9];
        let mut buf = [0; 4];
        device.write(0x4020, 7, &data).unwrap();
        device.read_exact(0x04020, 7, &mut buf).unwrap();
        assert_eq!(data, buf);

        assert!(matches!(device.read_exact(0x4021, 0, &mut buf), Err(Error::Ads(_, _, 0x702))));
        assert!(matches!(
            device.read_exact(0x4020, 98765, &mut buf),
            Err(Error::Ads(_, _, 0x703))
        ));

        device.write_value(0x4020, 7, &0xdeadbeef_u32).unwrap();
        assert!(device.read_value::<u32>(0x4020, 7).unwrap() == 0xdeadbeef);
    })
}

#[test]
fn test_multi_requests() {
    use crate::client::*;
    use crate::index::*;
    run_test(ServerOpts::default(), |device| {
        let mut buf1 = *b"ABCDEFGHIJ";
        let mut buf2 = *b"0123456789";
        let mut buf3 = *b"-----";
        let mut reqs = vec![
            WriteRequest::new(0x4020, 7, &buf1),
            WriteRequest::new(0x4020, 9, &buf2),
            WriteRequest::new(0x6789, 5, &buf3),
        ];
        device.write_multi(&mut reqs).unwrap();
        assert!(reqs[0].ensure().is_ok());
        assert!(reqs[1].ensure().is_ok());
        assert!(reqs[2].ensure().is_err());

        let mut reqs = vec![
            ReadRequest::new(0x4020, 7, &mut buf1),
            ReadRequest::new(0x4020, 9, &mut buf2),
            ReadRequest::new(0x7689, 5, &mut buf3),
        ];
        device.read_multi(&mut reqs).unwrap();
        assert!(reqs[0].data().unwrap() == b"AB01234567");
        assert!(reqs[1].data().unwrap() == b"0123456789");

        let mut reqs = vec![
            WriteReadRequest::new(FILE_OPEN, 0, b"/etc/passwd", &mut buf1),
            WriteReadRequest::new(FILE_READ, 0, b"", &mut buf2),
            WriteReadRequest::new(FILE_CLOSE, 42, b"", &mut []),
            WriteReadRequest::new(0x7689, 5, b"blub", &mut buf3),
        ];
        device.write_read_multi(&mut reqs).unwrap();
        assert!(reqs[0].data().unwrap() == 42u32.to_le_bytes());
        assert!(reqs[1].data().unwrap() == b"\0\0\0\0\0\0\0\0\0\0");
        assert!(reqs[2].data().unwrap() == b"");
        assert!(reqs[3].data().is_err());
    });
}

#[test]
fn test_fileaccess() {
    use crate::file::*;
    run_test(ServerOpts::default(), |device| {
        assert!(File::open(device, "blub", 0).is_err());
        let mut file = File::open(device, "/etc/passwd", WRITE).unwrap();
        assert!(File::open(device, "/etc/passwd", 0).is_err());
        assert!(file.read(&mut [0; 4]).is_err());
        file.write_all(b"asdf").unwrap();
        file.flush().unwrap();
        drop(file);

        let mut file = File::open(device, "/etc/passwd", READ).unwrap();
        assert!(file.write(&[0; 4]).is_err());
        let mut vec = vec![];
        file.read_to_end(&mut vec).unwrap();
        assert!(vec.len() == 888);
        assert!(File::delete(device, "/etc/passwd", 0).is_err());
        drop(file);

        File::delete(device, "/etc/passwd", 0).unwrap();
    })
}

#[test]
fn test_symbolaccess() {
    use crate::symbol::*;
    run_test(ServerOpts::default(), |device| {
        assert!(Handle::new(device, "blub").is_err());
        let handle = Handle::new(device, "SYMBOL").unwrap();
        assert!(handle.write(&[1, 2, 3, 4, 5]).is_err());
        assert!(handle.read(&mut [0; 5]).is_err());

        assert!(handle.raw() == 77);

        handle.write(&[4, 3, 2, 1]).unwrap();
        let mut buf = [0; 4];
        handle.read(&mut buf).unwrap();
        assert!(buf == [4, 3, 2, 1]);

        handle.write_value(&0xdeadbeef_u32).unwrap();
        assert!(handle.read_value::<u32>().unwrap() == 0xdeadbeef);
    })
}

#[test]
fn test_notification() {
    use crate::notif::*;
    use std::time::Duration;
    run_test(ServerOpts::default(), |device| {
        let chan = device.client.get_notification_channel();

        let attrib = Attributes::new(
            4,
            TransmissionMode::ServerOnChange,
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        device.write(0x4020, 0, &[4, 4, 1, 1]).unwrap();
        let handle = device.add_notification(0x4020, 0, &attrib).unwrap();
        device.write(0x4020, 0, &[8, 8, 1, 1]).unwrap();
        device.delete_notification(handle).unwrap();

        // Including the add_notification, each request generates a notification
        // from the test server.
        let first = chan.try_recv().unwrap();
        let second = chan.try_recv().unwrap();

        println!("{first:?}");

        let mut samples = first.samples();
        assert_eq!(
            samples.next().unwrap(),
            Sample { handle, timestamp: 0x9988776655443322, data: &[4, 4, 1, 1] }
        );
        assert_eq!(samples.next(), None);
        assert_eq!(
            second.samples().next().unwrap(),
            Sample { handle, timestamp: 0x9988776655443322, data: &[8, 8, 1, 1] }
        );
    })
}

#[test]
fn test_multi_notification() {
    use crate::client::*;
    use crate::notif::*;
    use std::time::Duration;
    run_test(ServerOpts::default(), |device| {
        let attrib = Attributes::new(
            4,
            TransmissionMode::ServerOnChange,
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let mut reqs = [
            AddNotifRequest::new(0x4020, 7, &attrib),
            AddNotifRequest::new(0x6789, 0, &attrib),
        ];
        device.add_notification_multi(&mut reqs).unwrap();
        let handle = reqs[0].handle().unwrap();
        assert!(reqs[1].handle().is_err());

        // The same call carrying caller-supplied ids: the id correlates back to its
        // result without relying on slice position.
        let mut id_reqs = [
            AddNotifRequest::new(0x4020, 7, &attrib).with_id("ok"),
            AddNotifRequest::new(0x6789, 0, &attrib).with_id("bad"),
        ];
        device.add_notification_multi(&mut id_reqs).unwrap();
        assert_eq!(*id_reqs[0].id(), "ok");
        assert!(id_reqs[0].handle().is_ok());
        assert_eq!(*id_reqs[1].id(), "bad");
        assert!(id_reqs[1].handle().is_err());

        // Consuming accessor yields the owned id paired with the handle result, ready
        // to collect successful registrations into a map keyed by handle.
        let registered: std::collections::HashMap<u32, &str> = Vec::from(id_reqs)
            .into_iter()
            .filter_map(|req| {
                let (id, handle) = req.into_id_handle();
                handle.ok().map(|h| (h, id))
            })
            .collect();
        assert_eq!(registered.len(), 1);
        assert!(registered.values().any(|&id| id == "ok"));

        let mut reqs = [DelNotifRequest::new(handle), DelNotifRequest::new(42)];
        device.delete_notification_multi(&mut reqs).unwrap();
        assert!(reqs[0].ensure().is_ok());
        assert!(reqs[1].ensure().is_err());
    });
}

#[test]
fn test_bad_notification() {
    use crate::notif::*;
    use std::time::Duration;
    run_test(ServerOpts { bad_notif: true, ..Default::default() }, |device| {
        let chan = device.client.get_notification_channel();

        let attrib = Attributes::new(
            4,
            TransmissionMode::ServerOnChange,
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let _ = device.add_notification(0x4020, 0, &attrib).unwrap();
        device.write(0x4020, 0, &[8, 8, 1, 1]).unwrap();

        // No notification should have come through.
        assert!(chan.try_recv().is_err());

        // Notification is automatically deleted at end of scope.
    })
}

#[test]
fn test_string_type() {
    type String5 = crate::strings::String<5>;

    run_test(ServerOpts::default(), |device| {
        let mut bstr = String5::try_from("abc").unwrap();
        assert!(<[u8; 5]>::from(bstr) == [b'a', b'b', b'c', 0, 0]);
        assert!(bstr == &b"abc"[..]);
        assert!(bstr == "abc");
        assert!(bstr.len() == 3);
        assert!(bstr.backing_array().len() == 5);

        assert!(String5::try_from("abcdef").is_err());

        let bstr2 = String5::try_from(&b"abc"[..]).unwrap();
        assert!(<[u8; 5]>::from(bstr2) == [b'a', b'b', b'c', 0, 0]);
        assert!(bstr == bstr2);

        assert!(format!("{bstr:?}") == "\"abc\"");

        device.write_value(0x4020, 7, &bstr).unwrap();

        let ret = device.read_value::<String5>(0x4020, 7).unwrap();
        assert!(<[u8; 5]>::from(ret) == [b'a', b'b', b'c', 0, 0]);
        assert!(String::try_from(ret).unwrap() == "abc");
        assert!(<Vec<u8>>::from(ret) == [b'a', b'b', b'c']);
    })
}

#[test]
fn test_wstring_type() {
    type WString5 = crate::strings::WString<5>;

    run_test(ServerOpts::default(), |device| {
        let mut wstr = WString5::try_from("abc").unwrap();
        assert!(<[u16; 5]>::from(wstr) == [b'a' as u16, b'b' as u16, b'c' as u16, 0, 0]);
        assert!(wstr == &[b'a' as u16, b'b' as u16, b'c' as u16][..]);
        assert!(wstr == "abc");
        assert!(wstr.len() == 3);
        assert!(wstr.backing_array().len() == 5);

        assert!(WString5::try_from(&[1, 2, 3, 4, 5, 6][..]).is_err());
        assert!(WString5::try_from("abcdef").is_err());

        assert!(format!("{wstr:?}") == "\"abc\"");

        let wstr2 = WString5::try_from(&[b'a' as u16, b'b' as u16, b'c' as u16][..]).unwrap();
        assert!(<[u16; 5]>::from(wstr2) == [b'a' as u16, b'b' as u16, b'c' as u16, 0, 0]);
        assert!(wstr == wstr2);

        device.write_value(0x4020, 7, &wstr).unwrap();

        let ret = device.read_value::<WString5>(0x4020, 7).unwrap();
        assert!(<[u16; 5]>::from(ret) == [b'a' as u16, b'b' as u16, b'c' as u16, 0, 0]);
        assert!(String::try_from(ret).unwrap() == "abc");
        assert!(<Vec<u16>>::from(ret) == [b'a' as u16, b'b' as u16, b'c' as u16]);
    })
}

fn run_scripted(reply: impl Fn(&AdsHeader) -> Vec<u8> + Send + 'static, f: impl Fn(Device)) {
    let port = scripted_server(reply);
    let timeouts = Timeouts::new(Duration::from_secs(1));
    let client = Client::new(("127.0.0.1", port), timeouts, Source::Auto).unwrap();
    f(client.device(AmsAddr::new(AmsNetId::new(1, 2, 3, 4, 5, 6), 851)));
}

#[test]
fn test_short_notification_is_skipped() {
    run_scripted(
        |request| {
            let mut state = 0u32.to_le_bytes().to_vec();
            state.extend([5, 0, 0, 0]);
            let notif = frame(request, Command::Notification as u16, 4, &[]);
            [notif, frame(request, request.command.get(), 5, &state)].concat()
        },
        |device| assert_eq!(device.get_state().unwrap(), (crate::AdsState::Run, 0)),
    );
}

#[test]
fn test_inconsistent_lengths_fail_pending_request() {
    run_scripted(
        |request| {
            let mut bad = frame(request, request.command.get(), 5, &[0; 8]);
            bad[26..30].copy_from_slice(&1_000_000u32.to_le_bytes());
            bad
        },
        |device| {
            let err = device.get_state().unwrap_err();
            let expected = "AMS/TCP header and AMS header contain inconsistent data";
            assert!(matches!(err, Error::Reply(_, msg, _) if msg == expected), "{:?}", err);
        },
    );
}

#[test]
fn test_short_reply_is_an_error() {
    run_scripted(
        |request| frame(request, request.command.get(), 5, &[]),
        |device| {
            let err = device.get_state().unwrap_err();
            let expected = "reply shorter than its result field";
            assert!(matches!(err, Error::Reply(_, msg, 0) if msg == expected), "{:?}", err);
        },
    );
}

#[test]
fn test_write_read_length_beyond_buffer() {
    run_scripted(
        |request| {
            let payload = [0u32.to_le_bytes(), 100u32.to_le_bytes(), [0; 4]].concat();
            frame(request, request.command.get(), 5, &payload)
        },
        |device| {
            let err = device.write_read(0, 0, &[], &mut [0; 4]).unwrap_err();
            assert!(
                matches!(err, Error::Reply(_, "returned length exceeds buffer", 100)),
                "{:?}",
                err
            );
        },
    );
}

#[test]
fn test_read_length_beyond_buffer() {
    run_scripted(
        |request| {
            let payload = [0u32.to_le_bytes(), 100u32.to_le_bytes(), [0; 4]].concat();
            frame(request, request.command.get(), 5, &payload)
        },
        |device| {
            let mut buf = [0; 4];
            let len = device.read(0, 0, &mut buf);
            assert!(
                matches!(len, Err(Error::Reply(_, "returned length exceeds buffer", 100))),
                "{:?}",
                len
            );
        },
    );
}

#[test]
fn test_samples_of_many_empty_stamps_do_not_overflow_the_stack() {
    const STAMPS: u32 = 100_000;
    let mut frame = vec![0; 42];
    frame.extend_from_slice(&STAMPS.to_le_bytes());
    frame.resize(frame.len() + 12 * STAMPS as usize, 0);
    let notification = crate::notif::Notification::new(frame).unwrap();

    let count = std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(move || notification.samples().count())
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(count, 0);
}

fn run_with_payload(words: &'static [u32], data: &'static [u8], f: impl Fn(Device)) {
    run_scripted(
        move |request| {
            let mut payload = 0u32.to_le_bytes().to_vec();
            words.iter().for_each(|word| payload.extend_from_slice(&word.to_le_bytes()));
            payload.extend_from_slice(data);
            frame(request, request.command.get(), 5, &payload)
        },
        f,
    );
}

fn assert_short(result: crate::Result<impl std::fmt::Debug>) {
    let err = result.unwrap_err();
    assert!(
        matches!(
            err,
            Error::Reply(_, "got less data than expected" | "claimed less data than expected", _)
        ),
        "{:?}",
        err
    );
}

#[test]
fn test_read_claiming_more_than_received_is_an_error() {
    run_with_payload(&[4], &[], |device| {
        assert_short(device.read(0, 0, &mut [0xAA; 4]));
        assert_short(device.read_exact(0, 0, &mut [0xAA; 4]));
        assert_short(device.read_value::<u32>(0, 0));
        assert_short(device.write_read(0, 0, &[], &mut [0xAA; 4]));
        assert_short(device.write_read_exact(0, 0, &[], &mut [0xAA; 4]));
    });
}

#[test]
fn test_read_into_larger_buffer_returns_the_count() {
    run_with_payload(&[2], &[1, 2], |device| {
        let mut buf = [0xAA; 8];
        assert_eq!(device.read(0, 0, &mut buf).unwrap(), 2);
        assert_eq!(buf[..2], [1, 2]);
        assert_eq!(device.write_read(0, 0, &[], &mut buf).unwrap(), 2);
        assert_short(device.read_exact(0, 0, &mut buf));
    });
}

fn assert_all_multis_short(device: Device) {
    let attrs = crate::notif::Attributes::new(
        4,
        crate::notif::TransmissionMode::ServerOnChange,
        Duration::ZERO,
        Duration::ZERO,
    );
    let mut buf = [0xAA; 4];
    assert_short(device.write_multi(&mut [WriteRequest::new(0, 0, &[1])]));
    assert_short(device.read_multi(&mut [ReadRequest::new(0, 0, &mut buf)]));
    assert_short(device.write_read_multi(&mut [WriteReadRequest::new(0, 0, &[], &mut buf)]));
    assert_short(device.add_notification_multi(&mut [AddNotifRequest::new(0, 0, &attrs)]));
    assert_short(device.delete_notification_multi(&mut [DelNotifRequest::new(1)]));
}

#[test]
fn test_multi_claiming_no_results_is_an_error() {
    run_with_payload(&[0], &[], assert_all_multis_short);
}

#[test]
fn test_multi_claiming_results_it_did_not_send_is_an_error() {
    run_with_payload(&[8], &[], assert_all_multis_short);
}

#[test]
fn test_multi_result_length_without_its_data_is_an_error() {
    run_with_payload(&[8, 0, 4], &[], |device| {
        let mut buf = [0xAA; 4];
        assert_short(device.read_multi(&mut [ReadRequest::new(0, 0, &mut buf)]));
        assert_short(device.write_read_multi(&mut [WriteReadRequest::new(0, 0, &[], &mut buf)]));
    });
}

#[test]
fn test_multi_short_read_returns_the_short_data() {
    run_with_payload(&[10, 0, 2], &[1, 2], |device| {
        let mut buf = [0xAA; 4];
        let mut requests = [ReadRequest::new(0, 0, &mut buf)];
        device.read_multi(&mut requests).unwrap();
        assert_eq!(requests[0].data().unwrap(), [1, 2]);

        let mut buf = [0xAA; 4];
        let mut requests = [WriteReadRequest::new(0, 0, &[], &mut buf)];
        device.write_read_multi(&mut requests).unwrap();
        assert_eq!(requests[0].data().unwrap(), [1, 2]);
    });
}

#[test]
fn test_multi_failed_request_needs_no_data() {
    run_with_payload(&[8, 0x710, 4], &[], |device| {
        let mut buf = [0xAA; 4];
        let mut requests = [ReadRequest::new(0, 0, &mut buf)];
        device.read_multi(&mut requests).unwrap();
        assert!(matches!(requests[0].data(), Err(Error::Ads(_, _, 0x710))));
    });
}

#[test]
fn test_multi_length_beyond_buffer_stays_a_per_request_error() {
    run_with_payload(&[8, 0, 100], &[], |device| {
        let mut buf = [0xAA; 4];
        let mut requests = [ReadRequest::new(0, 0, &mut buf)];
        device.read_multi(&mut requests).unwrap();
        assert!(matches!(
            requests[0].data(),
            Err(Error::Reply(_, "returned length exceeds buffer", 100))
        ));
    });
}

#[test]
fn test_idle_client_outlives_its_read_timeout() {
    // The default test server answers while a timeout option would make it go silent
    let port = config_test_server(ServerOpts::default());
    let client =
        Client::new(("127.0.0.1", port), Timeouts::new(Duration::from_millis(50)), Source::Auto).unwrap();
    let device = client.device(AmsAddr::new(AmsNetId::new(1, 2, 3, 4, 5, 6), 851));
    // Nothing crosses the wire for longer than the read timeout
    std::thread::sleep(Duration::from_millis(200));
    device.get_state().expect("a request after an idle period still works");
}
