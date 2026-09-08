#![allow(missing_docs)]

use core::fmt;

#[cfg(any(feature = "compression-lz4", feature = "encryption-aes-siv"))]
use w9pt_fs_storage::Publication;
use w9pt_fs_storage::{
    CompressionPolicy, ContentCipher, ContentRepository, CreationDefaults, MutationId,
    StorageLimits,
    testing::{MemoryTarget, block_on},
};
use w9pt_fs_storage::{
    ContentContextId, Digest, FileContextScope, FileId, FileStoragePolicy, ObjectProvenance,
    PreparationIdentity, RepresentationError, SecureEntropy, StorageError, StorageMethod,
    TargetStore, encode_object,
    format::{FileHead, FileManifest, ObjectKind, encode_head, encode_manifest},
    generate_content_metadata, open_committed_context,
};
#[cfg(feature = "encryption-aes-siv")]
use w9pt_fs_storage::{
    CorruptionError, MasterKey, MasterKeyId, TargetOperation, format::ManifestLayout,
    rewrap_content_metadata,
};
#[cfg(all(feature = "compression-lz4", feature = "encryption-aes-siv"))]
use w9pt_fs_storage::{LimitKind, StorageLimitValues, testing::FailureTiming};

#[derive(Debug)]
struct EntropyError;

impl fmt::Display for EntropyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("entropy failure")
    }
}

impl std::error::Error for EntropyError {}

struct FixedEntropy {
    byte: u8,
    calls: usize,
    fail: bool,
}

impl SecureEntropy for FixedEntropy {
    type Error = EntropyError;

    fn fill_secure(&mut self, destination: &mut [u8]) -> Result<(), Self::Error> {
        self.calls += 1;
        if self.fail {
            return Err(EntropyError);
        }
        destination.fill(self.byte);
        Ok(())
    }
}

fn scope(method: StorageMethod, ordinal: u8) -> (FileContextScope, FileStoragePolicy) {
    let file = FileId::from_u128(u128::from(ordinal) + 1);
    (
        FileContextScope::new(
            [ordinal; 16],
            [ordinal.wrapping_add(1); 16],
            file,
            ContentContextId::from_u128(u128::from(ordinal) + 100),
        ),
        FileStoragePolicy::plain(method),
    )
}

#[test]
fn plain_candidate_pins_policy_without_entropy_or_master() {
    let (scope, policy) = scope(StorageMethod::Raw, 1);
    let mut entropy = FixedEntropy {
        byte: 7,
        calls: 0,
        fail: true,
    };
    let candidate = generate_content_metadata(scope, policy, None, &mut entropy).unwrap();
    assert_eq!(entropy.calls, 0);
    assert!(candidate.key_commitment().is_none());
    assert!(candidate.wrapped_key_bytes().is_none());
    let context = open_committed_context(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        None,
        None,
        1,
        None,
    )
    .unwrap();
    assert_eq!(context.policy(), policy);
    assert!(format!("{context:?}").contains("None"));
}

#[test]
fn standalone_publisher_rejects_authenticated_manifest_method_mismatch() {
    let (scope, policy) = scope(StorageMethod::Raw, 36);
    let mut entropy = FixedEntropy {
        byte: 0,
        calls: 0,
        fail: true,
    };
    let candidate = generate_content_metadata(scope, policy, None, &mut entropy).unwrap();
    let context = open_committed_context(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        None,
        None,
        1,
        None,
    )
    .unwrap();
    let target = MemoryTarget::new();
    let repository = ContentRepository::new(
        target.clone(),
        "standalone-method-mismatch",
        CreationDefaults::new(StorageMethod::Raw),
        StorageLimits::default(),
    )
    .unwrap();
    let mutation = MutationId::from_u128(3_600);
    let identity = PreparationIdentity::for_create(mutation, StorageMethod::Raw, b"").unwrap();
    let provenance = ObjectProvenance::Manifest {
        identity,
        attempt: 0,
        generation: 1,
    };
    let manifest_key = repository.keys().manifest(scope.file_id(), identity, 0);
    let canonical = encode_manifest(
        &FileManifest::block_split(scope.file_id(), 1, 0, None),
        StorageLimits::default(),
    )
    .unwrap();
    let stored = encode_object(
        ObjectKind::Manifest,
        &manifest_key,
        &canonical,
        provenance,
        &context,
        false,
        StorageLimits::default().max_manifest_bytes(),
    )
    .unwrap();
    assert!(matches!(
        block_on(target.put_if_absent(manifest_key.clone(), stored.clone())).unwrap(),
        w9pt_fs_storage::PutIfAbsent::Created { .. }
    ));
    let head = FileHead::new(
        scope.file_id(),
        1,
        manifest_key,
        Digest::blake3(&stored),
        mutation,
    );
    let head_key = repository.keys().head(scope.file_id());
    let head_bytes = encode_head(&head, StorageLimits::default()).unwrap();
    assert!(matches!(
        block_on(target.compare_exchange(head_key, None, head_bytes)).unwrap(),
        w9pt_fs_storage::CompareExchange::Replaced { .. }
    ));
    assert!(matches!(
        block_on(
            repository
                .publisher()
                .load_with_context(scope.file_id(), &context)
        ),
        Err(StorageError::Representation(
            RepresentationError::ContextMismatch
        ))
    ));
}

#[cfg(feature = "encryption-aes-siv")]
#[test]
fn generated_key_wrong_master_and_same_dek_rewrap_are_checked_and_redacted() {
    let (scope, _) = scope(StorageMethod::BlockSplit, 2);
    let policy = FileStoragePolicy::new(
        StorageMethod::BlockSplit,
        CompressionPolicy::Identity,
        ContentCipher::Aes256SivV1,
    );
    let old = MasterKey::new(MasterKeyId::new([1; 16]), [2; 32]);
    let new = MasterKey::new(MasterKeyId::new([3; 16]), [4; 32]);
    let wrong = MasterKey::new(MasterKeyId::new([5; 16]), [6; 32]);
    let mut entropy = FixedEntropy {
        byte: 9,
        calls: 0,
        fail: false,
    };
    let candidate = generate_content_metadata(scope, policy, Some(&old), &mut entropy).unwrap();
    assert_eq!(
        candidate
            .wrapped_key_bytes()
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        "573950544b4559000100010101010101010101010101010101013000de782a3878680eac7b710b31ca8c72a50a54b7f49abbc2d1e09272ff3b9d821883029fedf7e2c12b3602309232a29bb0"
    );
    assert_eq!(entropy.calls, 1);
    assert!(!format!("{old:?}").contains(&"02".repeat(16)));
    assert_eq!(
        open_committed_context(
            scope,
            candidate.policy_format(),
            candidate.policy_bytes(),
            candidate.key_commitment().copied(),
            candidate.wrapped_key_bytes(),
            1,
            Some(&wrong),
        )
        .unwrap_err(),
        RepresentationError::WrongMaster
    );
    let rewrapped = rewrap_content_metadata(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        *candidate.key_commitment().unwrap(),
        candidate.wrapped_key_bytes().unwrap(),
        &old,
        &new,
    )
    .unwrap();
    let context = open_committed_context(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        candidate.key_commitment().copied(),
        Some(&rewrapped),
        2,
        Some(&new),
    )
    .unwrap();
    assert_eq!(
        context.binding().key_commitment(),
        candidate.key_commitment()
    );

    let mut other_entropy = FixedEntropy {
        byte: 10,
        calls: 0,
        fail: false,
    };
    let other = generate_content_metadata(scope, policy, Some(&old), &mut other_entropy).unwrap();
    assert_ne!(candidate.key_commitment(), other.key_commitment());
    let swapped_scope = FileContextScope::new(
        *scope.filesystem_id(),
        *scope.owner_inode_id(),
        scope.file_id(),
        ContentContextId::from_u128(999),
    );
    assert!(matches!(
        open_committed_context(
            swapped_scope,
            candidate.policy_format(),
            candidate.policy_bytes(),
            candidate.key_commitment().copied(),
            candidate.wrapped_key_bytes(),
            1,
            Some(&old),
        ),
        Err(RepresentationError::AuthenticationFailed)
    ));
    let mut changed_policy = candidate.policy_bytes().to_vec();
    changed_policy[2] ^= 1;
    assert_eq!(
        open_committed_context(
            scope,
            candidate.policy_format(),
            &changed_policy,
            candidate.key_commitment().copied(),
            candidate.wrapped_key_bytes(),
            1,
            Some(&old),
        )
        .unwrap_err(),
        RepresentationError::AuthenticationFailed
    );
    assert_eq!(
        open_committed_context(
            scope,
            candidate.policy_format(),
            candidate.policy_bytes(),
            Some([0x99; 32]),
            candidate.wrapped_key_bytes(),
            1,
            Some(&old),
        )
        .unwrap_err(),
        RepresentationError::AuthenticationFailed
    );

    let mut failing = FixedEntropy {
        byte: 0,
        calls: 0,
        fail: true,
    };
    assert!(matches!(
        generate_content_metadata(scope, policy, Some(&old), &mut failing),
        Err(w9pt_fs_storage::CandidateKeyError::Entropy(_))
    ));
}

#[cfg(feature = "encryption-aes-siv")]
#[test]
fn rewrap_preserves_content_refs_keys_and_stored_bytes() {
    let (scope, _) = scope(StorageMethod::BlockSplit, 30);
    let policy = FileStoragePolicy::new(
        StorageMethod::BlockSplit,
        CompressionPolicy::Identity,
        ContentCipher::Aes256SivV1,
    );
    let old = MasterKey::new(MasterKeyId::new([0x31; 16]), [0x32; 32]);
    let new = MasterKey::new(MasterKeyId::new([0x33; 16]), [0x34; 32]);
    let mut entropy = FixedEntropy {
        byte: 0x35,
        calls: 0,
        fail: false,
    };
    let candidate = generate_content_metadata(scope, policy, Some(&old), &mut entropy).unwrap();
    let context = open_committed_context(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        candidate.key_commitment().copied(),
        candidate.wrapped_key_bytes(),
        7,
        Some(&old),
    )
    .unwrap();
    let target = MemoryTarget::new();
    let repository = ContentRepository::new(
        target.clone(),
        "rewrap-invariance",
        CreationDefaults::new(StorageMethod::Raw),
        StorageLimits::default(),
    )
    .unwrap();
    let offset = u64::from(w9pt_fs_storage::BLOCK_SIZE) * 16_384 + 3;
    let bytes = vec![0x44; 1_025];
    let prepared = block_on(repository.prepare_write_from_new_with_context(
        &context,
        MutationId::from_u128(3_000),
        0,
        offset,
        &bytes,
    ))
    .unwrap();
    let content = prepared.content().clone();
    let Publication::Published(published) = block_on(repository.publisher().create_with_context(
        scope.file_id(),
        &context,
        MutationId::from_u128(3_000),
        &prepared,
    ))
    .unwrap() else {
        panic!("fresh protected standalone head must publish");
    };
    let put_keys = target
        .trace()
        .unwrap()
        .into_iter()
        .filter(|event| event.operation == TargetOperation::PutIfAbsent)
        .map(|event| event.key)
        .collect::<Vec<_>>();
    let stored_before = put_keys
        .iter()
        .map(|key| target.inspect(key).unwrap().unwrap())
        .collect::<Vec<_>>();
    target.clear_trace().unwrap();

    let wrapped = rewrap_content_metadata(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        *candidate.key_commitment().unwrap(),
        candidate.wrapped_key_bytes().unwrap(),
        &old,
        &new,
    )
    .unwrap();
    let reopened = open_committed_context(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        candidate.key_commitment().copied(),
        Some(&wrapped),
        8,
        Some(&new),
    )
    .unwrap();
    let reopened_published = block_on(
        repository
            .publisher()
            .load_with_context(scope.file_id(), &reopened),
    )
    .unwrap()
    .unwrap();
    assert_eq!(reopened_published.content(), published.content());
    assert_eq!(
        block_on(repository.read_with_context(&content, &reopened, offset, bytes.len())).unwrap(),
        bytes
    );
    assert!(
        target
            .trace()
            .unwrap()
            .iter()
            .all(|event| event.operation != TargetOperation::PutIfAbsent)
    );
    assert_eq!(
        put_keys
            .iter()
            .map(|key| target.inspect(key).unwrap().unwrap())
            .collect::<Vec<_>>(),
        stored_before
    );
    assert_eq!(prepared.content(), &content);
    assert_eq!(
        open_committed_context(
            scope,
            candidate.policy_format(),
            candidate.policy_bytes(),
            candidate.key_commitment().copied(),
            Some(&wrapped),
            8,
            Some(&old),
        )
        .unwrap_err(),
        RepresentationError::WrongMaster
    );
}

#[cfg(feature = "encryption-aes-siv")]
#[test]
fn protected_roots_and_payloads_reject_tamper_and_cross_key_substitution() {
    let (scope, _) = scope(StorageMethod::Raw, 31);
    let policy = FileStoragePolicy::new(
        StorageMethod::Raw,
        CompressionPolicy::Identity,
        ContentCipher::Aes256SivV1,
    );
    let master = MasterKey::new(MasterKeyId::new([0x41; 16]), [0x42; 32]);
    let mut entropy = FixedEntropy {
        byte: 0x43,
        calls: 0,
        fail: false,
    };
    let candidate = generate_content_metadata(scope, policy, Some(&master), &mut entropy).unwrap();
    let context = open_committed_context(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        candidate.key_commitment().copied(),
        candidate.wrapped_key_bytes(),
        1,
        Some(&master),
    )
    .unwrap();
    let target = MemoryTarget::new();
    let repository = ContentRepository::new(
        target.clone(),
        "protected-corruption",
        CreationDefaults::new(StorageMethod::BlockSplit),
        StorageLimits::default(),
    )
    .unwrap();
    let first = block_on(repository.prepare_create_with_context(
        &context,
        MutationId::from_u128(3_100),
        0,
        &[0x11; 32],
    ))
    .unwrap();
    let second = block_on(repository.prepare_create_with_context(
        &context,
        MutationId::from_u128(3_101),
        0,
        &[0x22; 32],
    ))
    .unwrap();
    let first_manifest =
        block_on(repository.load_manifest_with_context(first.content(), &context)).unwrap();
    let second_manifest =
        block_on(repository.load_manifest_with_context(second.content(), &context)).unwrap();
    let ManifestLayout::Raw {
        blob: Some(first_blob),
    } = first_manifest.layout()
    else {
        panic!("expected first raw payload");
    };
    let ManifestLayout::Raw {
        blob: Some(second_blob),
    } = second_manifest.layout()
    else {
        panic!("expected second raw payload");
    };
    let second_bytes = target.inspect(second_blob.key()).unwrap().unwrap();
    assert!(target.corrupt(first_blob.key(), second_bytes).unwrap());
    assert!(matches!(
        block_on(repository.read_with_context(first.content(), &context, 0, 32)),
        Err(StorageError::Representation(
            RepresentationError::AuthenticationFailed
        ))
    ));

    let empty = block_on(repository.prepare_create_with_context(
        &context,
        MutationId::from_u128(3_102),
        0,
        b"",
    ))
    .unwrap();
    let mut root = target
        .inspect(empty.content().manifest_key())
        .unwrap()
        .unwrap();
    *root.last_mut().unwrap() ^= 1;
    assert!(
        target
            .corrupt(empty.content().manifest_key(), root)
            .unwrap()
    );
    assert!(matches!(
        block_on(repository.load_manifest_with_context(empty.content(), &context)),
        Err(StorageError::Representation(
            RepresentationError::AuthenticationFailed
        )) | Err(StorageError::Corruption(CorruptionError::DigestMismatch))
    ));
}

#[cfg(all(feature = "compression-lz4", feature = "encryption-aes-siv"))]
#[test]
fn representation_work_limit_fails_before_target_mutation() {
    let (scope, _) = scope(StorageMethod::Raw, 32);
    let policy = FileStoragePolicy::new(
        StorageMethod::Raw,
        CompressionPolicy::Lz4BlockV1,
        ContentCipher::Aes256SivV1,
    );
    let master = MasterKey::new(MasterKeyId::new([0x61; 16]), [0x62; 32]);
    let mut entropy = FixedEntropy {
        byte: 0x63,
        calls: 0,
        fail: false,
    };
    let candidate = generate_content_metadata(scope, policy, Some(&master), &mut entropy).unwrap();
    let context = open_committed_context(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        candidate.key_commitment().copied(),
        candidate.wrapped_key_bytes(),
        1,
        Some(&master),
    )
    .unwrap();
    let target = MemoryTarget::new();
    let limits = StorageLimits::new(StorageLimitValues {
        max_representation_working_bytes: 1_024,
        ..StorageLimitValues::default()
    })
    .unwrap();
    let repository = ContentRepository::new(
        target.clone(),
        "representation-limit",
        CreationDefaults::new(StorageMethod::Raw),
        limits,
    )
    .unwrap();
    assert!(matches!(
        block_on(repository.prepare_create_with_context(
            &context,
            MutationId::from_u128(3_200),
            0,
            &[0x64; 1_025],
        )),
        Err(StorageError::Limit(error))
            if error.kind == LimitKind::RepresentationWorkingBytes
    ));
    assert!(target.trace().unwrap().is_empty());

    let small_target = MemoryTarget::new();
    let small_limits = StorageLimits::new(StorageLimitValues {
        max_representation_working_bytes: 500,
        ..StorageLimitValues::default()
    })
    .unwrap();
    let small_repository = ContentRepository::new(
        small_target.clone(),
        "representation-small-limit",
        CreationDefaults::new(StorageMethod::Raw),
        small_limits,
    )
    .unwrap();
    assert!(matches!(
        block_on(small_repository.prepare_create_with_context(
            &context,
            MutationId::from_u128(3_202),
            0,
            &[1],
        )),
        Err(StorageError::Limit(error))
            if error.kind == LimitKind::RepresentationWorkingBytes
    ));
    assert!(small_target.trace().unwrap().is_empty());
    assert!(matches!(
        block_on(small_repository.prepare_create_with_context(
            &context,
            MutationId::from_u128(3_203),
            0,
            b"",
        )),
        Err(StorageError::Limit(error))
            if error.kind == LimitKind::RepresentationWorkingBytes
    ));
    assert!(small_target.trace().unwrap().is_empty());

    let source_target = MemoryTarget::new();
    let source = ContentRepository::new(
        source_target.clone(),
        "representation-read-limit",
        CreationDefaults::new(StorageMethod::Raw),
        StorageLimits::default(),
    )
    .unwrap();
    let mut state = 0x9e37_79b9_u32;
    let noisy = (0..10_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect::<Vec<_>>();
    let prepared = block_on(source.prepare_create_with_context(
        &context,
        MutationId::from_u128(3_201),
        0,
        &noisy,
    ))
    .unwrap();
    let manifest =
        block_on(source.load_manifest_with_context(prepared.content(), &context)).unwrap();
    let ManifestLayout::Raw { blob: Some(blob) } = manifest.layout() else {
        panic!("expected bounded raw payload");
    };
    source_target.clear_trace().unwrap();
    let read_limits = StorageLimits::new(StorageLimitValues {
        max_representation_working_bytes: 20_000,
        ..StorageLimitValues::default()
    })
    .unwrap();
    let limited_reader = ContentRepository::new(
        source_target.clone(),
        "representation-read-limit",
        CreationDefaults::new(StorageMethod::BlockSplit),
        read_limits,
    )
    .unwrap();
    assert!(matches!(
        block_on(limited_reader.read_with_context(
            prepared.content(),
            &context,
            0,
            noisy.len(),
        )),
        Err(StorageError::Limit(error))
            if error.kind == LimitKind::RepresentationWorkingBytes
    ));
    assert!(
        source_target
            .trace()
            .unwrap()
            .iter()
            .all(|event| event.key != *blob.key())
    );
}

#[cfg(feature = "compression-lz4")]
#[test]
fn standalone_publisher_accepts_explicit_compression_context() {
    let (scope, _) = scope(StorageMethod::Raw, 35);
    let policy = FileStoragePolicy::new(
        StorageMethod::Raw,
        CompressionPolicy::Lz4BlockV1,
        ContentCipher::None,
    );
    let mut entropy = FixedEntropy {
        byte: 0,
        calls: 0,
        fail: true,
    };
    let candidate = generate_content_metadata(scope, policy, None, &mut entropy).unwrap();
    let context = open_committed_context(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        None,
        None,
        1,
        None,
    )
    .unwrap();
    let target = MemoryTarget::new();
    let repository = ContentRepository::new(
        target,
        "compressed-standalone",
        CreationDefaults::new(StorageMethod::BlockSplit),
        StorageLimits::default(),
    )
    .unwrap();
    let mutation = MutationId::from_u128(3_500);
    let prepared =
        block_on(repository.prepare_create_with_context(&context, mutation, 0, &[b'A'; 1_025]))
            .unwrap();
    assert!(matches!(
        block_on(repository.publisher().create_with_context(
            scope.file_id(),
            &context,
            mutation,
            &prepared,
        )),
        Ok(Publication::Published(_))
    ));
    assert_eq!(
        block_on(
            repository
                .publisher()
                .load_with_context(scope.file_id(), &context)
        )
        .unwrap()
        .unwrap()
        .content(),
        prepared.content()
    );
}

#[cfg(all(feature = "compression-lz4", feature = "encryption-aes-siv"))]
#[test]
fn transformed_payload_page_and_root_failures_leave_the_selected_base_complete() {
    let (scope, _) = scope(StorageMethod::BlockSplit, 34);
    let policy = FileStoragePolicy::new(
        StorageMethod::BlockSplit,
        CompressionPolicy::Lz4BlockV1,
        ContentCipher::Aes256SivV1,
    );
    let master = MasterKey::new(MasterKeyId::new([0x71; 16]), [0x72; 32]);
    let mut entropy = FixedEntropy {
        byte: 0x73,
        calls: 0,
        fail: false,
    };
    let candidate = generate_content_metadata(scope, policy, Some(&master), &mut entropy).unwrap();
    let context = open_committed_context(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        candidate.key_commitment().copied(),
        candidate.wrapped_key_bytes(),
        1,
        Some(&master),
    )
    .unwrap();
    let update_offset = u64::from(w9pt_fs_storage::BLOCK_SIZE) * 16_384;

    let probe_target = MemoryTarget::new();
    let probe = ContentRepository::new(
        probe_target.clone(),
        "transformed-failures",
        CreationDefaults::new(StorageMethod::BlockSplit),
        StorageLimits::default(),
    )
    .unwrap();
    let probe_base = block_on(probe.prepare_create_with_context(
        &context,
        MutationId::from_u128(3_400),
        0,
        b"selected base",
    ))
    .unwrap();
    probe_target.clear_trace().unwrap();
    block_on(probe.prepare_write_with_context(
        probe_base.content(),
        &context,
        MutationId::from_u128(3_401),
        0,
        update_offset,
        &[0x74; 1_025],
    ))
    .unwrap();
    let dependency_keys = probe_target
        .trace()
        .unwrap()
        .into_iter()
        .filter(|event| event.operation == TargetOperation::PutIfAbsent)
        .map(|event| event.key)
        .collect::<Vec<_>>();
    assert!(
        dependency_keys.len() >= 4,
        "expected payload, pages, and root"
    );

    for key in dependency_keys {
        for timing in [FailureTiming::Before, FailureTiming::After] {
            let target = MemoryTarget::new();
            let repository = ContentRepository::new(
                target.clone(),
                "transformed-failures",
                CreationDefaults::new(StorageMethod::Raw),
                StorageLimits::default(),
            )
            .unwrap();
            let base = block_on(repository.prepare_create_with_context(
                &context,
                MutationId::from_u128(3_400),
                0,
                b"selected base",
            ))
            .unwrap();
            target
                .inject_failure_for(TargetOperation::PutIfAbsent, timing, key.clone())
                .unwrap();
            let update = block_on(repository.prepare_write_with_context(
                base.content(),
                &context,
                MutationId::from_u128(3_401),
                0,
                update_offset,
                &[0x74; 1_025],
            ));
            match timing {
                FailureTiming::Before => assert!(update.is_err()),
                FailureTiming::After => assert!(update.is_ok()),
            }
            assert_eq!(
                block_on(repository.read_with_context(
                    base.content(),
                    &context,
                    0,
                    b"selected base".len(),
                ))
                .unwrap(),
                b"selected base"
            );
            assert!(
                block_on(repository.read_with_context(base.content(), &context, update_offset, 1,))
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[cfg(not(feature = "compression-lz4"))]
#[test]
fn unavailable_compression_writer_fails_before_puts_even_for_empty_content() {
    let (scope, _) = scope(StorageMethod::Raw, 33);
    let policy = FileStoragePolicy::new(
        StorageMethod::Raw,
        CompressionPolicy::Lz4BlockV1,
        ContentCipher::None,
    );
    let mut entropy = FixedEntropy {
        byte: 0,
        calls: 0,
        fail: true,
    };
    let candidate = generate_content_metadata(scope, policy, None, &mut entropy).unwrap();
    let context = open_committed_context(
        scope,
        candidate.policy_format(),
        candidate.policy_bytes(),
        None,
        None,
        1,
        None,
    )
    .unwrap();
    let target = MemoryTarget::new();
    let repository = ContentRepository::new(
        target.clone(),
        "unsupported-compression",
        CreationDefaults::new(StorageMethod::Raw),
        StorageLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        block_on(repository.prepare_create_with_context(
            &context,
            MutationId::from_u128(3_300),
            0,
            b"",
        )),
        Err(StorageError::Representation(
            RepresentationError::UnsupportedCompression
        ))
    ));
    assert!(target.trace().unwrap().is_empty());
}

#[cfg(all(feature = "compression-lz4", feature = "encryption-aes-siv"))]
#[test]
fn both_methods_round_trip_all_four_policies_with_deterministic_reopen() {
    for (method_index, method) in [StorageMethod::Raw, StorageMethod::BlockSplit]
        .into_iter()
        .enumerate()
    {
        for (policy_index, (compression, encryption)) in [
            (CompressionPolicy::Identity, ContentCipher::None),
            (CompressionPolicy::Lz4BlockV1, ContentCipher::None),
            (CompressionPolicy::Identity, ContentCipher::Aes256SivV1),
            (CompressionPolicy::Lz4BlockV1, ContentCipher::Aes256SivV1),
        ]
        .into_iter()
        .enumerate()
        {
            let ordinal = u8::try_from(method_index * 4 + policy_index + 10).unwrap();
            let (scope, _) = scope(method, ordinal);
            let policy = FileStoragePolicy::new(method, compression, encryption);
            let master = MasterKey::new(
                MasterKeyId::new([ordinal; 16]),
                [ordinal.wrapping_add(1); 32],
            );
            let mut entropy = FixedEntropy {
                byte: ordinal.wrapping_add(2),
                calls: 0,
                fail: false,
            };
            let candidate = generate_content_metadata(
                scope,
                policy,
                encryption.is_encrypted().then_some(&master),
                &mut entropy,
            )
            .unwrap();
            let context = open_committed_context(
                scope,
                candidate.policy_format(),
                candidate.policy_bytes(),
                candidate.key_commitment().copied(),
                candidate.wrapped_key_bytes(),
                1,
                encryption.is_encrypted().then_some(&master),
            )
            .unwrap();
            let target = MemoryTarget::new();
            let writer = ContentRepository::new(
                target.clone(),
                format!("representation-{ordinal}"),
                CreationDefaults::new(method),
                StorageLimits::default(),
            )
            .unwrap();
            let bytes = vec![b'A'; 65_537];
            let prepared = block_on(writer.prepare_create_with_context(
                &context,
                MutationId::from_u128(u128::from(ordinal) + 1_000),
                0,
                &bytes,
            ))
            .unwrap();
            if encryption.is_encrypted() {
                assert!(
                    target
                        .trace()
                        .unwrap()
                        .iter()
                        .filter(|event| {
                            event.operation == w9pt_fs_storage::TargetOperation::PutIfAbsent
                        })
                        .all(|event| {
                            event.key.as_str().contains("/v3/protected/")
                                && !event
                                    .key
                                    .as_str()
                                    .contains(&format!("{:032x}", u128::from(ordinal) + 1_000))
                        })
                );
            }
            drop(context);
            let reopened_context = open_committed_context(
                scope,
                candidate.policy_format(),
                candidate.policy_bytes(),
                candidate.key_commitment().copied(),
                candidate.wrapped_key_bytes(),
                1,
                encryption.is_encrypted().then_some(&master),
            )
            .unwrap();
            let reader = ContentRepository::new(
                target,
                format!("representation-{ordinal}"),
                CreationDefaults::new(if method == StorageMethod::Raw {
                    StorageMethod::BlockSplit
                } else {
                    StorageMethod::Raw
                }),
                StorageLimits::default(),
            )
            .unwrap();
            assert_eq!(
                block_on(reader.read_with_context(
                    prepared.content(),
                    &reopened_context,
                    0,
                    bytes.len()
                ))
                .unwrap(),
                bytes
            );
        }
    }
}

#[cfg(all(feature = "compression-lz4", feature = "encryption-aes-siv"))]
#[test]
fn committed_context_conformance_covers_all_methods_and_policies() {
    let master = MasterKey::new(MasterKeyId::new([0x51; 16]), [0x52; 32]);
    let mut contexts = Vec::new();
    for (index, method) in [StorageMethod::Raw, StorageMethod::BlockSplit]
        .into_iter()
        .enumerate()
    {
        for (policy_index, (compression, encryption)) in [
            (CompressionPolicy::Identity, ContentCipher::None),
            (CompressionPolicy::Lz4BlockV1, ContentCipher::None),
            (CompressionPolicy::Identity, ContentCipher::Aes256SivV1),
            (CompressionPolicy::Lz4BlockV1, ContentCipher::Aes256SivV1),
        ]
        .into_iter()
        .enumerate()
        {
            let ordinal = u8::try_from(index * 4 + policy_index + 40).unwrap();
            let (scope, _) = scope(method, ordinal);
            let policy = FileStoragePolicy::new(method, compression, encryption);
            let mut entropy = FixedEntropy {
                byte: ordinal,
                calls: 0,
                fail: false,
            };
            let candidate = generate_content_metadata(
                scope,
                policy,
                encryption.is_encrypted().then_some(&master),
                &mut entropy,
            )
            .unwrap();
            contexts.push(
                open_committed_context(
                    scope,
                    candidate.policy_format(),
                    candidate.policy_bytes(),
                    candidate.key_commitment().copied(),
                    candidate.wrapped_key_bytes(),
                    1,
                    encryption.is_encrypted().then_some(&master),
                )
                .unwrap(),
            );
        }
    }
    let target = MemoryTarget::new();
    block_on(
        w9pt_fs_storage::testing::check_repository_context_conformance(
            target.clone(),
            target,
            "committed-context-conformance",
            &contexts,
        ),
    )
    .unwrap();
}
