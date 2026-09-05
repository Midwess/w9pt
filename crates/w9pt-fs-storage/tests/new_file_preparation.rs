#![allow(missing_docs)]

use w9pt_fs_storage::{
    BLOCK_SIZE_V1, BaseContentIdentity, ContentRepository, CorruptionError, CreationDefaults,
    FileId, LimitError, LimitKind, MutationId, PreparationIdentity, StorageError,
    StorageLimitValues, StorageLimits, StorageMethod, TargetOperation,
    format::ManifestLayout,
    testing::{FailureTiming, MemoryTarget, TracePhase, block_on},
};

fn repository(
    method: StorageMethod,
    target: MemoryTarget,
    limits: StorageLimits,
) -> ContentRepository<MemoryTarget> {
    ContentRepository::new(target, "new-file", CreationDefaults::new(method), limits).unwrap()
}

#[test]
fn positioned_first_writes_match_a_byte_vector_model() {
    for method in [StorageMethod::Raw, StorageMethod::BlockSplit] {
        let target = MemoryTarget::new();
        let repository = repository(method, target, StorageLimits::default());
        let offset = if method == StorageMethod::Raw {
            17
        } else {
            u64::from(BLOCK_SIZE_V1) * 3 + 17
        };
        let bytes = b"model";
        let prepared = block_on(repository.prepare_write_from_new(
            FileId::from_u128(1),
            MutationId::from_u128(2),
            0,
            offset,
            bytes,
        ))
        .unwrap();
        let mut model = vec![0; usize::try_from(offset).unwrap()];
        model.extend_from_slice(bytes);
        assert_eq!(prepared.identity().base(), BaseContentIdentity::NEW_FILE);
        assert_eq!(prepared.content().generation(), 1);
        assert_eq!(
            block_on(repository.read(prepared.content(), 0, model.len())).unwrap(),
            model
        );
    }
}

#[test]
fn identical_new_file_preparations_have_identical_content_and_traces() {
    for method in [StorageMethod::Raw, StorageMethod::BlockSplit] {
        let run = || {
            let target = MemoryTarget::new();
            let repository = repository(method, target.clone(), StorageLimits::default());
            let write = block_on(repository.prepare_write_from_new(
                FileId::from_u128(3),
                MutationId::from_u128(4),
                7,
                9,
                b"replay",
            ))
            .unwrap();
            let truncate = block_on(repository.prepare_truncate_from_new(
                FileId::from_u128(5),
                MutationId::from_u128(6),
                8,
                19,
            ))
            .unwrap();
            (write, truncate, target.trace().unwrap())
        };
        let first = run();
        let second = run();
        assert_eq!(first, second, "{method:?} replay diverged");
    }
}

#[test]
fn collisions_and_corruption_are_never_accepted_as_preparation() {
    for method in [StorageMethod::Raw, StorageMethod::BlockSplit] {
        let target = MemoryTarget::new();
        let repository = repository(method, target.clone(), StorageLimits::default());
        let file_id = FileId::from_u128(7);
        let mutation_id = MutationId::from_u128(8);
        let prepared =
            block_on(repository.prepare_write_from_new(file_id, mutation_id, 0, 0, b"payload"))
                .unwrap();
        let identity =
            PreparationIdentity::for_write_from_new(mutation_id, method, 0, b"payload").unwrap();
        let payload_key = match method {
            StorageMethod::Raw => repository.keys().raw_payload(file_id, identity, 0),
            StorageMethod::BlockSplit => repository.keys().block_payload(file_id, identity, 0, 0),
        };
        assert!(
            target
                .corrupt(&payload_key, b"not-the-envelope".to_vec())
                .unwrap()
        );
        assert!(matches!(
            block_on(repository.read(prepared.content(), 0, 7)),
            Err(StorageError::Corruption(_)) | Err(StorageError::Format(_))
        ));
        assert!(matches!(
            block_on(repository.prepare_write_from_new(file_id, mutation_id, 0, 0, b"payload",)),
            Err(StorageError::Corruption(
                CorruptionError::ImmutableCollision
            ))
        ));

        let truncated = block_on(repository.prepare_truncate_from_new(
            FileId::from_u128(9),
            MutationId::from_u128(10),
            0,
            4,
        ))
        .unwrap();
        assert!(
            target
                .corrupt(truncated.content().manifest_key(), b"collision".to_vec())
                .unwrap()
        );
        assert!(matches!(
            block_on(repository.prepare_truncate_from_new(
                FileId::from_u128(9),
                MutationId::from_u128(10),
                0,
                4,
            )),
            Err(StorageError::Corruption(
                CorruptionError::ImmutableCollision
            ))
        ));
    }
}

#[test]
fn limits_and_failure_ordering_stop_before_manifest_publication() {
    let limited_target = MemoryTarget::new();
    let limited = repository(
        StorageMethod::BlockSplit,
        limited_target.clone(),
        StorageLimits::new(StorageLimitValues {
            max_blocks: 1,
            ..StorageLimitValues::default()
        })
        .unwrap(),
    );
    let mut two_blocks = vec![0; BLOCK_SIZE_V1 as usize + 1];
    two_blocks[0] = 1;
    two_blocks[BLOCK_SIZE_V1 as usize] = 1;
    assert!(matches!(
        block_on(limited.prepare_write_from_new(
            FileId::from_u128(11),
            MutationId::from_u128(12),
            0,
            0,
            &two_blocks,
        )),
        Err(StorageError::Limit(LimitError {
            kind: LimitKind::BlockCount,
            ..
        }))
    ));
    assert_eq!(limited_target.object_count().unwrap(), 0);

    for method in [StorageMethod::Raw, StorageMethod::BlockSplit] {
        let target = MemoryTarget::new();
        let repository = repository(method, target.clone(), StorageLimits::default());
        let file_id = FileId::from_u128(13);
        let mutation_id = MutationId::from_u128(14);
        let identity =
            PreparationIdentity::for_write_from_new(mutation_id, method, 0, b"x").unwrap();
        let payload_key = match method {
            StorageMethod::Raw => repository.keys().raw_payload(file_id, identity, 0),
            StorageMethod::BlockSplit => repository.keys().block_payload(file_id, identity, 0, 0),
        };
        let manifest_key = repository.keys().manifest(file_id, identity, 0);
        target
            .inject_failure_for(
                TargetOperation::PutIfAbsent,
                FailureTiming::Before,
                manifest_key.clone(),
            )
            .unwrap();
        assert!(
            block_on(repository.prepare_write_from_new(file_id, mutation_id, 0, 0, b"x")).is_err()
        );
        assert!(target.inspect(&payload_key).unwrap().is_some());
        assert!(target.inspect(&manifest_key).unwrap().is_none());
        let puts = target
            .trace()
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.operation == TargetOperation::PutIfAbsent && event.phase == TracePhase::Before
            })
            .map(|event| event.key)
            .collect::<Vec<_>>();
        assert_eq!(puts, vec![payload_key, manifest_key]);
    }

    let target = MemoryTarget::new();
    let repository = repository(
        StorageMethod::BlockSplit,
        target.clone(),
        StorageLimits::default(),
    );
    let identity = PreparationIdentity::for_truncate_from_new(
        MutationId::from_u128(16),
        StorageMethod::BlockSplit,
        1024,
    );
    let manifest_key = repository
        .keys()
        .manifest(FileId::from_u128(15), identity, 0);
    target
        .inject_failure_for(
            TargetOperation::PutIfAbsent,
            FailureTiming::Before,
            manifest_key,
        )
        .unwrap();
    assert!(
        block_on(repository.prepare_truncate_from_new(
            FileId::from_u128(15),
            MutationId::from_u128(16),
            0,
            1024,
        ))
        .is_err()
    );
    assert_eq!(target.object_count().unwrap(), 0);
}

#[test]
fn block_truncates_are_all_holes_and_raw_truncates_materialize_zeros() {
    for method in [StorageMethod::Raw, StorageMethod::BlockSplit] {
        let target = MemoryTarget::new();
        let repository = repository(method, target.clone(), StorageLimits::default());
        let prepared = block_on(repository.prepare_truncate_from_new(
            FileId::from_u128(17),
            MutationId::from_u128(18),
            0,
            u64::from(BLOCK_SIZE_V1) * 2 + 1,
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(prepared.content())).unwrap();
        match manifest.layout() {
            ManifestLayout::Raw { blob } => assert!(blob.is_some()),
            ManifestLayout::BlockSplit { blocks, .. } => assert!(blocks.is_empty()),
        }
        assert_eq!(
            block_on(repository.read(prepared.content(), u64::from(BLOCK_SIZE_V1) * 2 - 1, 2,))
                .unwrap(),
            [0, 0]
        );
    }
}
