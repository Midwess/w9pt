//! Sparse fixed-size block-split layout over immutable radix mapping pages.

use std::collections::BTreeMap;

use crate::{
    BLOCK_SIZE, ContentRef, ContentRepository, Digest, FileCryptoContext, FileId, FormatError,
    LimitError, LimitKind, MutationId, PreparationIdentity, PreparedContent, StorageError,
    StorageMethod, TargetStore,
    format::{BlobRef, FileManifest, ManifestLayout, PageRef, encode_manifest, minimum_root_level},
    layout::{BlockSpans, LogicalRange},
};

use super::block_map::{
    MapBudget, MapCursor, RewriteMode, highest_old_excluding, retained_summary, rewrite_tree,
};

pub(crate) async fn create<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    file_id: FileId,
    mutation_id: MutationId,
    attempt: u32,
    bytes: &[u8],
) -> Result<PreparedContent, StorageError<S::Error>> {
    let identity = PreparationIdentity::for_create(mutation_id, StorageMethod::BlockSplit, bytes)?;
    check_write_bound(repository, bytes.len())?;
    prepare_new_write(repository, context, file_id, identity, attempt, 0, bytes).await
}

pub(crate) async fn read<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
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
    let root = block_root(manifest)?;
    let mut budget = MapBudget::new(repository.limits(), 0)?;
    let mut cursor = MapCursor::new(repository, context, manifest.file_id(), root);
    for span in BlockSpans::new(range)? {
        let Some(blob) = cursor.lookup(span.block_index(), &mut budget).await? else {
            continue;
        };
        let block = repository.load_payload(context, &blob).await?;
        validate_block_length(&block)?;
        validate_final_padding(manifest, span.block_index(), &block)?;
        let source_start = usize::try_from(span.within_block())
            .map_err(|_| crate::RangeError::LengthConversion)?;
        let span_len =
            usize::try_from(span.len()).map_err(|_| crate::RangeError::LengthConversion)?;
        let source_end =
            source_start
                .checked_add(span_len)
                .ok_or(FormatError::ArithmeticOverflow {
                    field: "block read range",
                })?;
        let destination_end =
            span.buffer_offset()
                .checked_add(span_len)
                .ok_or(FormatError::ArithmeticOverflow {
                    field: "read output range",
                })?;
        output[span.buffer_offset()..destination_end]
            .copy_from_slice(&block[source_start..source_end]);
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn write<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
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
        return Ok(PreparedContent::new(
            base.clone(),
            false,
            identity,
            attempt,
            context.binding().clone(),
        ));
    }
    let root = block_root(manifest)?;
    let update_bound = block_span_count(range)?;
    let mut budget = MapBudget::new(repository.limits(), update_bound)?;
    let preflight = plan_write_pass(
        repository,
        context,
        manifest,
        root,
        identity,
        attempt,
        range,
        data,
        &mut budget,
        false,
    )
    .await?;
    if !preflight.content_changed {
        return Ok(PreparedContent::new(
            base.clone(),
            false,
            identity,
            attempt,
            context.binding().clone(),
        ));
    }
    let preflight_count = preflight.materialized_count;
    let preflight_highest = preflight.highest;
    check_materialized_count(repository, preflight_count)?;
    let desired_level = preflight_highest.map(minimum_root_level).transpose()?;
    let preflight_root = if let Some(level) = desired_level {
        rewrite_tree(
            repository,
            context,
            manifest.file_id(),
            root,
            &preflight.updates,
            None,
            level,
            identity,
            attempt,
            &mut budget,
            RewriteMode::Preflight,
        )
        .await?
    } else {
        None
    };
    validate_root_summary(&preflight_root, preflight_count, preflight_highest)?;
    let logical_size = manifest.logical_size().max(range.end());
    let generation = next_generation(manifest)?;
    let expected_manifest = FileManifest::block_split(
        manifest.file_id(),
        generation,
        logical_size,
        preflight_root.clone(),
    );
    encode_manifest(&expected_manifest, repository.limits()).map_err(StorageError::from)?;
    drop(preflight);
    budget.reserve_replay()?;

    let prepared = plan_write_pass(
        repository,
        context,
        manifest,
        root,
        identity,
        attempt,
        range,
        data,
        &mut budget,
        true,
    )
    .await?;
    if prepared.materialized_count != preflight_count || prepared.highest != preflight_highest {
        return Err(crate::CorruptionError::IdentityMismatch {
            field: "write preflight",
        }
        .into());
    }
    let actual_root = if let Some(level) = desired_level {
        rewrite_tree(
            repository,
            context,
            manifest.file_id(),
            root,
            &prepared.updates,
            None,
            level,
            identity,
            attempt,
            &mut budget,
            RewriteMode::Store,
        )
        .await?
    } else {
        None
    };
    if actual_root != preflight_root {
        return Err(crate::CorruptionError::IdentityMismatch {
            field: "write map replay",
        }
        .into());
    }
    let result_manifest =
        FileManifest::block_split(manifest.file_id(), generation, logical_size, actual_root);
    repository
        .prepare_manifest(context, &result_manifest, identity, attempt, true)
        .await
}

pub(crate) async fn write_from_new<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
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
    prepare_new_write(
        repository, context, file_id, identity, attempt, offset, data,
    )
    .await
}

async fn prepare_new_write<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    file_id: FileId,
    identity: PreparationIdentity,
    attempt: u32,
    offset: u64,
    data: &[u8],
) -> Result<PreparedContent, StorageError<S::Error>> {
    let range = LogicalRange::from_usize(offset, data.len())?;
    let logical_size = if data.is_empty() { 0 } else { range.end() };
    let empty_manifest = FileManifest::block_split(file_id, 1, 0, None);
    let update_bound = block_span_count(range)?;
    let mut budget = MapBudget::new(repository.limits(), update_bound)?;
    let preflight = plan_write_pass(
        repository,
        context,
        &empty_manifest,
        None,
        identity,
        attempt,
        range,
        data,
        &mut budget,
        false,
    )
    .await?;
    let preflight_count = preflight.materialized_count;
    let preflight_highest = preflight.highest;
    check_materialized_count(repository, preflight_count)?;
    let desired_level = preflight_highest.map(minimum_root_level).transpose()?;
    let preflight_root = if let Some(level) = desired_level {
        rewrite_tree(
            repository,
            context,
            file_id,
            None,
            &preflight.updates,
            None,
            level,
            identity,
            attempt,
            &mut budget,
            RewriteMode::Preflight,
        )
        .await?
    } else {
        None
    };
    validate_root_summary(&preflight_root, preflight_count, preflight_highest)?;
    let expected_manifest =
        FileManifest::block_split(file_id, 1, logical_size, preflight_root.clone());
    encode_manifest(&expected_manifest, repository.limits()).map_err(StorageError::from)?;
    drop(preflight);
    budget.reserve_replay()?;

    let prepared = plan_write_pass(
        repository,
        context,
        &empty_manifest,
        None,
        identity,
        attempt,
        range,
        data,
        &mut budget,
        true,
    )
    .await?;
    if prepared.materialized_count != preflight_count || prepared.highest != preflight_highest {
        return Err(crate::CorruptionError::IdentityMismatch {
            field: "new write preflight",
        }
        .into());
    }
    let actual_root = if let Some(level) = desired_level {
        rewrite_tree(
            repository,
            context,
            file_id,
            None,
            &prepared.updates,
            None,
            level,
            identity,
            attempt,
            &mut budget,
            RewriteMode::Store,
        )
        .await?
    } else {
        None
    };
    if actual_root != preflight_root {
        return Err(crate::CorruptionError::IdentityMismatch {
            field: "new write map replay",
        }
        .into());
    }
    let manifest = FileManifest::block_split(file_id, 1, logical_size, actual_root);
    repository
        .prepare_manifest(context, &manifest, identity, attempt, true)
        .await
}

struct WritePlan {
    updates: BTreeMap<u64, Option<BlobRef>>,
    materialized_count: u64,
    highest: Option<u64>,
    content_changed: bool,
}

#[allow(clippy::too_many_arguments)]
async fn plan_write_pass<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    manifest: &FileManifest,
    root: Option<&PageRef>,
    identity: PreparationIdentity,
    attempt: u32,
    range: LogicalRange,
    data: &[u8],
    budget: &mut MapBudget,
    store_payloads: bool,
) -> Result<WritePlan, StorageError<S::Error>> {
    let mut updates = BTreeMap::new();
    let mut cursor = MapCursor::new(repository, context, manifest.file_id(), root);
    verify_distant_partial_tail(repository, context, manifest, range, &mut cursor, budget).await?;
    let mut materialized_count = root.map_or(0, PageRef::materialized_block_count);
    let mut content_changed = range.end() > manifest.logical_size();
    for span in BlockSpans::new(range)? {
        let old = cursor.lookup(span.block_index(), budget).await?;
        let input_start = span.buffer_offset();
        let input_end = input_start
            .checked_add(
                usize::try_from(span.len()).map_err(|_| crate::RangeError::LengthConversion)?,
            )
            .ok_or(FormatError::ArithmeticOverflow {
                field: "write input range",
            })?;
        let mut block = if span.is_full_block() {
            data[input_start..input_end].to_vec()
        } else if let Some(old) = &old {
            let block = repository.load_payload(context, old).await?;
            validate_block_length(&block)?;
            validate_final_padding(manifest, span.block_index(), &block)?;
            block
        } else {
            vec![0; BLOCK_SIZE as usize]
        };
        validate_block_length(&block)?;
        if !span.is_full_block() {
            let start = usize::try_from(span.within_block())
                .map_err(|_| crate::RangeError::LengthConversion)?;
            let end = start
                .checked_add(
                    usize::try_from(span.len()).map_err(|_| crate::RangeError::LengthConversion)?,
                )
                .ok_or(FormatError::ArithmeticOverflow {
                    field: "block write range",
                })?;
            block[start..end].copy_from_slice(&data[input_start..input_end]);
        }
        let replacement = if block.iter().all(|byte| *byte == 0) {
            None
        } else if old
            .as_ref()
            .is_some_and(|old| old.digest() == Digest::blake3(&block))
        {
            old.clone()
        } else if store_payloads {
            Some(
                repository
                    .store_block_payload(
                        context,
                        manifest.file_id(),
                        identity,
                        attempt,
                        span.block_index(),
                        &block,
                    )
                    .await?,
            )
        } else {
            Some(expected_block_blob(
                repository,
                context,
                manifest.file_id(),
                identity,
                attempt,
                span.block_index(),
                &block,
            )?)
        };
        if replacement != old {
            content_changed = true;
            match (old.is_some(), replacement.is_some()) {
                (false, true) => {
                    materialized_count = materialized_count.checked_add(1).ok_or(
                        FormatError::ArithmeticOverflow {
                            field: "materialized block count",
                        },
                    )?
                }
                (true, false) => {
                    materialized_count = materialized_count.checked_sub(1).ok_or(
                        FormatError::ArithmeticOverflow {
                            field: "materialized block count",
                        },
                    )?
                }
                _ => {}
            }
            updates.insert(span.block_index(), replacement);
        }
    }
    let old_highest = highest_old_excluding(
        repository,
        context,
        manifest.file_id(),
        root,
        &updates,
        budget,
    )
    .await?;
    let new_highest = updates
        .iter()
        .rev()
        .find_map(|(index, value)| value.as_ref().map(|_| *index));
    Ok(WritePlan {
        updates,
        materialized_count,
        highest: match (old_highest, new_highest) {
            (Some(old), Some(new)) => Some(old.max(new)),
            (old, new) => old.or(new),
        },
        content_changed,
    })
}

async fn verify_distant_partial_tail<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    manifest: &FileManifest,
    range: LogicalRange,
    cursor: &mut MapCursor<'_, S>,
    budget: &mut MapBudget,
) -> Result<(), StorageError<S::Error>> {
    let block_size = u64::from(BLOCK_SIZE);
    if range.end() <= manifest.logical_size() || manifest.logical_size().is_multiple_of(block_size)
    {
        return Ok(());
    }
    let final_index = manifest.logical_size() / block_size;
    let final_start = final_index * block_size;
    let final_end = final_start
        .checked_add(block_size)
        .ok_or(FormatError::ArithmeticOverflow {
            field: "old final block",
        })?;
    let touches = range.start() < final_end && range.end() > final_start;
    if touches {
        return Ok(());
    }
    if let Some(blob) = cursor.lookup(final_index, budget).await? {
        let block = repository.load_payload(context, &blob).await?;
        validate_block_length(&block)?;
        validate_final_padding(manifest, final_index, &block)?;
    }
    Ok(())
}

pub(crate) async fn truncate<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    base: &ContentRef,
    manifest: &FileManifest,
    mutation_id: MutationId,
    attempt: u32,
    logical_size: u64,
) -> Result<PreparedContent, StorageError<S::Error>> {
    let identity = PreparationIdentity::for_truncate(mutation_id, base, logical_size);
    if logical_size == manifest.logical_size() {
        return Ok(PreparedContent::new(
            base.clone(),
            false,
            identity,
            attempt,
            context.binding().clone(),
        ));
    }
    let root = block_root(manifest)?;
    let mut budget = MapBudget::new(repository.limits(), 1)?;
    let preflight = plan_truncate_pass(
        repository,
        context,
        manifest,
        root,
        identity,
        attempt,
        logical_size,
        &mut budget,
        false,
    )
    .await?;
    let preflight_count = preflight.materialized_count;
    let preflight_highest = preflight.highest;
    check_materialized_count(repository, preflight_count)?;
    let desired_level = preflight_highest.map(minimum_root_level).transpose()?;
    let preflight_root = if logical_size < manifest.logical_size() {
        if let Some(level) = desired_level {
            rewrite_tree(
                repository,
                context,
                manifest.file_id(),
                root,
                &preflight.updates,
                Some(visible_blocks(logical_size)),
                level,
                identity,
                attempt,
                &mut budget,
                RewriteMode::Preflight,
            )
            .await?
        } else {
            None
        }
    } else {
        root.cloned()
    };
    validate_root_summary(&preflight_root, preflight_count, preflight_highest)?;
    let generation = next_generation(manifest)?;
    let expected_manifest = FileManifest::block_split(
        manifest.file_id(),
        generation,
        logical_size,
        preflight_root.clone(),
    );
    encode_manifest(&expected_manifest, repository.limits()).map_err(StorageError::from)?;
    drop(preflight);
    budget.reserve_replay()?;

    let prepared = plan_truncate_pass(
        repository,
        context,
        manifest,
        root,
        identity,
        attempt,
        logical_size,
        &mut budget,
        true,
    )
    .await?;
    if prepared.materialized_count != preflight_count || prepared.highest != preflight_highest {
        return Err(crate::CorruptionError::IdentityMismatch {
            field: "truncate preflight",
        }
        .into());
    }
    let actual_root = if logical_size < manifest.logical_size() {
        if let Some(level) = desired_level {
            rewrite_tree(
                repository,
                context,
                manifest.file_id(),
                root,
                &prepared.updates,
                Some(visible_blocks(logical_size)),
                level,
                identity,
                attempt,
                &mut budget,
                RewriteMode::Store,
            )
            .await?
        } else {
            None
        }
    } else {
        root.cloned()
    };
    if actual_root != preflight_root {
        return Err(crate::CorruptionError::IdentityMismatch {
            field: "truncate map replay",
        }
        .into());
    }
    let result_manifest =
        FileManifest::block_split(manifest.file_id(), generation, logical_size, actual_root);
    repository
        .prepare_manifest(context, &result_manifest, identity, attempt, true)
        .await
}

struct TruncatePlan {
    updates: BTreeMap<u64, Option<BlobRef>>,
    materialized_count: u64,
    highest: Option<u64>,
}

#[allow(clippy::too_many_arguments)]
async fn plan_truncate_pass<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    manifest: &FileManifest,
    root: Option<&PageRef>,
    identity: PreparationIdentity,
    attempt: u32,
    logical_size: u64,
    budget: &mut MapBudget,
    store_payloads: bool,
) -> Result<TruncatePlan, StorageError<S::Error>> {
    let block_size = u64::from(BLOCK_SIZE);
    let mut updates = BTreeMap::new();
    if logical_size > manifest.logical_size() {
        if !manifest.logical_size().is_multiple_of(block_size) {
            let index = manifest.logical_size() / block_size;
            let mut cursor = MapCursor::new(repository, context, manifest.file_id(), root);
            if let Some(blob) = cursor.lookup(index, budget).await? {
                let block = repository.load_payload(context, &blob).await?;
                validate_block_length(&block)?;
                validate_final_padding(manifest, index, &block)?;
            }
        }
        return Ok(TruncatePlan {
            updates,
            materialized_count: root.map_or(0, PageRef::materialized_block_count),
            highest: root.map(PageRef::highest_materialized_block),
        });
    }

    let cutoff = visible_blocks(logical_size);
    let summary = retained_summary(
        repository,
        context,
        manifest.file_id(),
        root,
        cutoff,
        budget,
    )
    .await?;
    let mut materialized_count = summary.map_or(0, |(count, _)| count);
    let mut highest = summary.map(|(_, highest)| highest);
    let visible_tail = logical_size % block_size;
    if visible_tail != 0 {
        let final_index = logical_size / block_size;
        let mut cursor = MapCursor::new(repository, context, manifest.file_id(), root);
        if let Some(old) = cursor.lookup(final_index, budget).await? {
            let mut block = repository.load_payload(context, &old).await?;
            validate_block_length(&block)?;
            validate_final_padding(manifest, final_index, &block)?;
            let tail =
                usize::try_from(visible_tail).map_err(|_| crate::RangeError::LengthConversion)?;
            block[tail..].fill(0);
            let replacement = if block.iter().all(|byte| *byte == 0) {
                None
            } else if Digest::blake3(&block) == old.digest() {
                Some(old.clone())
            } else if store_payloads {
                Some(
                    repository
                        .store_block_payload(
                            context,
                            manifest.file_id(),
                            identity,
                            attempt,
                            final_index,
                            &block,
                        )
                        .await?,
                )
            } else {
                Some(expected_block_blob(
                    repository,
                    context,
                    manifest.file_id(),
                    identity,
                    attempt,
                    final_index,
                    &block,
                )?)
            };
            if replacement.as_ref() != Some(&old) {
                if replacement.is_none() {
                    materialized_count = materialized_count.checked_sub(1).ok_or(
                        FormatError::ArithmeticOverflow {
                            field: "materialized block count",
                        },
                    )?;
                    highest = retained_summary(
                        repository,
                        context,
                        manifest.file_id(),
                        root,
                        final_index,
                        budget,
                    )
                    .await?
                    .map(|(_, highest)| highest);
                }
                updates.insert(final_index, replacement);
            }
        }
    }
    Ok(TruncatePlan {
        updates,
        materialized_count,
        highest,
    })
}

pub(crate) async fn truncate_from_new<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
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
    let manifest = FileManifest::block_split(file_id, 1, logical_size, None);
    encode_manifest(&manifest, repository.limits()).map_err(StorageError::from)?;
    repository
        .prepare_manifest(context, &manifest, identity, attempt, true)
        .await
}

fn block_root(manifest: &FileManifest) -> Result<Option<&PageRef>, FormatError> {
    let ManifestLayout::BlockSplit { root, .. } = manifest.layout() else {
        return Err(FormatError::NonCanonical {
            field: "block-split layout",
        });
    };
    Ok(root.as_ref())
}

fn visible_blocks(logical_size: u64) -> u64 {
    logical_size / u64::from(BLOCK_SIZE)
        + u64::from(!logical_size.is_multiple_of(u64::from(BLOCK_SIZE)))
}

fn block_span_count(range: LogicalRange) -> Result<usize, FormatError> {
    if range.is_empty() {
        return Ok(0);
    }
    let first = range.start() / u64::from(BLOCK_SIZE);
    let last = (range.end() - 1) / u64::from(BLOCK_SIZE);
    usize::try_from(last - first + 1).map_err(|_| FormatError::ArithmeticOverflow {
        field: "block span count",
    })
}

fn next_generation(manifest: &FileManifest) -> Result<u64, FormatError> {
    manifest
        .generation()
        .checked_add(1)
        .ok_or(FormatError::ArithmeticOverflow {
            field: "content generation",
        })
}

fn validate_root_summary<E>(
    root: &Option<PageRef>,
    count: u64,
    highest: Option<u64>,
) -> Result<(), StorageError<E>> {
    match (root, highest) {
        (None, None) if count == 0 => Ok(()),
        (Some(root), Some(highest))
            if root.materialized_block_count() == count
                && root.highest_materialized_block() == highest =>
        {
            Ok(())
        }
        _ => Err(crate::CorruptionError::IdentityMismatch {
            field: "map root summary",
        }
        .into()),
    }
}

fn validate_block_length<E>(block: &[u8]) -> Result<(), StorageError<E>> {
    if block.len() == BLOCK_SIZE as usize {
        Ok(())
    } else {
        Err(crate::CorruptionError::InvalidLength {
            field: "block plaintext",
            expected: u64::from(BLOCK_SIZE),
            actual: u64::try_from(block.len()).unwrap_or(u64::MAX),
        }
        .into())
    }
}

fn expected_block_blob<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    file_id: FileId,
    identity: PreparationIdentity,
    attempt: u32,
    block_index: u64,
    block: &[u8],
) -> Result<BlobRef, StorageError<S::Error>> {
    repository.expected_block_payload(context, file_id, identity, attempt, block_index, block)
}

fn validate_final_padding<E>(
    manifest: &FileManifest,
    block_index: u64,
    block: &[u8],
) -> Result<(), StorageError<E>> {
    let block_size = u64::from(BLOCK_SIZE);
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

fn check_materialized_count<S: TargetStore>(
    repository: &ContentRepository<S>,
    actual: u64,
) -> Result<(), LimitError> {
    let limit = repository.limits().max_materialized_blocks();
    if actual > limit {
        Err(LimitError::new(
            LimitKind::MaterializedBlocks,
            actual,
            limit,
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
