#![allow(missing_docs)]

use w9pt_storage::{
    Digest, FileId, FormatError, MutationId, ObjectKey, StorageLimits,
    format::{
        BlobRef, BlockEntry, FileHead, FileManifest, ObjectKind, PersistentFormatError,
        decode_envelope, decode_head, decode_manifest, encode_envelope, encode_head,
        encode_manifest,
    },
};

const HEAD_HEX: &str = concat!(
    "573950544f424a0001010000005200000000000000",
    "c5fc33927fb79433f44b425738df2511c81d41db80bc5134286baa16b753c2ed",
    "11111111111111111111111111111111010000000000000006000000702f76312f6d",
    "2222222222222222222222222222222222222222222222222222222222222222",
    "33333333333333333333333333333333"
);
const EMPTY_RAW_HEX: &str = concat!(
    "573950544f424a0002010000002500000000000000",
    "4cb95040f522123d59cd3db3ef4a524046f32c0632bb22bf845411dc268f1270",
    "11111111111111111111111111111111010000000000000000000000000000000101000000"
);
const BLOCK_HEX: &str = concat!(
    "573950544f424a0002010000007300000000000000",
    "adaefbe01a72a4457876ab3aa7cd6321ac0db8f08aafc2367f86da2c7ddc658b",
    "1111111111111111111111111111111102000000000000000180000000000000020100000080000001000000",
    "010000000000000008000000702f76312f622f3100800000000000000080000000000000010000",
    "4444444444444444444444444444444444444444444444444444444444444444"
);

fn fixture_head() -> FileHead {
    FileHead::new(
        FileId::new([0x11; 16]),
        1,
        ObjectKey::new("p/v1/m").unwrap(),
        Digest::new([0x22; 32]),
        MutationId::new([0x33; 16]),
    )
}

fn fixture_raw() -> FileManifest {
    FileManifest::raw(FileId::new([0x11; 16]), 1, 0, None)
}

fn fixture_block() -> FileManifest {
    FileManifest::block_split(
        FileId::new([0x11; 16]),
        2,
        32_769,
        vec![BlockEntry::new(
            1,
            BlobRef::new(
                ObjectKey::new("p/v1/b/1").unwrap(),
                32_768,
                32_768,
                Digest::new([0x44; 32]),
            ),
        )],
    )
}

fn hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = core::str::from_utf8(pair).unwrap();
            u8::from_str_radix(text, 16).unwrap()
        })
        .collect()
}

fn mutate_manifest(encoded: &[u8], mutate: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut payload = decode_envelope(ObjectKind::Manifest, encoded, 1 << 20)
        .unwrap()
        .payload()
        .to_vec();
    mutate(&mut payload);
    encode_envelope(ObjectKind::Manifest, &payload, 1 << 20).unwrap()
}

#[test]
fn independent_head_and_manifest_golden_fixtures_are_stable() {
    let limits = StorageLimits::default();
    let head_bytes = hex(HEAD_HEX);
    let raw_bytes = hex(EMPTY_RAW_HEX);
    let block_bytes = hex(BLOCK_HEX);

    assert_eq!(encode_head(&fixture_head(), limits).unwrap(), head_bytes);
    assert_eq!(encode_manifest(&fixture_raw(), limits).unwrap(), raw_bytes);
    assert_eq!(
        encode_manifest(&fixture_block(), limits).unwrap(),
        block_bytes
    );
    assert_eq!(decode_head(&head_bytes, limits).unwrap(), fixture_head());
    assert_eq!(decode_manifest(&raw_bytes, limits).unwrap(), fixture_raw());
    assert_eq!(
        decode_manifest(&block_bytes, limits).unwrap(),
        fixture_block()
    );
}

#[test]
fn every_truncated_manifest_prefix_is_rejected() {
    let fixture = hex(BLOCK_HEX);
    for length in 0..fixture.len() {
        assert!(decode_manifest(&fixture[..length], StorageLimits::default()).is_err());
    }
}

#[test]
fn malformed_outer_envelopes_are_rejected() {
    let fixture = hex(EMPTY_RAW_HEX);

    let mut bad_magic = fixture.clone();
    bad_magic[0] ^= 1;
    assert!(matches!(
        decode_manifest(&bad_magic, StorageLimits::default()),
        Err(PersistentFormatError::Format(FormatError::InvalidMagic))
    ));

    let mut future_version = fixture.clone();
    future_version[9..11].copy_from_slice(&2_u16.to_le_bytes());
    assert!(matches!(
        decode_manifest(&future_version, StorageLimits::default()),
        Err(PersistentFormatError::Format(
            FormatError::UnsupportedVersion { major: 2, .. }
        ))
    ));

    let mut inconsistent_length = fixture.clone();
    inconsistent_length[13..21].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        decode_manifest(&inconsistent_length, StorageLimits::default()),
        Err(PersistentFormatError::Format(
            FormatError::InconsistentLength { .. }
        ))
    ));

    let mut bad_checksum = fixture;
    *bad_checksum.last_mut().unwrap() ^= 1;
    assert!(matches!(
        decode_manifest(&bad_checksum, StorageLimits::default()),
        Err(PersistentFormatError::Corruption(_))
    ));
}

#[test]
fn malformed_inner_manifests_are_rejected() {
    let fixture = hex(EMPTY_RAW_HEX);

    let unknown_method = mutate_manifest(&fixture, |payload| payload[32] = 99);
    assert!(matches!(
        decode_manifest(&unknown_method, StorageLimits::default()),
        Err(PersistentFormatError::Format(FormatError::UnknownTag {
            field: "storage method",
            ..
        }))
    ));

    let unknown_codec = mutate_manifest(&fixture, |payload| payload[34] = 7);
    assert!(matches!(
        decode_manifest(&unknown_codec, StorageLimits::default()),
        Err(PersistentFormatError::Format(FormatError::UnknownTag {
            field: "payload codec",
            ..
        }))
    ));

    let trailing = mutate_manifest(&fixture, |payload| payload.push(0));
    assert!(matches!(
        decode_manifest(&trailing, StorageLimits::default()),
        Err(PersistentFormatError::Format(FormatError::TrailingData))
    ));

    let overflowing_index = mutate_manifest(&hex(BLOCK_HEX), |payload| {
        payload[44..52].copy_from_slice(&u64::MAX.to_le_bytes());
    });
    assert!(matches!(
        decode_manifest(&overflowing_index, StorageLimits::default()),
        Err(PersistentFormatError::Format(
            FormatError::ArithmeticOverflow {
                field: "block offset"
            }
        ))
    ));
}

#[test]
fn decode_limits_apply_before_nested_allocation() {
    let fixture = hex(BLOCK_HEX);
    let values = w9pt_storage::StorageLimitValues {
        max_manifest_bytes: fixture.len() - 1,
        ..w9pt_storage::StorageLimitValues::default()
    };
    let limits = StorageLimits::new(values).unwrap();
    assert!(matches!(
        decode_manifest(&fixture, limits),
        Err(PersistentFormatError::Limit(_))
    ));

    let huge_count = mutate_manifest(&fixture, |payload| {
        payload[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    });
    assert!(matches!(
        decode_manifest(&huge_count, StorageLimits::default()),
        Err(PersistentFormatError::Limit(_))
    ));
}
