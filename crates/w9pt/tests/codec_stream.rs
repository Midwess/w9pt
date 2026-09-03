#![allow(missing_docs)]

use w9pt::{Limits, protocol::FrameDecoder};

const TVERSION: &[u8] = &[
    21, 0, 0, 0, 100, 0xff, 0xff, 0x00, 0x10, 0, 0, 8, 0, b'9', b'P', b'2', b'0', b'0', b'0', b'.',
    b'L',
];

#[test]
fn every_split_point_produces_exactly_one_frame() {
    for split in 0..=TVERSION.len() {
        let mut decoder = FrameDecoder::new(&Limits::default());
        let first = decoder.push(&TVERSION[..split], 4096).unwrap();
        let second = decoder.push(&TVERSION[split..], 4096).unwrap();
        let frames: Vec<_> = first.into_iter().chain(second).collect();
        assert_eq!(frames, vec![TVERSION.to_vec()], "split {split}");
        assert_eq!(decoder.retained_len(), 0);
    }
}

#[test]
fn one_byte_chunks_and_coalesced_frames_work() {
    let mut decoder = FrameDecoder::new(&Limits::default());
    let mut frames = Vec::new();
    for byte in TVERSION {
        frames.extend(decoder.push(core::slice::from_ref(byte), 4096).unwrap());
    }
    assert_eq!(frames, vec![TVERSION.to_vec()]);

    let mut decoder = FrameDecoder::new(&Limits::default());
    let mut input = TVERSION.to_vec();
    input.extend_from_slice(TVERSION);
    assert_eq!(
        decoder.push(&input, 4096).unwrap(),
        vec![TVERSION.to_vec(), TVERSION.to_vec()]
    );
}
