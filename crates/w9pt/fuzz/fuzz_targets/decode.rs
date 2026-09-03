#![no_main]

use libfuzzer_sys::fuzz_target;
use w9pt::{Limits, protocol::decode_request};

fuzz_target!(|data: &[u8]| {
    let _ = decode_request(data, &Limits::default(), 1024 * 1024);
});
