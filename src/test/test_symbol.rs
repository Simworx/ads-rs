use std::time::Duration;

use crate::symbol::{decode_symbol_info, get_symbol_info, get_type_info_by_name};
use crate::test_util::{FakeServer, Handlers, Reply};
use crate::{AmsAddr, AmsNetId, Client, Error, Source, Timeouts};

// Struct used to avoid clippy::too_many_arguments on build_type_entry.
struct TypeEntryBuilder<'a> {
    name: &'a str,
    type_name: &'a str,
    comment: &'a str,
    size: u32,
    offset: u32,
    base_type: u32,
    flags: u32,
    array_dims: &'a [(i32, i32)],
    sub_items: &'a [Vec<u8>],
}

/// Build a type entry byte blob matching the Beckhoff SYM_DT_UPLOAD wire format.
///
/// The binary layout is fixed by Beckhoff and will not change, so this test
/// ensures our parsing stays in sync with the on-wire structure.
fn build_type_entry(entry: &TypeEntryBuilder) -> Vec<u8> {
    let mut body = Vec::new();

    // Header (version 1)
    body.extend_from_slice(&1u32.to_le_bytes()); // version
    body.extend_from_slice(&0u16.to_le_bytes()); // subitem_index
    body.extend_from_slice(&0u16.to_le_bytes()); // plc_interface_id
    body.extend_from_slice(&0u32.to_le_bytes()); // reserved
    body.extend_from_slice(&entry.size.to_le_bytes());
    body.extend_from_slice(&entry.offset.to_le_bytes());
    body.extend_from_slice(&entry.base_type.to_le_bytes());
    body.extend_from_slice(&entry.flags.to_le_bytes());
    body.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
    body.extend_from_slice(&(entry.type_name.len() as u16).to_le_bytes());
    body.extend_from_slice(&(entry.comment.len() as u16).to_le_bytes());
    body.extend_from_slice(&(entry.array_dims.len() as u16).to_le_bytes());
    body.extend_from_slice(&(entry.sub_items.len() as u16).to_le_bytes());

    // Strings (null-terminated)
    body.extend_from_slice(entry.name.as_bytes());
    body.push(0);
    body.extend_from_slice(entry.type_name.as_bytes());
    body.push(0);
    body.extend_from_slice(entry.comment.as_bytes());
    body.push(0);

    // Array dimensions: each is (lower_bound, element_count)
    for &(lower, upper) in entry.array_dims {
        body.extend_from_slice(&lower.to_le_bytes());
        // on wire this is the total element count, not upper bound
        body.extend_from_slice(&(upper - lower + 1).to_le_bytes());
    }

    // Sub-items (already framed with their own entry_size)
    for sub in entry.sub_items {
        body.extend_from_slice(sub);
    }

    // Frame with leading entry_size (includes itself)
    let entry_size = (body.len() + 4) as u32;
    let mut out = Vec::with_capacity(entry_size as usize);
    out.extend_from_slice(&entry_size.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Verify that our parsing of the Beckhoff SYM_DT_UPLOAD binary format produces
/// a stable, known structure. The on-wire layout is defined by Beckhoff and will
/// not change, so if this test breaks it means our parsing has drifted from the
/// expected output — not that Beckhoff changed the format.
#[test]
fn decode_struct_type_from_bytes() {
    // Simulate a Beckhoff struct "ST_Test" with two fields:
    //   bFlag : BOOL   (offset 0, size 1, base_type 33)
    //   nValue: INT    (offset 2, size 2, base_type 2)
    // Total struct size: 4 (padding byte between BOOL and INT)

    let field_bflag = build_type_entry(&TypeEntryBuilder {
        name: "bFlag",
        type_name: "BOOL",
        comment: "",
        size: 1,
        offset: 0,
        base_type: 33,
        flags: 0x01,
        array_dims: &[],
        sub_items: &[],
    });
    let field_nvalue = build_type_entry(&TypeEntryBuilder {
        name: "nValue",
        type_name: "INT",
        comment: "",
        size: 2,
        offset: 2,
        base_type: 2,
        flags: 0x01,
        array_dims: &[],
        sub_items: &[],
    });

    let struct_entry = build_type_entry(&TypeEntryBuilder {
        name: "ST_Test",
        type_name: "",
        comment: "",
        size: 4,
        offset: 0,
        base_type: 65, // compound type
        flags: 0x01,   // data type flag
        array_dims: &[],
        sub_items: &[field_bflag, field_nvalue],
    });

    let (symbols, type_map) = decode_symbol_info(Vec::new(), struct_entry).unwrap();
    assert!(symbols.is_empty());
    assert_eq!(type_map.len(), 1);

    let ty = type_map.get("ST_Test").expect("ST_Test should be in type map");
    assert_eq!(ty.name, "ST_Test");
    assert_eq!(ty.type_name, "");
    assert_eq!(ty.size, 4);
    assert_eq!(ty.base_type, 65);
    assert_eq!(ty.flags, 0x01);
    assert!(ty.array.is_empty());
    assert_eq!(ty.fields.len(), 2);

    // Field 0: bFlag
    let f0 = &ty.fields[0];
    assert_eq!(f0.name, "bFlag");
    assert_eq!(f0.typ, "BOOL");
    assert_eq!(f0.offset, Some(0));
    assert_eq!(f0.size, 1);
    assert_eq!(f0.base_type, 33);

    // Field 1: nValue
    let f1 = &ty.fields[1];
    assert_eq!(f1.name, "nValue");
    assert_eq!(f1.typ, "INT");
    assert_eq!(f1.offset, Some(2));
    assert_eq!(f1.size, 2);
    assert_eq!(f1.base_type, 2);
}

fn entry(flags: u32, size: u32, base_type: u32, array_dims: &[(i32, i32)], sub_items: &[Vec<u8>]) -> Vec<u8> {
    build_type_entry(&TypeEntryBuilder {
        name: "T",
        type_name: "",
        comment: "",
        size,
        offset: 0,
        base_type,
        flags,
        array_dims,
        sub_items,
    })
}

// Re-frames a built type entry with `extra` appended inside it.
fn with_trailer(entry: Vec<u8>, extra: &[u8]) -> Vec<u8> {
    let body = [&entry[4..], extra].concat();
    [&((body.len() + 4) as u32).to_le_bytes()[..], &body].concat()
}

fn decode_err(type_data: Vec<u8>, symbol_data: Vec<u8>) -> Error {
    match decode_symbol_info(symbol_data, type_data) {
        Ok(_) => panic!("decoded corrupt symbol data"),
        Err(e) => e,
    }
}

#[test]
fn type_entry_shorter_than_its_length_field() {
    let err = decode_err(0u32.to_le_bytes().to_vec(), vec![]);
    assert!(matches!(err, Error::Reply(_, "invalid entry length", 0)), "{:?}", err);
}

#[test]
fn type_entry_longer_than_data() {
    let err = decode_err([100u32.to_le_bytes(), [0; 4]].concat(), vec![]);
    assert!(matches!(err, Error::Reply(_, "invalid entry length", 100)), "{:?}", err);
}

#[test]
fn symbol_entry_longer_than_data() {
    let err = decode_err(vec![], [100u32.to_le_bytes(), [0; 4]].concat());
    assert!(matches!(err, Error::Reply(_, "invalid entry length", 100)), "{:?}", err);
}

#[test]
fn method_entry_longer_than_type() {
    let err = decode_err(with_trailer(entry(0x800, 0, 0, &[], &[]), &[1, 0, 0xE8, 3, 0, 0]), vec![]);
    assert!(matches!(err, Error::Reply(_, "invalid entry length", 1000)), "{:?}", err);
}

#[test]
fn sub_item_longer_than_type() {
    let err = decode_err(entry(0, 0, 0, &[], &[vec![0xFF, 0, 0, 0]]), vec![]);
    assert!(matches!(err, Error::Reply(_, "invalid entry length", 255)), "{:?}", err);
}

#[test]
fn enum_value_shorter_than_base_type() {
    let data = with_trailer(entry(0x2000, 0, 2, &[], &[]), &[1, 0, 1, b'A', 0]);
    let (_, types) = decode_symbol_info(vec![], data).unwrap_or_else(|e| panic!("{}", e));
    assert_eq!(types["T"].enum_info.as_ref().unwrap()[0].value, 0);
}

#[test]
fn array_bounds_overflow() {
    let mut data = entry(0, 0, 0, &[(0, 0)], &[]);
    data[46..50].copy_from_slice(&i32::MIN.to_le_bytes());
    data[50..54].copy_from_slice(&0i32.to_le_bytes());
    let err = decode_err(data, vec![]);
    assert!(matches!(err, Error::Reply(_, "array bounds overflow", 0)), "{:?}", err);
}

// Answers every request with the frame `reply` builds.
fn scripted_server(reply: impl FnMut(&crate::test_util::Request) -> Reply + Send + 'static) -> FakeServer {
    FakeServer::start(Handlers { custom: Some(Box::new(reply)), ..Default::default() }).unwrap()
}

// Answers a type info request with `read_len` as the returned length and `data` as the bytes.
fn type_info_by_name_err(read_len: u32, data: Vec<u8>) -> Error {
    let server = scripted_server(move |request| {
        let payload = [&0u32.to_le_bytes()[..], &read_len.to_le_bytes(), &data].concat();
        Reply::Frame(request.frame(request.command(), 5, &payload))
    });
    let client = Client::new(server.addr(), Timeouts::new(Duration::from_secs(1)), Source::Auto).unwrap();
    let device = client.device(AmsAddr::new(AmsNetId::new(1, 2, 3, 4, 5, 6), 851));
    match get_type_info_by_name(device, "T") {
        Ok(_) => panic!("decoded corrupt type info"),
        Err(e) => e,
    }
}

#[test]
fn type_info_by_name_length_beyond_buffer() {
    let err = type_info_by_name_err(5000, entry(0, 0, 0, &[], &[]));
    assert!(
        matches!(err, Error::Reply(_, "returned length exceeds buffer", 5000)),
        "{:?}",
        err
    );
}

#[test]
fn type_info_by_name_entry_shorter_than_its_length_field() {
    let err = type_info_by_name_err(8, [2u32.to_le_bytes(), [0; 4]].concat());
    assert!(matches!(err, Error::Reply(_, "invalid entry length", 2)), "{:?}", err);
}

#[test]
fn symbol_info_upload_length_beyond_limit() {
    let server = scripted_server(|request| {
        let mut info = [0u8; 64];
        info[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        info[12..16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let payload = [&0u32.to_le_bytes()[..], &64u32.to_le_bytes(), &info].concat();
        Reply::Frame(request.frame(request.command(), 5, &payload))
    });
    let client = Client::new(server.addr(), Timeouts::new(Duration::from_secs(1)), Source::Auto).unwrap();
    let device = client.device(AmsAddr::new(AmsNetId::new(1, 2, 3, 4, 5, 6), 851));
    let err = match get_symbol_info(device) {
        Ok(_) => panic!("allocated a 4 GiB upload"),
        Err(e) => e,
    };
    assert!(
        matches!(err, Error::Reply(_, "upload length exceeds limit", 0xFFFF_FFFF)),
        "{:?}",
        err
    );
}
