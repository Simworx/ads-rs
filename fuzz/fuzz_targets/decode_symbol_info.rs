#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: (Vec<u8>, Vec<u8>)| {
    let _ = ads::symbol::decode_symbol_info(input.0, input.1);
});
