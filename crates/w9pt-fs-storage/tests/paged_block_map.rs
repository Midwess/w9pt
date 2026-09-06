#![allow(missing_docs)]

use w9pt_fs_storage::{
    BLOCK_SIZE, ContentRepository, CreationDefaults, FileId, LimitKind, MissingObjectKind,
    MutationId, StorageError, StorageLimitValues, StorageLimits, StorageMethod, TargetOperation,
    format::ManifestLayout,
    testing::{MemoryTarget, TracePhase, block_on},
};

fn repository(target: MemoryTarget) -> ContentRepository<MemoryTarget> {
    ContentRepository::new(
        target,
        "paged",
        CreationDefaults::new(StorageMethod::BlockSplit),
        StorageLimits::default(),
    )
    .unwrap()
}

#[test]
fn roots_grow_at_real_leaf_and_branch_boundaries_and_reads_are_range_local() {
    let target = MemoryTarget::new();
    let repository = repository(target.clone());
    let file_id = FileId::from_u128(1);
    let mut prepared = block_on(repository.prepare_write_from_new(
        file_id,
        MutationId::from_u128(1),
        0,
        u64::from(BLOCK_SIZE) * 127,
        b"a",
    ))
    .unwrap();
    for (mutation, index, value, expected_level) in [
        (2, 128, b'b', 1),
        (3, 16_383, b'c', 1),
        (4, 16_384, b'd', 2),
    ] {
        prepared = block_on(repository.prepare_write(
            prepared.content(),
            MutationId::from_u128(mutation),
            0,
            u64::from(BLOCK_SIZE) * index,
            &[value],
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(prepared.content())).unwrap();
        let ManifestLayout::BlockSplit {
            root: Some(root), ..
        } = manifest.layout()
        else {
            panic!("expected paged root");
        };
        assert_eq!(root.level(), expected_level);
        assert_eq!(root.highest_materialized_block(), index);
    }

    for (index, value) in [(127, b'a'), (128, b'b'), (16_383, b'c'), (16_384, b'd')] {
        assert_eq!(
            block_on(repository.read(prepared.content(), u64::from(BLOCK_SIZE) * index, 1))
                .unwrap(),
            [value]
        );
    }

    target.clear_trace().unwrap();
    assert_eq!(
        block_on(repository.read(prepared.content(), u64::from(BLOCK_SIZE) * 16_384, 1,)).unwrap(),
        b"d"
    );
    let map_gets = target
        .trace()
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.phase == TracePhase::Before
                && event.operation == TargetOperation::Get
                && event.key.as_str().contains("/maps/")
        })
        .collect::<Vec<_>>();
    assert_eq!(map_gets.len(), 3);
    assert!(
        map_gets
            .iter()
            .all(|event| !event.key.as_str().ends_with("0000000000003f80"))
    );
}

#[test]
fn page_immutable_reconciliation_and_missing_page_are_typed() {
    let target = MemoryTarget::new();
    let repository = repository(target.clone());
    let file_id = FileId::from_u128(10);
    let mutation = MutationId::from_u128(11);
    let identity = w9pt_fs_storage::PreparationIdentity::for_write_from_new(
        mutation,
        StorageMethod::BlockSplit,
        0,
        b"x",
    )
    .unwrap();
    let page_key = repository.keys().map_page(file_id, identity, 0, 0, 0);
    target
        .inject_failure_for(
            TargetOperation::PutIfAbsent,
            w9pt_fs_storage::testing::FailureTiming::After,
            page_key.clone(),
        )
        .unwrap();
    let prepared =
        block_on(repository.prepare_write_from_new(file_id, mutation, 0, 0, b"x")).unwrap();
    let retry = block_on(repository.prepare_write_from_new(file_id, mutation, 0, 0, b"x")).unwrap();
    assert_eq!(prepared, retry);

    assert!(target.remove(&page_key).unwrap());
    assert!(matches!(
        block_on(repository.read(prepared.content(), 0, 1)),
        Err(StorageError::Missing {
            kind: MissingObjectKind::MappingPage,
            ..
        })
    ));
}

#[test]
fn page_length_and_digest_corruption_fail_before_children_are_followed() {
    let target = MemoryTarget::new();
    let repository = repository(target.clone());
    let prepared = block_on(repository.prepare_write_from_new(
        FileId::from_u128(20),
        MutationId::from_u128(21),
        0,
        0,
        b"x",
    ))
    .unwrap();
    let manifest = block_on(repository.load_manifest(prepared.content())).unwrap();
    let ManifestLayout::BlockSplit {
        root: Some(root), ..
    } = manifest.layout()
    else {
        panic!("expected paged root");
    };
    let mut bytes = target.inspect(root.key()).unwrap().unwrap();
    bytes.push(0);
    assert!(target.corrupt(root.key(), bytes).unwrap());
    target.clear_trace().unwrap();
    assert!(matches!(
        block_on(repository.read(prepared.content(), 0, 1)),
        Err(StorageError::Target(_))
    ));
    assert!(target.trace().unwrap().iter().all(|event| {
        !event.key.as_str().contains("/blocks/") || event.operation != TargetOperation::Get
    }));
}

#[test]
fn root_only_validation_empty_reads_path_reuse_and_page_work_limits_are_explicit() {
    let target = MemoryTarget::new();
    let repository = repository(target.clone());
    let block = BLOCK_SIZE as usize;
    let mut data = vec![0; block * 3];
    data[0] = 1;
    data[block] = 2;
    data[block * 2] = 3;
    let prepared = block_on(repository.prepare_write_from_new(
        FileId::from_u128(30),
        MutationId::from_u128(31),
        0,
        0,
        &data,
    ))
    .unwrap();

    target.clear_trace().unwrap();
    block_on(repository.validate_content(prepared.content())).unwrap();
    block_on(repository.sync_content(prepared.content())).unwrap();
    assert!(target.trace().unwrap().iter().all(|event| {
        event.operation != TargetOperation::Get || !event.key.as_str().contains("/maps/")
    }));

    target.clear_trace().unwrap();
    assert_eq!(
        block_on(repository.read(prepared.content(), u64::MAX, 0)).unwrap(),
        b""
    );
    assert_eq!(
        block_on(repository.read(prepared.content(), prepared.content().logical_size(), 1))
            .unwrap(),
        b""
    );
    assert!(target.trace().unwrap().iter().all(|event| {
        event.operation != TargetOperation::Get || !event.key.as_str().contains("/maps/")
    }));

    target.clear_trace().unwrap();
    assert_eq!(
        block_on(repository.read(prepared.content(), 0, data.len())).unwrap(),
        data
    );
    let leaf_gets = target
        .trace()
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.phase == TracePhase::Before
                && event.operation == TargetOperation::Get
                && event.key.as_str().contains("/maps/")
        })
        .count();
    assert_eq!(
        leaf_gets, 1,
        "neighboring blocks must reuse one loaded leaf"
    );

    let high = block_on(repository.prepare_write(
        prepared.content(),
        MutationId::from_u128(32),
        0,
        u64::from(BLOCK_SIZE) * 16_384,
        b"h",
    ))
    .unwrap();
    let limits = StorageLimits::new(StorageLimitValues {
        max_map_page_reads: 1,
        ..StorageLimitValues::default()
    })
    .unwrap();
    let limited = ContentRepository::new(
        target.clone(),
        "paged",
        CreationDefaults::new(StorageMethod::BlockSplit),
        limits,
    )
    .unwrap();
    target.clear_trace().unwrap();
    assert!(matches!(
        block_on(limited.read(high.content(), u64::from(BLOCK_SIZE) * 16_384, 1)),
        Err(StorageError::Limit(w9pt_fs_storage::LimitError {
            kind: LimitKind::MapPageReads,
            ..
        }))
    ));
    assert_eq!(
        target
            .trace()
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.phase == TracePhase::Before
                    && event.operation == TargetOperation::Get
                    && event.key.as_str().contains("/maps/")
            })
            .count(),
        1
    );
}

#[test]
fn write_preflight_rejects_known_page_work_and_memory_limits_before_upload() {
    let target = MemoryTarget::new();
    let limits = StorageLimits::new(StorageLimitValues {
        // One pass plans three pages; the combined preflight/replay requirement is six.
        max_map_page_writes: 4,
        ..StorageLimitValues::default()
    })
    .unwrap();
    let limited = ContentRepository::new(
        target.clone(),
        "limited",
        CreationDefaults::new(StorageMethod::BlockSplit),
        limits,
    )
    .unwrap();
    assert!(matches!(
        block_on(limited.prepare_write_from_new(
            FileId::from_u128(40),
            MutationId::from_u128(41),
            0,
            u64::from(BLOCK_SIZE) * 16_384,
            b"x",
        )),
        Err(StorageError::Limit(w9pt_fs_storage::LimitError {
            kind: LimitKind::MapPageWrites,
            ..
        }))
    ));
    assert_eq!(target.object_count().unwrap(), 0);

    let target = MemoryTarget::new();
    let defaults = StorageLimitValues::default();
    let limits = StorageLimits::new(StorageLimitValues {
        max_map_working_bytes: defaults.max_map_page_bytes * 2,
        ..defaults
    })
    .unwrap();
    let limited = ContentRepository::new(
        target.clone(),
        "memory-limited",
        CreationDefaults::new(StorageMethod::BlockSplit),
        limits,
    )
    .unwrap();
    assert!(matches!(
        block_on(limited.prepare_write_from_new(
            FileId::from_u128(42),
            MutationId::from_u128(43),
            0,
            0,
            b"x",
        )),
        Err(StorageError::Limit(w9pt_fs_storage::LimitError {
            kind: LimitKind::MapWorkingBytes,
            ..
        }))
    ));
    assert_eq!(target.object_count().unwrap(), 0);

    let target = MemoryTarget::new();
    let defaults = StorageLimitValues::default();
    let limits = StorageLimits::new(StorageLimitValues {
        max_map_working_bytes: defaults.max_map_page_bytes * 4 + 2_048,
        ..defaults
    })
    .unwrap();
    let tightly_bounded = ContentRepository::new(
        target,
        "memory-tight",
        CreationDefaults::new(StorageMethod::BlockSplit),
        limits,
    )
    .unwrap();
    block_on(tightly_bounded.prepare_write_from_new(
        FileId::from_u128(44),
        MutationId::from_u128(45),
        0,
        0,
        b"x",
    ))
    .unwrap();
}

#[test]
fn full_overwrite_skips_old_payload_partial_and_distant_extension_verify_it() {
    let target = MemoryTarget::new();
    let repository = repository(target.clone());
    let block = BLOCK_SIZE as usize;
    let initial = block_on(repository.prepare_write_from_new(
        FileId::from_u128(50),
        MutationId::from_u128(51),
        0,
        0,
        &vec![1; block],
    ))
    .unwrap();

    target.clear_trace().unwrap();
    let full = block_on(repository.prepare_write(
        initial.content(),
        MutationId::from_u128(52),
        0,
        0,
        &vec![2; block],
    ))
    .unwrap();
    assert!(target.trace().unwrap().iter().all(|event| {
        event.operation != TargetOperation::Get || !event.key.as_str().contains("/blocks/")
    }));

    target.clear_trace().unwrap();
    let partial =
        block_on(repository.prepare_write(full.content(), MutationId::from_u128(53), 0, 7, b"p"))
            .unwrap();
    assert!(target.trace().unwrap().iter().any(|event| {
        event.operation == TargetOperation::Get && event.key.as_str().contains("/blocks/")
    }));

    let short =
        block_on(repository.prepare_truncate(partial.content(), MutationId::from_u128(54), 0, 8))
            .unwrap();
    target.clear_trace().unwrap();
    let distant = block_on(repository.prepare_write(
        short.content(),
        MutationId::from_u128(55),
        0,
        u64::from(BLOCK_SIZE) * 3,
        b"d",
    ))
    .unwrap();
    assert!(target.trace().unwrap().iter().any(|event| {
        event.operation == TargetOperation::Get && event.key.as_str().contains("/blocks/")
    }));
    assert_eq!(
        block_on(repository.read(distant.content(), 0, 8)).unwrap()[7],
        b'p'
    );
}

#[test]
fn changed_pages_finalize_once_in_child_before_parent_order() {
    let target = MemoryTarget::new();
    let repository = repository(target.clone());
    let block = BLOCK_SIZE as usize;
    let initial = block_on(repository.prepare_truncate_from_new(
        FileId::from_u128(60),
        MutationId::from_u128(61),
        0,
        u64::from(BLOCK_SIZE) * 130,
    ))
    .unwrap();
    let mut bytes = vec![0; block * 2];
    bytes[0] = 1;
    bytes[block] = 2;
    target.clear_trace().unwrap();
    let prepared = block_on(repository.prepare_write(
        initial.content(),
        MutationId::from_u128(62),
        0,
        u64::from(BLOCK_SIZE) * 127,
        &bytes,
    ))
    .unwrap();
    let map_puts = target
        .trace()
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.phase == TracePhase::Before
                && event.operation == TargetOperation::PutIfAbsent
                && event.key.as_str().contains("/maps/")
        })
        .map(|event| event.key)
        .collect::<Vec<_>>();
    assert_eq!(map_puts.len(), 3);
    let mut unique = map_puts.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), map_puts.len());
    assert!(map_puts[0].as_str().ends_with("/00/0000000000000000"));
    assert!(map_puts[1].as_str().ends_with("/00/0000000000000080"));
    assert!(map_puts[2].as_str().ends_with("/01/0000000000000000"));
    assert_eq!(
        block_on(repository.read(prepared.content(), u64::from(BLOCK_SIZE) * 127, bytes.len(),))
            .unwrap(),
        bytes
    );
}

#[test]
fn shrink_prunes_unread_suffix_collapses_root_and_reextension_stays_zero() {
    let target = MemoryTarget::new();
    let repository = repository(target.clone());
    let file_id = FileId::from_u128(70);
    let mut prepared =
        block_on(repository.prepare_write_from_new(file_id, MutationId::from_u128(71), 0, 0, b"a"))
            .unwrap();
    prepared = block_on(repository.prepare_write(
        prepared.content(),
        MutationId::from_u128(72),
        0,
        u64::from(BLOCK_SIZE) * 128,
        b"b",
    ))
    .unwrap();
    prepared = block_on(repository.prepare_write(
        prepared.content(),
        MutationId::from_u128(73),
        0,
        u64::from(BLOCK_SIZE) * 16_384,
        b"c",
    ))
    .unwrap();

    target.clear_trace().unwrap();
    let shrunk = block_on(repository.prepare_truncate(
        prepared.content(),
        MutationId::from_u128(74),
        0,
        u64::from(BLOCK_SIZE) * 128,
    ))
    .unwrap();
    let manifest = block_on(repository.load_manifest(shrunk.content())).unwrap();
    let ManifestLayout::BlockSplit {
        root: Some(root), ..
    } = manifest.layout()
    else {
        panic!("expected retained root");
    };
    assert_eq!(root.level(), 0);
    assert_eq!(root.highest_materialized_block(), 0);
    assert!(target.trace().unwrap().iter().all(|event| {
        event.operation != TargetOperation::Get
            || (!event.key.as_str().ends_with("/00/0000000000004000")
                && !event.key.as_str().contains("/blocks/0000000000004000"))
    }));
    assert!(target.trace().unwrap().iter().all(|event| {
        event.operation != TargetOperation::PutIfAbsent || !event.key.as_str().contains("/maps/")
    }));

    let extended = block_on(repository.prepare_truncate(
        shrunk.content(),
        MutationId::from_u128(75),
        0,
        u64::from(BLOCK_SIZE) * 16_385,
    ))
    .unwrap();
    assert_eq!(
        block_on(repository.read(extended.content(), u64::from(BLOCK_SIZE) * 16_384, 1,)).unwrap(),
        b"\0"
    );

    target.clear_trace().unwrap();
    let zero =
        block_on(repository.prepare_truncate(extended.content(), MutationId::from_u128(76), 0, 0))
            .unwrap();
    let zero_manifest = block_on(repository.load_manifest(zero.content())).unwrap();
    let ManifestLayout::BlockSplit { root, .. } = zero_manifest.layout() else {
        panic!("expected block split");
    };
    assert!(root.is_none());
    assert!(target.trace().unwrap().iter().all(|event| {
        event.operation != TargetOperation::Get || !event.key.as_str().contains("/maps/")
    }));
}

#[test]
fn highest_representable_position_uses_level_six_without_byte_endpoint_overflow() {
    let target = MemoryTarget::new();
    let repository = repository(target);
    let offset = u64::MAX - 1;
    let prepared = block_on(repository.prepare_write_from_new(
        FileId::from_u128(80),
        MutationId::from_u128(81),
        0,
        offset,
        b"x",
    ))
    .unwrap();
    assert_eq!(prepared.content().logical_size(), u64::MAX);
    let manifest = block_on(repository.load_manifest(prepared.content())).unwrap();
    let ManifestLayout::BlockSplit {
        root: Some(root), ..
    } = manifest.layout()
    else {
        panic!("expected maximum-depth root");
    };
    assert_eq!(root.level(), 6);
    assert_eq!(root.highest_materialized_block(), (1_u64 << 49) - 1);
    assert_eq!(
        block_on(repository.read(prepared.content(), offset, 1)).unwrap(),
        b"x"
    );
}

#[test]
fn failures_before_and_after_each_mapping_dependency_never_expose_partial_roots() {
    use w9pt_fs_storage::testing::FailureTiming;

    for timing in [FailureTiming::Before, FailureTiming::After] {
        for (level, first) in [(0, 16_384), (1, 16_384), (2, 0)] {
            let target = MemoryTarget::new();
            let repository = repository(target.clone());
            let file_id = FileId::from_u128(90 + u128::from(level));
            let mutation = MutationId::from_u128(100 + u128::from(level));
            let offset = u64::from(BLOCK_SIZE) * 16_384;
            let identity = w9pt_fs_storage::PreparationIdentity::for_write_from_new(
                mutation,
                StorageMethod::BlockSplit,
                offset,
                b"x",
            )
            .unwrap();
            let page_key = repository
                .keys()
                .map_page(file_id, identity, 0, level, first);
            let manifest_key = repository.keys().manifest(file_id, identity, 0);
            target
                .inject_failure_for(TargetOperation::PutIfAbsent, timing, page_key)
                .unwrap();
            let result =
                block_on(repository.prepare_write_from_new(file_id, mutation, 0, offset, b"x"));
            match timing {
                FailureTiming::Before => {
                    assert!(result.is_err());
                    assert!(target.inspect(&manifest_key).unwrap().is_none());
                }
                FailureTiming::After => {
                    let prepared = result.unwrap();
                    assert_eq!(
                        block_on(repository.read(prepared.content(), offset, 1)).unwrap(),
                        b"x"
                    );
                }
            }
        }
    }
}
