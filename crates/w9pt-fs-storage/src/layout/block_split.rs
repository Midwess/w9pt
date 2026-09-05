//! Sparse fixed-size block-split layout.

use std::collections::BTreeMap;

use crate::{
    BLOCK_SIZE_V1, ContentRef, ContentRepository, Digest, FileId, FormatError, LimitError,
    LimitKind, MutationId, PreparationIdentity, PreparedContent, StorageError, StorageMethod,
    TargetStore,
    format::{BlobRef, BlockEntry, FileManifest, ManifestLayout, encode_manifest},
    layout::{BlockSpans, LogicalRange},
};

pub(crate) async fn create<S: TargetStore>(
    repository: &ContentRepository<S>,
    file_id: FileId,
    mutation_id: MutationId,
    attempt: u32,
    bytes: &[u8],
) -> Result<PreparedContent, StorageError<S::Error>> {
    let identity = PreparationIdentity::for_create(mutation_id, StorageMethod::BlockSplit, bytes)?;
    check_write_bound(repository, bytes.len())?;
    let logical_size =
        u64::try_from(bytes.len()).map_err(|_| crate::RangeError::LengthConversion)?;
    let nonzero_blocks = bytes
        .chunks(BLOCK_SIZE_V1 as usize)
        .filter(|chunk| chunk.iter().any(|byte| *byte != 0))
        .count();
    check_block_count(repository, nonzero_blocks)?;

    let mut blocks = Vec::with_capacity(nonzero_blocks);
    let mut stores = Vec::with_capacity(nonzero_blocks);
    for (index, chunk) in bytes.chunks(BLOCK_SIZE_V1 as usize).enumerate() {
        if chunk.iter().all(|byte| *byte == 0) {
            continue;
        }
        let mut canonical = vec![0; BLOCK_SIZE_V1 as usize];
        canonical[..chunk.len()].copy_from_slice(chunk);
        let index = u64::try_from(index).map_err(|_| FormatError::ArithmeticOverflow {
            field: "block index",
        })?;
        let blob = expected_block_blob(repository, file_id, identity, attempt, index, &canonical);
        blocks.push(BlockEntry::new(index, blob));
        stores.push((index, canonical));
    }
    let manifest = FileManifest::block_split(file_id, 1, logical_size, blocks);
    encode_manifest(&manifest, repository.limits()).map_err(StorageError::from)?;
    for (index, canonical) in stores {
        repository
            .store_block_payload(file_id, identity, attempt, index, &canonical)
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
    let output_len =
        usize::try_from(range.len()).map_err(|_| crate::RangeError::LengthConversion)?;
    let mut output = vec![0; output_len];
    if range.is_empty() {
        return Ok(output);
    }
    let ManifestLayout::BlockSplit { blocks, .. } = manifest.layout() else {
        return Err(FormatError::NonCanonical {
            field: "block-split layout",
        }
        .into());
    };

    for span in BlockSpans::new(range)? {
        let Ok(position) = blocks.binary_search_by_key(&span.block_index(), BlockEntry::index)
        else {
            continue;
        };
        let block = repository.load_payload(blocks[position].blob()).await?;
        if block.len() != BLOCK_SIZE_V1 as usize {
            return Err(crate::CorruptionError::InvalidLength {
                field: "block plaintext",
                expected: u64::from(BLOCK_SIZE_V1),
                actual: u64::try_from(block.len()).unwrap_or(u64::MAX),
            }
            .into());
        }
        validate_final_padding(manifest, span.block_index(), &block)?;
        let source_start = span.within_block() as usize;
        let span_len = span.len() as usize;
        let source_end = source_start + span_len;
        let destination_start = span.buffer_offset();
        let destination_end = destination_start + span_len;
        output[destination_start..destination_end]
            .copy_from_slice(&block[source_start..source_end]);
    }
    Ok(output)
}

enum PlannedBlock {
    Hole,
    Reuse(BlobRef),
    Store(Vec<u8>),
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
    let ManifestLayout::BlockSplit { blocks, .. } = manifest.layout() else {
        return Err(FormatError::NonCanonical {
            field: "block-split layout",
        }
        .into());
    };
    let old_blocks = blocks
        .iter()
        .map(|entry| (entry.index(), entry.blob().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut planned = BTreeMap::<u64, PlannedBlock>::new();
    let mut content_changed = range.end() > manifest.logical_size();

    for span in BlockSpans::new(range)? {
        let old = old_blocks.get(&span.block_index());
        let input_start = span.buffer_offset();
        let input_end = input_start + span.len() as usize;
        let mut block = if span.is_full_block() {
            data[input_start..input_end].to_vec()
        } else if let Some(old) = old {
            let block = repository.load_payload(old).await?;
            validate_final_padding(manifest, span.block_index(), &block)?;
            block
        } else {
            vec![0; BLOCK_SIZE_V1 as usize]
        };
        if block.len() != BLOCK_SIZE_V1 as usize {
            return Err(crate::CorruptionError::InvalidLength {
                field: "block plaintext",
                expected: u64::from(BLOCK_SIZE_V1),
                actual: u64::try_from(block.len()).unwrap_or(u64::MAX),
            }
            .into());
        }
        if !span.is_full_block() {
            let block_start = span.within_block() as usize;
            let block_end = block_start + span.len() as usize;
            block[block_start..block_end].copy_from_slice(&data[input_start..input_end]);
        }

        let plan = if block.iter().all(|byte| *byte == 0) {
            if old.is_some() {
                content_changed = true;
            }
            PlannedBlock::Hole
        } else {
            let digest = Digest::blake3(&block);
            if let Some(old) = old.filter(|old| old.digest() == digest) {
                PlannedBlock::Reuse(old.clone())
            } else {
                content_changed = true;
                PlannedBlock::Store(block)
            }
        };
        planned.insert(span.block_index(), plan);
    }

    let removed = planned
        .iter()
        .filter(|(index, plan)| {
            old_blocks.contains_key(index) && matches!(plan, PlannedBlock::Hole)
        })
        .count();
    let added = planned
        .iter()
        .filter(|(index, plan)| {
            !old_blocks.contains_key(index) && !matches!(plan, PlannedBlock::Hole)
        })
        .count();
    let result_count = old_blocks
        .len()
        .checked_sub(removed)
        .and_then(|count| count.checked_add(added))
        .ok_or(FormatError::ArithmeticOverflow {
            field: "block entry count",
        })?;
    check_block_count(repository, result_count)?;

    if !content_changed {
        return Ok(PreparedContent::new(base.clone(), false, identity, attempt));
    }
    let mut result_blocks = old_blocks;
    let mut stores = Vec::<(u64, Vec<u8>)>::new();
    for (index, plan) in planned {
        match plan {
            PlannedBlock::Hole => {
                result_blocks.remove(&index);
            }
            PlannedBlock::Reuse(blob) => {
                result_blocks.insert(index, blob);
            }
            PlannedBlock::Store(block) => {
                let blob = expected_block_blob(
                    repository,
                    manifest.file_id(),
                    identity,
                    attempt,
                    index,
                    &block,
                );
                result_blocks.insert(index, blob);
                stores.push((index, block));
            }
        }
    }
    let generation =
        manifest
            .generation()
            .checked_add(1)
            .ok_or(FormatError::ArithmeticOverflow {
                field: "content generation",
            })?;
    let result_manifest = FileManifest::block_split(
        manifest.file_id(),
        generation,
        manifest.logical_size().max(range.end()),
        result_blocks
            .into_iter()
            .map(|(index, blob)| BlockEntry::new(index, blob))
            .collect(),
    );
    encode_manifest(&result_manifest, repository.limits()).map_err(StorageError::from)?;
    for (index, block) in stores {
        repository
            .store_block_payload(manifest.file_id(), identity, attempt, index, &block)
            .await?;
    }
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
    let identity = PreparationIdentity::for_write_from_new(
        mutation_id,
        StorageMethod::BlockSplit,
        offset,
        data,
    )?;
    check_write_bound(repository, data.len())?;
    let range = LogicalRange::from_usize(offset, data.len())?;
    let logical_size = if data.is_empty() { 0 } else { range.end() };
    let mut blocks = Vec::new();
    let mut stores = Vec::new();
    if !data.is_empty() {
        for span in BlockSpans::new(range)? {
            let input_start = span.buffer_offset();
            let input_end = input_start + span.len() as usize;
            let mut block = vec![0; BLOCK_SIZE_V1 as usize];
            let block_start = span.within_block() as usize;
            let block_end = block_start + span.len() as usize;
            block[block_start..block_end].copy_from_slice(&data[input_start..input_end]);
            if block.iter().all(|byte| *byte == 0) {
                continue;
            }
            let index = span.block_index();
            let blob = expected_block_blob(repository, file_id, identity, attempt, index, &block);
            blocks.push(BlockEntry::new(index, blob));
            stores.push((index, block));
        }
    }
    check_block_count(repository, blocks.len())?;
    let manifest = FileManifest::block_split(file_id, 1, logical_size, blocks);
    encode_manifest(&manifest, repository.limits()).map_err(StorageError::from)?;
    for (index, block) in stores {
        repository
            .store_block_payload(file_id, identity, attempt, index, &block)
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
    if logical_size == manifest.logical_size() {
        return Ok(PreparedContent::new(base.clone(), false, identity, attempt));
    }
    let ManifestLayout::BlockSplit { blocks, .. } = manifest.layout() else {
        return Err(FormatError::NonCanonical {
            field: "block-split layout",
        }
        .into());
    };
    let mut result = blocks
        .iter()
        .map(|entry| (entry.index(), entry.blob().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut tail_store = None::<(u64, Vec<u8>)>;
    let block_size = u64::from(BLOCK_SIZE_V1);
    let old_final_index = manifest.logical_size() / block_size;
    let old_final_block = if !manifest.logical_size().is_multiple_of(block_size) {
        if let Some(old) = result.get(&old_final_index) {
            let block = repository.load_payload(old).await?;
            validate_block_length(&block)?;
            validate_final_padding(manifest, old_final_index, &block)?;
            Some((old_final_index, block))
        } else {
            None
        }
    } else {
        None
    };

    if logical_size < manifest.logical_size() {
        let first_discarded =
            logical_size / block_size + u64::from(!logical_size.is_multiple_of(block_size));
        result.retain(|index, _| *index < first_discarded);

        let visible_tail = logical_size % block_size;
        let final_index = logical_size / block_size;
        if visible_tail != 0
            && let Some(old) = result.get(&final_index).cloned()
        {
            let mut block = match &old_final_block {
                Some((index, block)) if *index == final_index => block.clone(),
                _ => repository.load_payload(&old).await?,
            };
            validate_block_length(&block)?;
            validate_final_padding(manifest, final_index, &block)?;
            let tail =
                usize::try_from(visible_tail).map_err(|_| crate::RangeError::LengthConversion)?;
            block[tail..].fill(0);
            if block.iter().all(|byte| *byte == 0) {
                result.remove(&final_index);
            } else if Digest::blake3(&block) != old.digest() {
                let blob = expected_block_blob(
                    repository,
                    manifest.file_id(),
                    identity,
                    attempt,
                    final_index,
                    &block,
                );
                result.insert(final_index, blob);
                tail_store = Some((final_index, block));
            }
        }
    }

    check_block_count(repository, result.len())?;
    let generation =
        manifest
            .generation()
            .checked_add(1)
            .ok_or(FormatError::ArithmeticOverflow {
                field: "content generation",
            })?;
    let result_manifest = FileManifest::block_split(
        manifest.file_id(),
        generation,
        logical_size,
        result
            .into_iter()
            .map(|(index, blob)| BlockEntry::new(index, blob))
            .collect(),
    );
    encode_manifest(&result_manifest, repository.limits()).map_err(StorageError::from)?;
    if let Some((index, block)) = tail_store {
        repository
            .store_block_payload(manifest.file_id(), identity, attempt, index, &block)
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
    let identity = PreparationIdentity::for_truncate_from_new(
        mutation_id,
        StorageMethod::BlockSplit,
        logical_size,
    );
    let manifest = FileManifest::block_split(file_id, 1, logical_size, Vec::new());
    encode_manifest(&manifest, repository.limits()).map_err(StorageError::from)?;
    repository
        .prepare_manifest(&manifest, identity, attempt, true)
        .await
}

fn validate_block_length<E>(block: &[u8]) -> Result<(), StorageError<E>> {
    if block.len() == BLOCK_SIZE_V1 as usize {
        Ok(())
    } else {
        Err(crate::CorruptionError::InvalidLength {
            field: "block plaintext",
            expected: u64::from(BLOCK_SIZE_V1),
            actual: u64::try_from(block.len()).unwrap_or(u64::MAX),
        }
        .into())
    }
}

fn expected_block_blob<S: TargetStore>(
    repository: &ContentRepository<S>,
    file_id: FileId,
    identity: PreparationIdentity,
    attempt: u32,
    block_index: u64,
    block: &[u8],
) -> BlobRef {
    BlobRef::new(
        repository
            .keys()
            .block_payload(file_id, identity, attempt, block_index),
        u64::from(BLOCK_SIZE_V1),
        u64::from(BLOCK_SIZE_V1),
        Digest::blake3(block),
    )
}

fn validate_final_padding<E>(
    manifest: &FileManifest,
    block_index: u64,
    block: &[u8],
) -> Result<(), StorageError<E>> {
    let block_size = u64::from(BLOCK_SIZE_V1);
    let final_index = manifest.logical_size() / block_size;
    let visible = manifest.logical_size() % block_size;
    if visible == 0 || block_index != final_index {
        return Ok(());
    }
    let visible = usize::try_from(visible).map_err(|_| crate::RangeError::LengthConversion)?;
    if block
        .get(visible..)
        .is_some_and(|tail| tail.iter().any(|byte| *byte != 0))
    {
        Err(crate::CorruptionError::NonZeroPadding.into())
    } else {
        Ok(())
    }
}

fn check_block_count<S: TargetStore>(
    repository: &ContentRepository<S>,
    actual: usize,
) -> Result<(), LimitError> {
    let limit = repository.limits().max_blocks();
    if u64::try_from(actual).unwrap_or(u64::MAX) > u64::from(limit) {
        Err(LimitError::new(
            LimitKind::BlockCount,
            u64::try_from(actual).unwrap_or(u64::MAX),
            u64::from(limit),
        ))
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
            CreationDefaults::new(StorageMethod::BlockSplit),
            StorageLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn sparse_blocks_assemble_in_order_and_slice_at_eof() {
        let target = MemoryTarget::new();
        let repository = repository(target);
        let block = BLOCK_SIZE_V1 as usize;
        let mut bytes = vec![0; block * 2 + 3];
        bytes[block - 1] = 1;
        bytes[block * 2..].copy_from_slice(b"end");
        let prepared = block_on(create(
            &repository,
            FileId::from_u128(1),
            MutationId::from_u128(1),
            0,
            &bytes,
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(prepared.content())).unwrap();
        let ManifestLayout::BlockSplit { blocks, .. } = manifest.layout() else {
            panic!("expected block-split manifest");
        };
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].index(), 0);
        assert_eq!(blocks[1].index(), 2);
        assert_eq!(
            block_on(read(&repository, &manifest, (block - 2) as u64, block + 9)).unwrap(),
            bytes[block - 2..].to_vec()
        );
    }

    #[test]
    fn full_overwrite_skips_old_read_while_partial_write_verifies_it() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let block = BLOCK_SIZE_V1 as usize;
        let initial = block_on(create(
            &repository,
            FileId::from_u128(1),
            MutationId::from_u128(1),
            0,
            &vec![1; block],
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(initial.content())).unwrap();

        target.clear_trace().unwrap();
        let full = block_on(write(
            &repository,
            initial.content(),
            &manifest,
            MutationId::from_u128(2),
            0,
            0,
            &vec![2; block],
        ))
        .unwrap();
        assert!(
            target
                .trace()
                .unwrap()
                .iter()
                .all(|event| event.operation != crate::TargetOperation::Get)
        );

        let full_manifest = block_on(repository.load_manifest(full.content())).unwrap();
        target.clear_trace().unwrap();
        let partial = block_on(write(
            &repository,
            full.content(),
            &full_manifest,
            MutationId::from_u128(3),
            0,
            7,
            b"x",
        ))
        .unwrap();
        assert!(
            target
                .trace()
                .unwrap()
                .iter()
                .any(|event| event.operation == crate::TargetOperation::Get)
        );
        let partial_manifest = block_on(repository.load_manifest(partial.content())).unwrap();
        let result = block_on(read(&repository, &partial_manifest, 0, block)).unwrap();
        assert_eq!(result[7], b'x');
        assert!(result[..7].iter().all(|byte| *byte == 2));
        assert!(result[8..].iter().all(|byte| *byte == 2));
    }

    #[test]
    fn unchanged_blocks_are_reused_and_zero_blocks_become_holes() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let block = BLOCK_SIZE_V1 as usize;
        let initial = block_on(create(
            &repository,
            FileId::from_u128(1),
            MutationId::from_u128(1),
            0,
            &vec![7; block],
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(initial.content())).unwrap();

        target.clear_trace().unwrap();
        let unchanged = block_on(write(
            &repository,
            initial.content(),
            &manifest,
            MutationId::from_u128(2),
            0,
            11,
            &[7; 10],
        ))
        .unwrap();
        assert!(!unchanged.content_changed());
        assert!(
            target
                .trace()
                .unwrap()
                .iter()
                .all(|event| event.operation != crate::TargetOperation::PutIfAbsent)
        );

        let hole = block_on(write(
            &repository,
            initial.content(),
            &manifest,
            MutationId::from_u128(3),
            0,
            0,
            &vec![0; block],
        ))
        .unwrap();
        let hole_manifest = block_on(repository.load_manifest(hole.content())).unwrap();
        let ManifestLayout::BlockSplit { blocks, .. } = hole_manifest.layout() else {
            panic!("expected block-split manifest");
        };
        assert!(blocks.is_empty());
        assert_eq!(
            block_on(read(&repository, &hole_manifest, 0, block)).unwrap(),
            vec![0; block]
        );
    }

    #[test]
    fn final_blocks_are_zero_padded_and_sparse_gaps_stay_absent() {
        let target = MemoryTarget::new();
        let repository = repository(target);
        let block = u64::from(BLOCK_SIZE_V1);
        let initial = block_on(create(
            &repository,
            FileId::from_u128(1),
            MutationId::from_u128(1),
            0,
            b"abc",
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(initial.content())).unwrap();
        let ManifestLayout::BlockSplit { blocks, .. } = manifest.layout() else {
            panic!("expected block-split manifest");
        };
        let canonical = block_on(repository.load_payload(blocks[0].blob())).unwrap();
        assert_eq!(&canonical[..3], b"abc");
        assert!(canonical[3..].iter().all(|byte| *byte == 0));

        let extended = block_on(write(
            &repository,
            initial.content(),
            &manifest,
            MutationId::from_u128(2),
            0,
            block * 3 + 5,
            b"x",
        ))
        .unwrap();
        let extended_manifest = block_on(repository.load_manifest(extended.content())).unwrap();
        let ManifestLayout::BlockSplit { blocks, .. } = extended_manifest.layout() else {
            panic!("expected block-split manifest");
        };
        assert_eq!(
            blocks.iter().map(BlockEntry::index).collect::<Vec<_>>(),
            vec![0, 3]
        );
        let gap = block_on(read(
            &repository,
            &extended_manifest,
            3,
            usize::try_from(block * 3 + 3).unwrap(),
        ))
        .unwrap();
        assert!(gap[..gap.len() - 1].iter().all(|byte| *byte == 0));
        assert_eq!(*gap.last().unwrap(), b'x');
    }

    #[test]
    fn shrink_zeroes_retained_tail_and_extension_does_not_restore_it() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let block = BLOCK_SIZE_V1 as usize;
        let mut initial_bytes = vec![9; block + 7];
        initial_bytes[..3].copy_from_slice(b"abc");
        let initial = block_on(create(
            &repository,
            FileId::from_u128(1),
            MutationId::from_u128(1),
            0,
            &initial_bytes,
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(initial.content())).unwrap();
        let shrunk = block_on(truncate(
            &repository,
            initial.content(),
            &manifest,
            MutationId::from_u128(2),
            0,
            3,
        ))
        .unwrap();
        let shrunk_manifest = block_on(repository.load_manifest(shrunk.content())).unwrap();
        let ManifestLayout::BlockSplit { blocks, .. } = shrunk_manifest.layout() else {
            panic!("expected block-split manifest");
        };
        assert_eq!(blocks.len(), 1);

        target.clear_trace().unwrap();
        let extended = block_on(truncate(
            &repository,
            shrunk.content(),
            &shrunk_manifest,
            MutationId::from_u128(3),
            0,
            (block + 7) as u64,
        ))
        .unwrap();
        assert!(target.trace().unwrap().iter().all(|event| {
            event.operation != crate::TargetOperation::PutIfAbsent
                || !event.key.as_str().contains("/blocks/")
        }));
        let extended_manifest = block_on(repository.load_manifest(extended.content())).unwrap();
        let bytes = block_on(read(&repository, &extended_manifest, 0, block + 7)).unwrap();
        assert_eq!(&bytes[..3], b"abc");
        assert!(bytes[3..].iter().all(|byte| *byte == 0));

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
        let ManifestLayout::BlockSplit { blocks, .. } = zero_manifest.layout() else {
            panic!("expected block-split manifest");
        };
        assert!(blocks.is_empty());
    }

    #[test]
    fn block_count_and_manifest_size_fail_before_payload_upload() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let block = BLOCK_SIZE_V1 as usize;
        let initial = block_on(create(
            &repository,
            FileId::from_u128(1),
            MutationId::from_u128(1),
            0,
            &vec![1; block],
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(initial.content())).unwrap();

        let values = crate::StorageLimitValues {
            max_blocks: 1,
            ..crate::StorageLimitValues::default()
        };
        let count_limited = ContentRepository::new(
            target.clone(),
            "private",
            CreationDefaults::new(StorageMethod::BlockSplit),
            StorageLimits::new(values).unwrap(),
        )
        .unwrap();
        target.clear_trace().unwrap();
        assert!(matches!(
            block_on(write(
                &count_limited,
                initial.content(),
                &manifest,
                MutationId::from_u128(2),
                0,
                block as u64,
                &vec![2; block],
            )),
            Err(StorageError::Limit(LimitError {
                kind: LimitKind::BlockCount,
                ..
            }))
        ));
        assert!(
            target
                .trace()
                .unwrap()
                .iter()
                .all(|event| event.operation != crate::TargetOperation::PutIfAbsent)
        );

        let base_manifest_bytes = target
            .inspect(initial.content().manifest_key())
            .unwrap()
            .unwrap()
            .len();
        let values = crate::StorageLimitValues {
            max_manifest_bytes: base_manifest_bytes,
            ..crate::StorageLimitValues::default()
        };
        let manifest_limited = ContentRepository::new(
            target.clone(),
            "private",
            CreationDefaults::new(StorageMethod::BlockSplit),
            StorageLimits::new(values).unwrap(),
        )
        .unwrap();
        target.clear_trace().unwrap();
        assert!(matches!(
            block_on(write(
                &manifest_limited,
                initial.content(),
                &manifest,
                MutationId::from_u128(3),
                0,
                block as u64,
                &vec![2; block],
            )),
            Err(StorageError::Limit(LimitError {
                kind: LimitKind::Manifest,
                ..
            }))
        ));
        assert!(
            target
                .trace()
                .unwrap()
                .iter()
                .all(|event| event.operation != crate::TargetOperation::PutIfAbsent)
        );
    }

    #[test]
    fn truncate_rejects_nonzero_old_final_padding_before_extension() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let file_id = FileId::from_u128(1);
        let created = block_on(create(
            &repository,
            file_id,
            MutationId::from_u128(1),
            0,
            b"A",
        ))
        .unwrap();
        let manifest = block_on(repository.load_manifest(created.content())).unwrap();
        let ManifestLayout::BlockSplit { blocks, .. } = manifest.layout() else {
            panic!("expected block-split manifest");
        };
        let blob_key = blocks[0].blob().key().clone();
        let mut noncanonical = vec![0; BLOCK_SIZE_V1 as usize];
        noncanonical[..2].copy_from_slice(b"AB");
        let payload = crate::format::encode_envelope(
            crate::format::ObjectKind::Payload,
            &noncanonical,
            repository.limits().max_object_bytes(),
        )
        .unwrap();
        assert!(target.corrupt(&blob_key, payload).unwrap());

        let corrupt_manifest = FileManifest::block_split(
            file_id,
            created.content().generation(),
            1,
            vec![BlockEntry::new(
                0,
                BlobRef::new(
                    blob_key,
                    u64::from(BLOCK_SIZE_V1),
                    u64::from(BLOCK_SIZE_V1),
                    Digest::blake3(&noncanonical),
                ),
            )],
        );
        let encoded_manifest = encode_manifest(&corrupt_manifest, repository.limits()).unwrap();
        assert!(
            target
                .corrupt(created.content().manifest_key(), encoded_manifest.clone())
                .unwrap()
        );
        let corrupt_content = ContentRef::from_persisted(
            file_id,
            created.content().generation(),
            1,
            created.content().manifest_key().clone(),
            Digest::blake3(&encoded_manifest),
            StorageMethod::BlockSplit,
        )
        .unwrap();

        assert!(matches!(
            block_on(
                repository.prepare_truncate(&corrupt_content, MutationId::from_u128(2), 0, 2,)
            ),
            Err(StorageError::Corruption(
                crate::CorruptionError::NonZeroPadding
            ))
        ));
    }
}
