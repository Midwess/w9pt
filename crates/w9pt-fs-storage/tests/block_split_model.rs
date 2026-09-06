#![allow(missing_docs)]

use w9pt_fs_storage::{
    BLOCK_SIZE, ContentRef, ContentRepository, CreationDefaults, FileId, MutationId, StorageError,
    StorageLimits, StorageMethod, TargetOperation,
    format::ManifestLayout,
    testing::{MemoryTarget, block_on},
};

fn repository(target: MemoryTarget, method: StorageMethod) -> ContentRepository<MemoryTarget> {
    ContentRepository::new(
        target,
        "private",
        CreationDefaults::new(method),
        StorageLimits::default(),
    )
    .unwrap()
}

#[test]
fn deterministic_block_trace_matches_byte_vector_at_boundaries() {
    let target = MemoryTarget::new();
    let repository = repository(target, StorageMethod::BlockSplit);
    let file_id = FileId::from_u128(1);
    let mut content =
        block_on(repository.prepare_create(file_id, MutationId::from_u128(1), 0, b""))
            .unwrap()
            .into_content();
    let mut model = Vec::<u8>::new();
    let mut random = 0x6a09_e667_f3bc_c909_u64;
    let block = BLOCK_SIZE as usize;

    for step in 1..=160_u128 {
        random = random
            .wrapping_mul(2_862_933_555_777_941_757)
            .wrapping_add(3_037_000_493);
        let mutation = MutationId::from_u128(step + 1);
        let prepared = if step % 5 == 0 {
            let new_size = usize::try_from((random >> 11) % (block as u64 * 4 + 37)).unwrap();
            model.resize(new_size, 0);
            block_on(repository.prepare_truncate(&content, mutation, 0, new_size as u64)).unwrap()
        } else {
            let boundary_bias = match step % 6 {
                0 => block.saturating_sub(1),
                1 => block,
                2 => block + 1,
                _ => usize::try_from((random >> 17) % (block as u64 * 3 + 19)).unwrap(),
            };
            let length = usize::try_from((random >> 43) % 97).unwrap();
            let bytes = (0..length)
                .map(|index| random.rotate_left((index % 64) as u32) as u8)
                .collect::<Vec<_>>();
            if !bytes.is_empty() {
                let end = boundary_bias + bytes.len();
                if end > model.len() {
                    model.resize(end, 0);
                }
                model[boundary_bias..end].copy_from_slice(&bytes);
            }
            block_on(repository.prepare_write(&content, mutation, 0, boundary_bias as u64, &bytes))
                .unwrap()
        };
        content = prepared.into_content();

        assert_eq!(content.logical_size(), model.len() as u64);
        assert_eq!(
            block_on(repository.read(&content, 0, model.len() + 1)).unwrap(),
            model,
            "full model mismatch at step {step}"
        );
        let start = usize::try_from((random >> 29) % (model.len() as u64 + 1)).unwrap();
        let requested = usize::try_from((random >> 37) % 257).unwrap();
        let end = start.saturating_add(requested).min(model.len());
        assert_eq!(
            block_on(repository.read(&content, start as u64, requested)).unwrap(),
            model[start..end],
            "range model mismatch at step {step}"
        );
    }
}

#[test]
fn persisted_method_survives_default_changes_in_both_directions() {
    let target = MemoryTarget::new();
    let raw_repository = repository(target.clone(), StorageMethod::Raw);
    let raw = block_on(raw_repository.prepare_create(
        FileId::from_u128(1),
        MutationId::from_u128(1),
        0,
        b"raw",
    ))
    .unwrap();
    let block_repository = repository(target.clone(), StorageMethod::BlockSplit);
    assert_eq!(
        block_on(block_repository.read(raw.content(), 0, 9)).unwrap(),
        b"raw"
    );

    let block = block_on(block_repository.prepare_create(
        FileId::from_u128(2),
        MutationId::from_u128(2),
        0,
        b"block",
    ))
    .unwrap();
    block_on(block_repository.sync_content(block.content())).unwrap();
    let persisted = block.content();
    let reconstructed = ContentRef::from_persisted(
        persisted.file_id(),
        persisted.generation(),
        persisted.logical_size(),
        persisted.manifest_key().clone(),
        persisted.manifest_digest(),
        persisted.method(),
    )
    .unwrap();
    drop(block_repository);
    let reopened_raw_default = repository(target, StorageMethod::Raw);
    assert_eq!(
        block_on(reopened_raw_default.read(&reconstructed, 0, 9)).unwrap(),
        b"block"
    );
}

#[test]
fn no_op_corruption_sparse_and_overflow_boundaries_are_explicit() {
    let target = MemoryTarget::new();
    let repository = repository(target.clone(), StorageMethod::BlockSplit);
    let file_id = FileId::from_u128(1);
    let created =
        block_on(repository.prepare_create(file_id, MutationId::from_u128(1), 0, b"x")).unwrap();

    target.clear_trace().unwrap();
    let unchanged =
        block_on(repository.prepare_write(created.content(), MutationId::from_u128(2), 0, 0, b"x"))
            .unwrap();
    assert!(!unchanged.content_changed());
    assert!(
        target
            .trace()
            .unwrap()
            .iter()
            .all(|event| event.operation != TargetOperation::PutIfAbsent)
    );

    target.clear_trace().unwrap();
    assert!(matches!(
        block_on(repository.prepare_write(
            created.content(),
            MutationId::from_u128(3),
            0,
            u64::MAX,
            b"z",
        )),
        Err(StorageError::Range(_))
    ));
    assert!(
        target
            .trace()
            .unwrap()
            .iter()
            .all(|event| event.operation != TargetOperation::PutIfAbsent)
    );

    let sparse = block_on(repository.prepare_write(
        created.content(),
        MutationId::from_u128(4),
        0,
        u64::from(BLOCK_SIZE) * 3 + 7,
        b"z",
    ))
    .unwrap();
    let manifest = block_on(repository.load_manifest(sparse.content())).unwrap();
    let ManifestLayout::BlockSplit { root, .. } = manifest.layout() else {
        panic!("expected block-split manifest");
    };
    assert_eq!(
        root.as_ref().map(|root| root.materialized_block_count()),
        Some(2)
    );
    assert_eq!(
        root.as_ref().map(|root| root.highest_materialized_block()),
        Some(3)
    );

    let first_blob_key = repository.keys().block_payload(
        sparse.content().file_id(),
        created.identity(),
        created.attempt(),
        0,
    );
    let mut bytes = target.inspect(&first_blob_key).unwrap().unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    assert!(target.corrupt(&first_blob_key, bytes).unwrap());
    assert!(matches!(
        block_on(repository.read(sparse.content(), 0, 1)),
        Err(StorageError::Corruption(_) | StorageError::Representation(_))
    ));
}
