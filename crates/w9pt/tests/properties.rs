#![allow(missing_docs)]

use std::panic::{AssertUnwindSafe, catch_unwind};

use w9pt::{
    Limits,
    protocol::{FrameDecoder, decode_request},
};

fn next(state: &mut u64) -> u8 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    (*state >> 32) as u8
}

#[test]
fn arbitrary_complete_frames_never_panic() {
    let limits = Limits::default();
    for seed in 0..128u64 {
        let mut state = seed;
        for length in 0..256usize {
            let bytes: Vec<u8> = (0..length).map(|_| next(&mut state)).collect();
            let result = catch_unwind(AssertUnwindSafe(|| {
                let _ = decode_request(&bytes, &limits, 4096);
            }));
            assert!(
                result.is_ok(),
                "decoder panicked for seed {seed}, length {length}"
            );
        }
    }
}

#[test]
fn arbitrary_stream_chunks_never_panic_or_retain_over_limit() {
    let limits = Limits {
        max_frame_size: 512,
        max_buffered_input_bytes: 512,
        ..Limits::default()
    };
    for seed in 0..128u64 {
        let mut state = seed;
        let bytes: Vec<u8> = (0..1024).map(|_| next(&mut state)).collect();
        let mut decoder = FrameDecoder::new(&limits);
        for chunk in bytes.chunks(1 + usize::from(next(&mut state) % 31)) {
            let result = catch_unwind(AssertUnwindSafe(|| decoder.push(chunk, 512)));
            assert!(result.is_ok(), "stream decoder panicked for seed {seed}");
            if result.unwrap().is_err() {
                decoder = FrameDecoder::new(&limits);
            }
            assert!(decoder.retained_len() <= 512);
        }
    }
}
