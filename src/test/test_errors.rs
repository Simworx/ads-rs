use std::io;

use crate::errors::{ads_error, AdsErrorCode, ErrContext};
use crate::Error;

#[test]
fn test_ads_code_is_typed() {
    let err = ads_error::<()>("read", 0x710).unwrap_err();
    assert_eq!(err.ads_code(), Some(AdsErrorCode::SYMBOL_NOT_FOUND));
    assert!(matches!(err, Error::Ads(_, "Symbol not found", 0x710)));
    assert_eq!(u32::from(AdsErrorCode::SYMBOL_NOT_FOUND), 0x710);
    assert_eq!(AdsErrorCode::from(0xFFFF).description(), "Unknown error code");
    assert_eq!(Error::Other("x").ads_code(), None);
}

#[test]
fn test_io_timeout_maps_to_timeout() {
    for kind in [io::ErrorKind::TimedOut, io::ErrorKind::WouldBlock] {
        let err = Err::<(), _>(io::Error::from(kind)).ctx("waiting").unwrap_err();
        assert!(matches!(err, Error::Timeout("waiting")), "{:?}", err);
    }
}

#[test]
fn test_clone_keeps_io_message() {
    let err = Error::Io("reading", io::Error::other("plc went away"));
    assert_eq!(err.clone().to_string(), "reading: plc went away");
}
