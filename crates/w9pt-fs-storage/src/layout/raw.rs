//! Bounded whole-file raw layout.

use crate::{
    ContentRef, ContentRepository, Digest, FileId, FormatError, LimitError, LimitKind, MutationId,
    PreparationIdentity, PreparedContent, StorageError, StorageMethod, TargetStore,
    format::{BlobRef, FileManifest, ManifestLayout, encode_manifest},
    layout::LogicalRange,
};

pub(crate) async fn create<S: TargetStore>(
    repository: &ContentRepository<S>,
    file_id: FileId,
    mutation_id: MutationId,
    attempt: u32,
    bytes: &[u8],
) -> Result<PreparedContent, StorageError<S::Error>> {
    let identity = PreparationIdentity::for_create(mutation_id, StorageMethod::Raw, bytes)?;
    check_write_bound(repository, bytes.len())?;
    let logical_size = u64::try_from(bytes.len()).map_err(|_| {
        LimitError::new(
            LimitKind::RawFile,
            u64::MAX,
            repository.limits().max_raw_file_bytes(),
        )
    })?;
    check_raw_bound(repository, logical_size)?;
    let blob = if bytes.is_empty() {
        None
    } else {
        Some(expected_raw_blob(
            repository, file_id, identity, attempt, bytes,
        ))
    };
    let manifest = FileManifest::raw(file_id, 1, logical_size, blob);
    encode_manifest(&manifest, repository.limits()).map_err(StorageError::from)?;
    if !bytes.is_empty() {
        repository
            .store_raw_payload(file_id, identity, attempt, bytes)
            .await?;
    }
    repository
        .prepare_manifest(&manifest, identity, attempt, true)
        .await
}

pub(crate) async fn read<S: TargetStore>(
    repository: &ContentRepository<S>,
    manifest: &FileManifest,
    offset: u64,
    length: usize,
) -> Result<Vec<u8>, StorageError<S::Error>> {
    check_read_bound(repository, length)?;
    let range = LogicalRange::from_usize(offset, length)?.clamp_to_eof(manifest.logical_size());
    if range.is_empty() {
        return Ok(Vec::new());
    }
    let bytes = load_all(repository, manifest).await?;
    let start = usize::try_from(range.start()).map_err(|_| crate::RangeError::LengthConversion)?;
    let end = usize::try_from(range.end()).map_err(|_| crate::RangeError::LengthConversion)?;
    Ok(bytes[start..end].to_vec())
}

pub(crate) async fn write<S: TargetStore>(
    repository: &ContentRepository<S>,
    base: &ContentRef,
    manifest: &FileManifest,
    mutation_id: MutationId,
    attempt: u32,
    offset: u64,
    data: &[u8],
) -> Result<PreparedContent, StorageError<S::Error>> {
    let identity = PreparationIdentity::for_write(mutation_id, base, offset, data)?;
    check_write_bound(repository, data.len())?;
    let range = LogicalRange::from_usize(offset, data.len())?;
    if data.is_empty() {
        return Ok(PreparedContent::new(base.clone(), false, identity, attempt));
    }
    let logical_size = manifest.logical_size().max(range.end());
    check_raw_bound(repository, logical_size)?;
    let result_len = usize::try_from(logical_size).map_err(|_| {
        LimitError::new(
            LimitKind::RawFile,
            logical_size,
            repository.limits().max_raw_file_bytes(),
        )
    })?;
    let start = usize::try_from(range.start()).map_err(|_| crate::RangeError::LengthConversion)?;
    let end = usize::try_from(range.end()).map_err(|_| crate::RangeError::LengthConversion)?;
    let old = load_all(repository, manifest).await?;
    let mut result = old.clone();
    result.resize(result_len, 0);
    result[start..end].copy_from_slice(data);
    if result == old {
        return Ok(PreparedContent::new(base.clone(), false, identity, attempt));
    }

    let generation =
        manifest
            .generation()
            .checked_add(1)
            .ok_or(FormatError::ArithmeticOverflow {
                field: "content generation",
            })?;
    let blob = expected_raw_blob(repository, manifest.file_id(), identity, attempt, &result);
    let result_manifest =
        FileManifest::raw(manifest.file_id(), generation, logical_size, Some(blob));
    encode_manifest(&result_manifest, repository.limits()).map_err(StorageError::from)?;
    repository
        .store_raw_payload(manifest.file_id(), identity, attempt, &result)
        .await?;
    repository
        .prepare_manifest(&result_manifest, identity, attempt, true)
        .await
}

pub(crate) async fn write_from_new<S: TargetStore>(
    repository: &ContentRepository<S>,
    file_id: FileId,
    mutation_id: MutationId,
    attempt: u32,
    offset: u64,
    data: &[u8],
) -> Result<PreparedContent, StorageError<S::Error>> {
    let identity =
        PreparationIdentity::for_write_from_new(mutation_id, StorageMethod::Raw, offset, data)?;
    check_write_bound(repository, data.len())?;
    let range = LogicalRange::from_usize(offset, data.len())?;
    let logical_size = if data.is_empty() { 0 } else { range.end() };
    check_raw_bound(repository, logical_size)?;
    let result_len = usize::try_from(logical_size).map_err(|_| {
        LimitError::new(
            LimitKind::RawFile,
            logical_size,
            repository.limits().max_raw_file_bytes(),
        )
    })?;
    let mut result = vec![0; result_len];
    if !data.is_empty() {
        let start =
            usize::try_from(range.start()).map_err(|_| crate::RangeError::LengthConversion)?;
        let end = usize::try_from(range.end()).map_err(|_| crate::RangeError::LengthConversion)?;
        result[start..end].copy_from_slice(data);
    }
    let blob = (!result.is_empty())
        .then(|| expected_raw_blob(repository, file_id, identity, attempt, &result));
    let manifest = FileManifest::raw(file_id, 1, logical_size, blob);
    encode_manifest(&manifest, repository.limits()).map_err(StorageError::from)?;
    if !result.is_empty() {
        repository
            .store_raw_payload(file_id, identity, attempt, &result)
            .await?;
    }
    repository
        .prepare_manifest(&manifest, identity, attempt, true)
        .await
}

pub(crate) async fn truncate<S: TargetStore>(
    repository: &ContentRepository<S>,
    base: &ContentRef,
    manifest: &FileManifest,
    mutation_id: MutationId,
    attempt: u32,
    logical_size: u64,
) -> Result<PreparedContent, StorageError<S::Error>> {
    let identity = PreparationIdentity::for_truncate(mutation_id, base, logical_size);
    check_raw_bound(repository, logical_size)?;
    if logical_size == manifest.logical_size() {
        return Ok(PreparedContent::new(base.clone(), false, identity, attempt));
    }
    let result_len = usize::try_from(logical_size).map_err(|_| {
        LimitError::new(
            LimitKind::RawFile,
            logical_size,
            repository.limits().max_raw_file_bytes(),
        )
    })?;
    let mut result = load_all(repository, manifest).await?;
    result.resize(result_len, 0);
    let generation =
        manifest
            .generation()
            .checked_add(1)
            .ok_or(FormatError::ArithmeticOverflow {
                field: "content generation",
            })?;
    let blob = if result.is_empty() {
        None
    } else {
        Some(expected_raw_blob(
            repository,
            manifest.file_id(),
            identity,
            attempt,
            &result,
        ))
    };
    let result_manifest = FileManifest::raw(manifest.file_id(), generation, logical_size, blob);
    encode_manifest(&result_manifest, repository.limits()).map_err(StorageError::from)?;
    if !result.is_empty() {
        repository
            .store_raw_payload(manifest.file_id(), identity, attempt, &result)
            .await?;
    }
    repository
        .prepare_manifest(&result_manifest, identity, attempt, true)
        .await
}

pub(crate) async fn truncate_from_new<S: TargetStore>(
    repository: &ContentRepository<S>,
    file_id: FileId,
    mutation_id: MutationId,
    attempt: u32,
    logical_size: u64,
) -> Result<PreparedContent, StorageError<S::Error>> {
    let identity =
        PreparationIdentity::for_truncate_from_new(mutation_id, StorageMethod::Raw, logical_size);
    check_raw_bound(repository, logical_size)?;
    let result_len = usize::try_from(logical_size).map_err(|_| {
        LimitError::new(
            LimitKind::RawFile,
            logical_size,
            repository.limits().max_raw_file_bytes(),
        )
    })?;
    let result = vec![0; result_len];
    let blob = (!result.is_empty())
        .then(|| expected_raw_blob(repository, file_id, identity, attempt, &result));
    let manifest = FileManifest::raw(file_id, 1, logical_size, blob);
    encode_manifest(&manifest, repository.limits()).map_err(StorageError::from)?;
    if !result.is_empty() {
        repository
            .store_raw_payload(file_id, identity, attempt, &result)
            .await?;
    }
    repository
        .prepare_manifest(&manifest, identity, attempt, true)
        .await
}

fn expected_raw_blob<S: TargetStore>(
    repository: &ContentRepository<S>,
    file_id: FileId,
    identity: PreparationIdentity,
    attempt: u32,
    bytes: &[u8],
) -> BlobRef {
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    BlobRef::new(
        repository.keys().raw_payload(file_id, identity, attempt),
        length,
        length,
        Digest::blake3(bytes),
    )
}

async fn load_all<S: TargetStore>(
    repository: &ContentRepository<S>,
    manifest: &FileManifest,
) -> Result<Vec<u8>, StorageError<S::Error>> {
    check_raw_bound(repository, manifest.logical_size())?;
    let blob = match manifest.layout() {
        ManifestLayout::Raw { blob: Some(blob) } => blob,
        ManifestLayout::Raw { blob: None } if manifest.logical_size() == 0 => {
            return Ok(Vec::new());
        }
        _ => {
            return Err(FormatError::NonCanonical {
                field: "raw payload",
            }
            .into());
        }
    };
    let bytes = repository.load_payload(blob).await?;
    let expected = usize::try_from(manifest.logical_size()).map_err(|_| {
        LimitError::new(
            LimitKind::RawFile,
            manifest.logical_size(),
            repository.limits().max_raw_file_bytes(),
        )
    })?;
    if bytes.len() != expected {
        return Err(crate::CorruptionError::InvalidLength {
            field: "raw plaintext",
            expected: manifest.logical_size(),
            actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        }
        .into());
    }
    Ok(bytes)
}

fn check_raw_bound<S: TargetStore>(
    repository: &ContentRepository<S>,
    actual: u64,
) -> Result<(), LimitError> {
    let limit = repository.limits().max_raw_file_bytes();
    if actual > limit {
        Err(LimitError::new(LimitKind::RawFile, actual, limit))
    } else {
        Ok(())
    }
}

fn check_read_bound<S: TargetStore>(
    repository: &ContentRepository<S>,
    actual: usize,
) -> Result<(), LimitError> {
    let limit = repository.limits().max_read_bytes();
    if actual > limit {
        Err(LimitError::new(
            LimitKind::Read,
            u64::try_from(actual).unwrap_or(u64::MAX),
            u64::try_from(limit).unwrap_or(u64::MAX),
        ))
    } else {
        Ok(())
    }
}

fn check_write_bound<S: TargetStore>(
    repository: &ContentRepository<S>,
    actual: usize,
) -> Result<(), LimitError> {
    let limit = repository.limits().max_write_bytes();
    if actual > limit {
        Err(LimitError::new(
            LimitKind::Write,
            u64::try_from(actual).unwrap_or(u64::MAX),
            u64::try_from(limit).unwrap_or(u64::MAX),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CreationDefaults, StorageLimits, StorageMethod,
        testing::{MemoryTarget, block_on},
    };

    fn repository(target: MemoryTarget) -> ContentRepository<MemoryTarget> {
        ContentRepository::new(
            target,
            "private",
            CreationDefaults::new(StorageMethod::Raw),
            StorageLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn empty_and_nonempty_raw_content_reopens_from_target_only() {
        let target = MemoryTarget::new();
        let first = repository(target.clone());
        let empty = block_on(create(
            &first,
            FileId::from_u128(1),
            MutationId::from_u128(1),
            0,
            b"",
        ))
        .unwrap();
        let nonempty = block_on(create(
            &first,
            FileId::from_u128(2),
            MutationId::from_u128(2),
            0,
            b"abcdef",
        ))
        .unwrap();
        drop(first);

        let reopened = repository(target);
        let empty_manifest = block_on(reopened.load_manifest(empty.content())).unwrap();
        let manifest = block_on(reopened.load_manifest(nonempty.content())).unwrap();
        assert_eq!(
            block_on(read(&reopened, &empty_manifest, 0, 10)).unwrap(),
            b""
        );
        assert_eq!(
            block_on(read(&reopened, &manifest, 2, 99)).unwrap(),
            b"cdef"
        );
        assert_eq!(block_on(read(&reopened, &manifest, 6, 1)).unwrap(), b"");
    }

    #[test]
    fn positioned_write_zero_fills_gap_and_reuses_unchanged_content() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let file_id = FileId::from_u128(1);
        let initial = block_on(create(
            &repository,
            file_id,
            MutationId::from_u128(1),
            0,
            b"abc",
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(initial.content())).unwrap();
        let changed = block_on(write(
            &repository,
            initial.content(),
            &manifest,
            MutationId::from_u128(2),
            0,
            5,
            b"xy",
        ))
        .unwrap();
        let changed_manifest = block_on(repository.load_manifest(changed.content())).unwrap();
        assert_eq!(
            block_on(read(&repository, &changed_manifest, 0, 99)).unwrap(),
            b"abc\0\0xy"
        );

        target.clear_trace().unwrap();
        let unchanged = block_on(write(
            &repository,
            changed.content(),
            &changed_manifest,
            MutationId::from_u128(3),
            0,
            5,
            b"xy",
        ))
        .unwrap();
        assert!(!unchanged.content_changed());
        assert_eq!(unchanged.content(), changed.content());
        assert!(
            target
                .trace()
                .unwrap()
                .iter()
                .all(|event| event.operation != crate::TargetOperation::PutIfAbsent)
        );
    }

    #[test]
    fn overflowing_write_fails_before_target_work() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let initial = block_on(create(
            &repository,
            FileId::from_u128(1),
            MutationId::from_u128(1),
            0,
            b"",
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(initial.content())).unwrap();
        target.clear_trace().unwrap();
        assert!(matches!(
            block_on(write(
                &repository,
                initial.content(),
                &manifest,
                MutationId::from_u128(2),
                0,
                u64::MAX,
                b"x",
            )),
            Err(StorageError::Range(_))
        ));
        assert!(target.trace().unwrap().is_empty());
    }

    #[test]
    fn truncate_shrink_extend_and_zero_never_resurrect_bytes() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let file_id = FileId::from_u128(1);
        let created = block_on(create(
            &repository,
            file_id,
            MutationId::from_u128(1),
            0,
            b"abcdef",
        ))
        .unwrap();
        let original = block_on(repository.load_manifest(created.content())).unwrap();
        let shrunk = block_on(truncate(
            &repository,
            created.content(),
            &original,
            MutationId::from_u128(2),
            0,
            3,
        ))
        .unwrap();
        let shrunk_manifest = block_on(repository.load_manifest(shrunk.content())).unwrap();
        assert_eq!(
            block_on(read(&repository, &shrunk_manifest, 0, 9)).unwrap(),
            b"abc"
        );

        let extended = block_on(truncate(
            &repository,
            shrunk.content(),
            &shrunk_manifest,
            MutationId::from_u128(3),
            0,
            6,
        ))
        .unwrap();
        let extended_manifest = block_on(repository.load_manifest(extended.content())).unwrap();
        assert_eq!(
            block_on(read(&repository, &extended_manifest, 0, 9)).unwrap(),
            b"abc\0\0\0"
        );

        let zero = block_on(truncate(
            &repository,
            extended.content(),
            &extended_manifest,
            MutationId::from_u128(4),
            0,
            0,
        ))
        .unwrap();
        let zero_manifest = block_on(repository.load_manifest(zero.content())).unwrap();
        assert!(matches!(
            zero_manifest.layout(),
            ManifestLayout::Raw { blob: None }
        ));

        target.clear_trace().unwrap();
        let unchanged = block_on(truncate(
            &repository,
            zero.content(),
            &zero_manifest,
            MutationId::from_u128(5),
            0,
            0,
        ))
        .unwrap();
        assert!(!unchanged.content_changed());
        assert!(target.trace().unwrap().is_empty());
    }

    #[test]
    fn deterministic_operation_trace_matches_byte_vector_model() {
        let target = MemoryTarget::new();
        let repository = repository(target);
        let file_id = FileId::from_u128(1);
        let mut reference = block_on(create(
            &repository,
            file_id,
            MutationId::from_u128(1),
            0,
            b"",
        ))
        .unwrap()
        .into_content();
        let mut model = Vec::<u8>::new();
        let mut state = 0x1234_5678_9abc_def0_u64;

        for step in 1..=200_u128 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let manifest = block_on(repository.load_manifest(&reference)).unwrap();
            let prepared = if step % 4 == 0 {
                let size = usize::try_from((state >> 16) % 80).unwrap();
                model.resize(size, 0);
                block_on(truncate(
                    &repository,
                    &reference,
                    &manifest,
                    MutationId::from_u128(step + 1),
                    0,
                    u64::try_from(size).unwrap(),
                ))
                .unwrap()
            } else {
                let offset = usize::try_from((state >> 24) % 70).unwrap();
                let length = usize::try_from((state >> 40) % 11).unwrap();
                let data = (0..length)
                    .map(|index| state.wrapping_add(index as u64) as u8)
                    .collect::<Vec<_>>();
                if !data.is_empty() {
                    let end = offset + data.len();
                    if end > model.len() {
                        model.resize(end, 0);
                    }
                    model[offset..end].copy_from_slice(&data);
                }
                block_on(write(
                    &repository,
                    &reference,
                    &manifest,
                    MutationId::from_u128(step + 1),
                    0,
                    u64::try_from(offset).unwrap(),
                    &data,
                ))
                .unwrap()
            };
            reference = prepared.into_content();
            let manifest = block_on(repository.load_manifest(&reference)).unwrap();
            assert_eq!(manifest.logical_size(), model.len() as u64);
            assert_eq!(
                block_on(read(&repository, &manifest, 0, model.len() + 1)).unwrap(),
                model,
                "model mismatch at deterministic step {step}"
            );
        }
    }

    #[test]
    fn raw_limits_and_corruption_are_typed() {
        let target = MemoryTarget::new();
        let values = crate::StorageLimitValues {
            max_raw_file_bytes: 8,
            max_write_bytes: 8,
            ..crate::StorageLimitValues::default()
        };
        let repository = ContentRepository::new(
            target.clone(),
            "private",
            CreationDefaults::new(StorageMethod::Raw),
            StorageLimits::new(values).unwrap(),
        )
        .unwrap();
        let created = block_on(create(
            &repository,
            FileId::from_u128(1),
            MutationId::from_u128(1),
            0,
            b"12345678",
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(created.content())).unwrap();

        target.clear_trace().unwrap();
        assert!(matches!(
            block_on(write(
                &repository,
                created.content(),
                &manifest,
                MutationId::from_u128(2),
                0,
                8,
                b"x",
            )),
            Err(StorageError::Limit(LimitError {
                kind: LimitKind::RawFile,
                ..
            }))
        ));
        assert!(target.trace().unwrap().is_empty());

        let ManifestLayout::Raw { blob: Some(blob) } = manifest.layout() else {
            panic!("nonempty raw manifest must have a blob");
        };
        let mut bytes = target.inspect(blob.key()).unwrap().unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        assert!(target.corrupt(blob.key(), bytes).unwrap());
        assert!(matches!(
            block_on(read(&repository, &manifest, 0, 8)),
            Err(StorageError::Corruption(_))
        ));
    }

    #[test]
    fn immutable_put_failures_leave_prepared_base_readable_and_retryable() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let file_id = FileId::from_u128(1);
        let created = block_on(create(
            &repository,
            file_id,
            MutationId::from_u128(1),
            0,
            b"old",
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(created.content())).unwrap();
        target
            .inject_failure(
                crate::TargetOperation::PutIfAbsent,
                crate::testing::FailureTiming::Before,
            )
            .unwrap();
        assert!(matches!(
            block_on(write(
                &repository,
                created.content(),
                &manifest,
                MutationId::from_u128(2),
                0,
                0,
                b"new",
            )),
            Err(StorageError::Target(_))
        ));
        assert_eq!(
            block_on(read(&repository, &manifest, 0, 9)).unwrap(),
            b"old"
        );

        let retry_mutation = MutationId::from_u128(3);
        target
            .inject_failure(
                crate::TargetOperation::PutIfAbsent,
                crate::testing::FailureTiming::After,
            )
            .unwrap();
        target
            .inject_failure(
                crate::TargetOperation::Get,
                crate::testing::FailureTiming::Before,
            )
            .unwrap();
        assert!(
            block_on(create(
                &repository,
                FileId::from_u128(2),
                retry_mutation,
                0,
                b""
            ))
            .is_err()
        );
        let retried = block_on(create(
            &repository,
            FileId::from_u128(2),
            retry_mutation,
            0,
            b"",
        ))
        .unwrap();
        block_on(repository.validate_content(retried.content())).unwrap();
    }
}
