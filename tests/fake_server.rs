#![cfg(feature = "test-util")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ads::test_util::{FakeServer, Handler, Handlers, Reply, Request};
use ads::{AdsState, AmsAddr, AmsNetId, Client, Error, Handle, Source, Timeouts};

#[test]
fn fake_server_serves_symbols_and_state() {
    let server = FakeServer::start(Handlers::default()).unwrap();
    server.add_symbol("MAIN.counter", &7u32.to_le_bytes());
    server.set_state(AdsState::Stop, 3);
    let client = Client::new(server.addr(), Timeouts::new(Duration::from_secs(1)), Source::Auto).unwrap();
    let device = client.device(AmsAddr::new(AmsNetId::new(1, 2, 3, 4, 5, 6), 851));
    assert_eq!(device.get_state().unwrap(), (AdsState::Stop, 3));
    let handle = Handle::new(device, "MAIN.counter").unwrap();
    assert_eq!(handle.read_value::<u32>().unwrap(), 7);
    drop(handle);
    drop(client);
    server.stop();
}

fn connect(server: &FakeServer, timeout: Duration) -> Client {
    Client::new(server.addr(), Timeouts::new(timeout), Source::Auto).unwrap()
}

fn plc() -> AmsAddr {
    AmsAddr::new(AmsNetId::new(1, 2, 3, 4, 5, 6), 851)
}

#[test]
fn close_reply_drops_the_connection() {
    let custom: Handler = Box::new(|_: &Request| Reply::Close);
    let server = FakeServer::start(Handlers { custom: Some(custom), ..Default::default() }).unwrap();
    let client = connect(&server, Duration::from_secs(5));
    let device = client.device(plc());
    let start = Instant::now();
    for _ in 0..2 {
        let err = device.get_state().unwrap_err();
        assert!(!matches!(err, Error::Timeout(_)), "{:?}", err);
    }
    assert!(start.elapsed() < Duration::from_secs(2), "took {:?}", start.elapsed());
}

#[test]
fn request_target_is_the_device_address() {
    let seen = Arc::new(Mutex::new(None));
    let record = seen.clone();
    let custom: Handler = Box::new(move |request: &Request| {
        *record.lock().unwrap() = Some(request.target());
        Reply::Default
    });
    let server = FakeServer::start(Handlers { custom: Some(custom), ..Default::default() }).unwrap();
    let client = connect(&server, Duration::from_secs(1));
    client.device(plc()).get_state().unwrap();
    assert_eq!(*seen.lock().unwrap(), Some(plc()));
}

#[test]
fn notification_reaches_the_client() {
    let custom: Handler = Box::new(|request: &Request| {
        let data = [9u8, 8, 7, 6];
        let mut stream = Vec::new();
        stream.extend((4 + 8 + 4 + 4 + 4 + data.len() as u32).to_le_bytes());
        stream.extend(1u32.to_le_bytes());
        stream.extend(0x1122u64.to_le_bytes());
        stream.extend(1u32.to_le_bytes());
        stream.extend(5u32.to_le_bytes());
        stream.extend((data.len() as u32).to_le_bytes());
        stream.extend(data);
        let state = [0u32.to_le_bytes(), [5, 0, 0, 0]].concat();
        Reply::Frame(
            [
                request.notification(&stream),
                request.frame(request.command(), 5, &state),
            ]
            .concat(),
        )
    });
    let server = FakeServer::start(Handlers { custom: Some(custom), ..Default::default() }).unwrap();
    let client = connect(&server, Duration::from_secs(1));
    let channel = client.get_notification_channel();
    client.device(plc()).get_state().unwrap();
    let notification = channel.recv_timeout(Duration::from_secs(1)).unwrap();
    let sample = notification.samples().next().unwrap();
    assert_eq!((sample.handle, sample.timestamp, sample.data), (5, 0x1122, &[9u8, 8, 7, 6][..]));
}

#[test]
fn split_frames_stops_at_end_of_input() {
    let (notifications, result) = ads::test_util::split_frames(&[0, 0, 32, 0, 0, 0]);
    assert!(notifications.is_empty());
    assert!(result.is_err());
}
