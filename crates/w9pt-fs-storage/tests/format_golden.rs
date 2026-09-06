#![allow(missing_docs)]

use w9pt_fs_storage::{
    Digest, FileId, FormatError, MutationId, ObjectKey, StorageLimits,
    format::{
        BlobRef, BlockMapPage, BranchEntry, FileHead, FileManifest, LeafEntry, ObjectKind, PageRef,
        PersistentFormatError, decode_block_map_page, decode_envelope, decode_head,
        decode_manifest, encode_block_map_page, encode_envelope, encode_head, encode_manifest,
    },
};

const HEAD_HEX: &str = concat!(
    "573950544f424a0001030000005200000000000000",
    "8bf1cd06050fd1ba3850094b5f3b74738f7f23b58db84c18a6fca6fe6935fcb5",
    "11111111111111111111111111111111010000000000000006000000702f76322f6d",
    "2222222222222222222222222222222222222222222222222222222222222222",
    "33333333333333333333333333333333"
);
const EMPTY_RAW_HEX: &str = concat!(
    "573950544f424a0002030000002500000000000000",
    "4cb95040f522123d59cd3db3ef4a524046f32c0632bb22bf845411dc268f1270",
    "11111111111111111111111111111111010000000000000000000000000000000101000000"
);
const BLOCK_HEX: &str = concat!(
    "573950544f424a0002030000008000000000000000",
    "ce50bc3ed8da032f3d214c54147bde54be7625ddff2a333351e39f95b2c15ee9",
    "111111111111111111111111111111110200000000000000018000000000000002010000008000008000070601",
    "0e000000702f76322f6d6170732f726f6f748001000000000000",
    "4444444444444444444444444444444444444444444444444444444444444444",
    "00000000000000000001000000000000000100000000000000"
);
const LEAF_HEX: &str = concat!(
    "573950544f424a0004030000006500000000000000",
    "926f4d5fe987a5f549df13b21c6e8bf740d493752a88b8743db2ad0b6e0ddba4",
    "111111111111111111111111111111110080000000000000000080000080000706010001",
    "0a000000702f76322f622f31323900800000000000000080000000000000010000",
    "5555555555555555555555555555555555555555555555555555555555555555"
);
const BRANCH_HEX: &str = concat!(
    "573950544f424a0005030000007700000000000000",
    "36e7ae7989d0103e80840655779b436ab460bd45ad35e790cae4eb931a46716c",
    "111111111111111111111111111111110100000000000000000080000080000706010001",
    "0e000000702f76322f6d6170732f6c6561668001000000000000",
    "6666666666666666666666666666666666666666666666666666666666666666",
    "00800000000000000001000000000000008100000000000000"
);

fn fixture_head() -> FileHead {
    FileHead::new(
        FileId::new([0x11; 16]),
        1,
        ObjectKey::new("p/v2/m").unwrap(),
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
        Some(PageRef::new(
            ObjectKey::new("p/v2/maps/root").unwrap(),
            384,
            Digest::new([0x44; 32]),
            0,
            0,
            1,
            1,
        )),
    )
}

fn fixture_leaf() -> BlockMapPage {
    BlockMapPage::leaf(
        FileId::new([0x11; 16]),
        128,
        vec![LeafEntry::new(
            1,
            BlobRef::new(
                ObjectKey::new("p/v2/b/129").unwrap(),
                32_768,
                32_768,
                Digest::new([0x55; 32]),
            ),
        )],
    )
}

fn fixture_branch() -> BlockMapPage {
    BlockMapPage::branch(
        FileId::new([0x11; 16]),
        1,
        0,
        vec![BranchEntry::new(
            1,
            PageRef::new(
                ObjectKey::new("p/v2/maps/leaf").unwrap(),
                384,
                Digest::new([0x66; 32]),
                0,
                128,
                1,
                129,
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

fn mutate_page(kind: ObjectKind, encoded: &[u8], mutate: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut payload = decode_envelope(kind, encoded, 1 << 20)
        .unwrap()
        .payload()
        .to_vec();
    mutate(&mut payload);
    encode_envelope(kind, &payload, 1 << 20).unwrap()
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
fn independent_leaf_and_branch_golden_fixtures_are_stable() {
    let limits = StorageLimits::default();
    let leaf = hex(LEAF_HEX);
    let branch = hex(BRANCH_HEX);
    assert_eq!(
        encode_block_map_page(&fixture_leaf(), limits).unwrap(),
        leaf
    );
    assert_eq!(
        encode_block_map_page(&fixture_branch(), limits).unwrap(),
        branch
    );
    assert_eq!(
        decode_block_map_page(&leaf, 0, limits).unwrap(),
        fixture_leaf()
    );
    assert_eq!(
        decode_block_map_page(&branch, 1, limits).unwrap(),
        fixture_branch()
    );
    for length in 0..leaf.len() {
        assert!(decode_block_map_page(&leaf[..length], 0, limits).is_err());
    }
    for length in 0..branch.len() {
        assert!(decode_block_map_page(&branch[..length], 1, limits).is_err());
    }
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
    future_version[9..11].copy_from_slice(&4_u16.to_le_bytes());
    assert!(matches!(
        decode_manifest(&future_version, StorageLimits::default()),
        Err(PersistentFormatError::Format(
            FormatError::UnsupportedVersion { major: 4, .. }
        ))
    ));

    let mut earlier_development_version = fixture.clone();
    earlier_development_version[9..11].copy_from_slice(&1_u16.to_le_bytes());
    assert!(matches!(
        decode_manifest(&earlier_development_version, StorageLimits::default()),
        Err(PersistentFormatError::Format(
            FormatError::UnsupportedVersion { major: 1, .. }
        ))
    ));
    earlier_development_version[9..11].copy_from_slice(&2_u16.to_le_bytes());
    assert!(matches!(
        decode_manifest(&earlier_development_version, StorageLimits::default()),
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

    let unsupported_profile = mutate_manifest(&hex(BLOCK_HEX), |payload| {
        payload[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
    });
    assert!(matches!(
        decode_manifest(&unsupported_profile, StorageLimits::default()),
        Err(PersistentFormatError::Format(FormatError::UnknownTag {
            field: "block map profile",
            ..
        }))
    ));
}

#[test]
fn decode_limits_apply_before_nested_allocation() {
    let fixture = hex(BLOCK_HEX);
    let values = w9pt_fs_storage::StorageLimitValues {
        max_manifest_bytes: fixture.len() - 1,
        ..w9pt_fs_storage::StorageLimitValues::default()
    };
    let limits = StorageLimits::new(values).unwrap();
    assert!(matches!(
        decode_manifest(&fixture, limits),
        Err(PersistentFormatError::Limit(_))
    ));

    let invalid_root_presence = mutate_manifest(&fixture, |payload| {
        payload[44] = 2;
    });
    assert!(matches!(
        decode_manifest(&invalid_root_presence, StorageLimits::default()),
        Err(PersistentFormatError::Format(FormatError::UnknownTag {
            field: "map root presence",
            ..
        }))
    ));
}

#[test]
fn malformed_page_relationships_and_exact_page_bound_are_rejected() {
    let limits = StorageLimits::default();
    let duplicate_leaf = BlockMapPage::leaf(
        FileId::new([0x11; 16]),
        0,
        vec![
            LeafEntry::new(
                1,
                BlobRef::new(
                    ObjectKey::new("p/v2/b/1").unwrap(),
                    32_768,
                    32_768,
                    Digest::new([1; 32]),
                ),
            ),
            LeafEntry::new(
                1,
                BlobRef::new(
                    ObjectKey::new("p/v2/b/1b").unwrap(),
                    32_768,
                    32_768,
                    Digest::new([2; 32]),
                ),
            ),
        ],
    );
    assert!(matches!(
        encode_block_map_page(&duplicate_leaf, limits),
        Err(PersistentFormatError::Format(
            FormatError::NonCanonical { .. }
        ))
    ));

    let nondecreasing_child = BlockMapPage::branch(
        FileId::new([0x11; 16]),
        1,
        0,
        vec![BranchEntry::new(
            0,
            PageRef::new(
                ObjectKey::new("p/v2/maps/cycle").unwrap(),
                398,
                Digest::new([3; 32]),
                1,
                0,
                1,
                1,
            ),
        )],
    );
    assert!(matches!(
        encode_block_map_page(&nondecreasing_child, limits),
        Err(PersistentFormatError::Format(
            FormatError::NonCanonical { .. }
        ))
    ));

    let encoded = encode_block_map_page(&fixture_leaf(), limits).unwrap();
    let page_limit = encoded.len() - 1;
    let page_limited = StorageLimits::new(w9pt_fs_storage::StorageLimitValues {
        max_map_page_bytes: page_limit,
        ..w9pt_fs_storage::StorageLimitValues::default()
    })
    .unwrap();
    assert!(matches!(
        encode_block_map_page(&fixture_leaf(), page_limited),
        Err(PersistentFormatError::Limit(w9pt_fs_storage::LimitError {
            kind: w9pt_fs_storage::LimitKind::MapPage,
            ..
        }))
    ));
}

#[test]
fn impossible_reference_lengths_out_of_domain_pages_and_summary_overflow_are_rejected() {
    let limits = StorageLimits::default();
    let impossible_leaf = FileManifest::block_split(
        FileId::from_u128(1),
        1,
        1,
        Some(PageRef::new(
            ObjectKey::new("p/v2/maps/impossible-leaf").unwrap(),
            383,
            Digest::new([1; 32]),
            0,
            0,
            1,
            0,
        )),
    );
    assert!(matches!(
        encode_manifest(&impossible_leaf, limits),
        Err(PersistentFormatError::Format(
            FormatError::InconsistentLength {
                field: "mapping page"
            }
        ))
    ));

    let impossible_branch = FileManifest::block_split(
        FileId::from_u128(1),
        1,
        u64::from(w9pt_fs_storage::BLOCK_SIZE) * 129,
        Some(PageRef::new(
            ObjectKey::new("p/v2/maps/impossible-branch").unwrap(),
            397,
            Digest::new([2; 32]),
            1,
            0,
            1,
            128,
        )),
    );
    assert!(matches!(
        encode_manifest(&impossible_branch, limits),
        Err(PersistentFormatError::Format(
            FormatError::InconsistentLength {
                field: "mapping page"
            }
        ))
    ));

    let out_of_domain = BlockMapPage::leaf(
        FileId::from_u128(1),
        1_u64 << 49,
        vec![LeafEntry::new(
            0,
            BlobRef::new(
                ObjectKey::new("p/v2/b/outside").unwrap(),
                32_768,
                32_768,
                Digest::new([3; 32]),
            ),
        )],
    );
    assert!(matches!(
        encode_block_map_page(&out_of_domain, limits),
        Err(PersistentFormatError::Format(FormatError::NonCanonical {
            field: "mapping page range"
        }))
    ));

    let overflowing_summary = BlockMapPage::leaf(
        FileId::from_u128(1),
        u64::MAX,
        vec![LeafEntry::new(
            1,
            BlobRef::new(
                ObjectKey::new("p/v2/b/overflow").unwrap(),
                32_768,
                32_768,
                Digest::new([4; 32]),
            ),
        )],
    );
    assert!(matches!(
        overflowing_summary.summary(),
        Err(PersistentFormatError::Format(
            FormatError::ArithmeticOverflow {
                field: "leaf highest block"
            }
        ))
    ));
    let encoded_leaf = encode_block_map_page(&fixture_leaf(), limits).unwrap();
    let decoded_out_of_domain = mutate_page(ObjectKind::LeafMap, &encoded_leaf, |payload| {
        payload[17..25].copy_from_slice(&(1_u64 << 49).to_le_bytes());
    });
    assert!(matches!(
        decode_block_map_page(&decoded_out_of_domain, 0, limits),
        Err(PersistentFormatError::Format(FormatError::NonCanonical {
            field: "mapping page range"
        }))
    ));
}
