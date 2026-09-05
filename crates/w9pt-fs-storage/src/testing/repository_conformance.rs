//! Reusable content-repository conformance checks.

use core::fmt;

use crate::{
    BLOCK_SIZE_V1, ContentRepository, CreationDefaults, FileId, MutationId, Publication,
    PublishedContent, StorageError, StorageLimits, StorageMethod, TargetStore,
    format::ManifestLayout,
};

/// Failure returned by reusable repository conformance.
#[derive(Debug)]
pub enum RepositoryConformanceError<E> {
    /// Repository construction failed.
    Configuration(crate::ConfigurationError),
    /// A repository or target operation failed.
    Storage(StorageError<E>),
    /// An observed result violated repository semantics.
    Assertion(&'static str),
}

impl<E: fmt::Display> fmt::Display for RepositoryConformanceError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::Assertion(message) => formatter.write_str(message),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for RepositoryConformanceError<E> {}

/// Exercises both version-1 layouts through two independently held target clients.
pub async fn check_repository_conformance<S: TargetStore + Clone>(
    first: S,
    second: S,
    namespace: &str,
) -> Result<(), RepositoryConformanceError<S::Error>> {
    check_repository_method_conformance(
        first.clone(),
        second.clone(),
        namespace,
        StorageMethod::Raw,
        FileId::from_u128(1),
    )
    .await?;
    check_repository_method_conformance(
        first.clone(),
        second.clone(),
        namespace,
        StorageMethod::BlockSplit,
        FileId::from_u128(2),
    )
    .await?;
    check_publication_boundaries(
        first.clone(),
        second.clone(),
        namespace,
        StorageMethod::Raw,
        FileId::from_u128(0x11_0001),
        0x11_0000,
    )
    .await?;
    check_publication_boundaries(
        first,
        second,
        namespace,
        StorageMethod::BlockSplit,
        FileId::from_u128(0x21_0001),
        0x21_0000,
    )
    .await
}

/// Exercises one selected persisted storage method through two target clients.
pub async fn check_repository_method_conformance<S: TargetStore + Clone>(
    first: S,
    second: S,
    namespace: &str,
    method: StorageMethod,
    file_id: FileId,
) -> Result<(), RepositoryConformanceError<S::Error>> {
    match method {
        StorageMethod::Raw => check_raw_lifecycle(first, second, namespace, file_id).await,
        StorageMethod::BlockSplit => {
            check_block_split_lifecycle(first, second, namespace, file_id).await
        }
    }
}

async fn check_raw_lifecycle<S: TargetStore + Clone>(
    first: S,
    second: S,
    namespace: &str,
    file_id: FileId,
) -> Result<(), RepositoryConformanceError<S::Error>> {
    let prefix = format!("{namespace}/raw/lifecycle");
    let writer = open_repository(first, &prefix, StorageMethod::Raw)?;
    let observer = open_repository(second.clone(), &prefix, StorageMethod::BlockSplit)?;

    let empty_file = FileId::from_u128(0x10_0001);
    let empty_mutation = MutationId::from_u128(0x10_0001);
    let empty = writer
        .prepare_create(empty_file, empty_mutation, 0, b"")
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    let Publication::Published(_) = writer
        .publisher()
        .create(empty_file, empty_mutation, &empty)
        .await
        .map_err(RepositoryConformanceError::Storage)?
    else {
        return Err(RepositoryConformanceError::Assertion(
            "fresh empty Raw head conflicted",
        ));
    };
    assert_visible(&observer, empty_file, b"").await?;

    let create_mutation = MutationId::from_u128(0x10_0010);
    let mut model = b"raw-lifecycle-base".to_vec();
    let initial = writer
        .prepare_create(file_id, create_mutation, 0, &model)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    let Publication::Published(mut published) = writer
        .publisher()
        .create(file_id, create_mutation, &initial)
        .await
        .map_err(RepositoryConformanceError::Storage)?
    else {
        return Err(RepositoryConformanceError::Assertion(
            "fresh Raw head conflicted",
        ));
    };
    assert_visible(&observer, file_id, &model).await?;

    let reused = open_repository(second, &prefix, StorageMethod::Raw)?
        .prepare_create(file_id, create_mutation, 0, &model)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    if reused.content() != initial.content() {
        return Err(RepositoryConformanceError::Assertion(
            "identical Raw immutable preparation was not reused exactly",
        ));
    }

    let write_mutation = MutationId::from_u128(0x10_0011);
    let write = writer
        .prepare_write(published.content(), write_mutation, 0, 3, b"XYZ")
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    model_write(&mut model, 3, b"XYZ")?;
    published = publish_replace(&writer, &published, write_mutation, &write).await?;
    assert_visible(&observer, file_id, &model).await?;

    let gap_mutation = MutationId::from_u128(0x10_0012);
    let gap_offset = model
        .len()
        .checked_add(5)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(RepositoryConformanceError::Assertion(
            "Raw gap offset overflowed",
        ))?;
    let gap_write = writer
        .prepare_write(published.content(), gap_mutation, 0, gap_offset, b"tail")
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    model_write(&mut model, gap_offset, b"tail")?;
    published = publish_replace(&writer, &published, gap_mutation, &gap_write).await?;
    assert_visible(&observer, file_id, &model).await?;

    let extended_size = model
        .len()
        .checked_add(3)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(RepositoryConformanceError::Assertion(
            "Raw extension size overflowed",
        ))?;
    let shrink_mutation = MutationId::from_u128(0x10_0013);
    let shrunk = writer
        .prepare_truncate(published.content(), shrink_mutation, 0, 7)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    model.truncate(7);
    published = publish_replace(&writer, &published, shrink_mutation, &shrunk).await?;
    assert_visible(&observer, file_id, &model).await?;

    let extend_mutation = MutationId::from_u128(0x10_0014);
    let extended = writer
        .prepare_truncate(published.content(), extend_mutation, 0, extended_size)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    let extended_size = usize::try_from(extended_size).map_err(|_| {
        RepositoryConformanceError::Assertion("Raw extension size is not representable")
    })?;
    model.resize(extended_size, 0);
    publish_replace(&writer, &published, extend_mutation, &extended).await?;
    assert_visible(&observer, file_id, &model).await?;
    Ok(())
}

async fn check_block_split_lifecycle<S: TargetStore + Clone>(
    first: S,
    second: S,
    namespace: &str,
    file_id: FileId,
) -> Result<(), RepositoryConformanceError<S::Error>> {
    let prefix = format!("{namespace}/block-split/lifecycle");
    let writer = open_repository(first, &prefix, StorageMethod::BlockSplit)?;
    let observer = open_repository(second.clone(), &prefix, StorageMethod::Raw)?;
    let retry = open_repository(second, &prefix, StorageMethod::BlockSplit)?;
    let block = usize::try_from(BLOCK_SIZE_V1).map_err(|_| {
        RepositoryConformanceError::Assertion("block size is not platform-representable")
    })?;
    let two_blocks = block
        .checked_mul(2)
        .ok_or(RepositoryConformanceError::Assertion(
            "two-block model length overflowed",
        ))?;
    let initial_len = two_blocks
        .checked_add(17)
        .ok_or(RepositoryConformanceError::Assertion(
            "BlockSplit initial length overflowed",
        ))?;
    let mut model = (0..two_blocks)
        .map(|index| u8::try_from(index % 251 + 1).expect("model byte is bounded"))
        .collect::<Vec<_>>();
    model.resize(initial_len, 0);

    let create_mutation = MutationId::from_u128(0x20_0010);
    let initial = writer
        .prepare_create(file_id, create_mutation, 0, &model)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    let reused = retry
        .prepare_create(file_id, create_mutation, 0, &model)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    if reused != initial {
        return Err(RepositoryConformanceError::Assertion(
            "identical BlockSplit immutable preparation was not reused exactly",
        ));
    }
    let Publication::Published(mut published) = writer
        .publisher()
        .create(file_id, create_mutation, &initial)
        .await
        .map_err(RepositoryConformanceError::Storage)?
    else {
        return Err(RepositoryConformanceError::Assertion(
            "fresh BlockSplit head conflicted",
        ));
    };
    assert_visible(&observer, file_id, &model).await?;
    assert_range(
        &observer,
        published.content(),
        u64::try_from(block - 19)
            .map_err(|_| RepositoryConformanceError::Assertion("within-block offset overflowed"))?,
        11,
        &model[block - 19..block - 8],
    )
    .await?;
    assert_range(
        &observer,
        published.content(),
        u64::try_from(block - 9)
            .map_err(|_| RepositoryConformanceError::Assertion("cross-block offset overflowed"))?,
        25,
        &model[block - 9..block + 16],
    )
    .await?;
    assert_range(
        &observer,
        published.content(),
        u64::try_from(initial_len - 8)
            .map_err(|_| RepositoryConformanceError::Assertion("EOF read offset overflowed"))?,
        31,
        &model[initial_len - 8..],
    )
    .await?;

    let partial_mutation = MutationId::from_u128(0x20_0011);
    let partial_offset = u64::try_from(block - 9)
        .map_err(|_| RepositoryConformanceError::Assertion("partial write offset overflowed"))?;
    let partial_bytes = vec![0xe1; 25];
    let partial = writer
        .prepare_write(
            published.content(),
            partial_mutation,
            0,
            partial_offset,
            &partial_bytes,
        )
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    model_write(&mut model, partial_offset, &partial_bytes)?;
    published = publish_replace(&writer, &published, partial_mutation, &partial).await?;
    assert_visible(&observer, file_id, &model).await?;

    let full_mutation = MutationId::from_u128(0x20_0012);
    let block_offset = u64::try_from(block)
        .map_err(|_| RepositoryConformanceError::Assertion("full-block offset overflowed"))?;
    let full_bytes = vec![0xa5; block];
    let full = writer
        .prepare_write(
            published.content(),
            full_mutation,
            0,
            block_offset,
            &full_bytes,
        )
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    model_write(&mut model, block_offset, &full_bytes)?;
    published = publish_replace(&writer, &published, full_mutation, &full).await?;
    assert_visible(&observer, file_id, &model).await?;

    let sparse_offset = block
        .checked_mul(3)
        .and_then(|value| value.checked_add(5))
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(RepositoryConformanceError::Assertion(
            "sparse write offset overflowed",
        ))?;
    let sparse_mutation = MutationId::from_u128(0x20_0013);
    let sparse = writer
        .prepare_write(
            published.content(),
            sparse_mutation,
            0,
            sparse_offset,
            b"gap",
        )
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    model_write(&mut model, sparse_offset, b"gap")?;
    published = publish_replace(&writer, &published, sparse_mutation, &sparse).await?;
    assert_visible(&observer, file_id, &model).await?;

    let zero_mutation = MutationId::from_u128(0x20_0014);
    let zero_block = vec![0; block];
    let zeroed = writer
        .prepare_write(
            published.content(),
            zero_mutation,
            0,
            block_offset,
            &zero_block,
        )
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    model_write(&mut model, block_offset, &zero_block)?;
    published = publish_replace(&writer, &published, zero_mutation, &zeroed).await?;
    assert_visible(&observer, file_id, &model).await?;
    let manifest = observer
        .load_manifest(published.content())
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    let ManifestLayout::BlockSplit { blocks, .. } = manifest.layout() else {
        return Err(RepositoryConformanceError::Assertion(
            "persisted BlockSplit method changed under Raw creation default",
        ));
    };
    if blocks.iter().any(|entry| matches!(entry.index(), 1 | 2)) {
        return Err(RepositoryConformanceError::Assertion(
            "zero or sparse BlockSplit hole retained a payload reference",
        ));
    }

    let shrink_size = block
        .checked_mul(3)
        .and_then(|value| value.checked_add(6))
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(RepositoryConformanceError::Assertion(
            "BlockSplit shrink size overflowed",
        ))?;
    let shrink_mutation = MutationId::from_u128(0x20_0015);
    let shrunk = writer
        .prepare_truncate(published.content(), shrink_mutation, 0, shrink_size)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    model.truncate(usize::try_from(shrink_size).map_err(|_| {
        RepositoryConformanceError::Assertion("BlockSplit shrink is not representable")
    })?);
    published = publish_replace(&writer, &published, shrink_mutation, &shrunk).await?;
    assert_visible(&observer, file_id, &model).await?;

    let extend_size = block
        .checked_mul(3)
        .and_then(|value| value.checked_add(29))
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(RepositoryConformanceError::Assertion(
            "BlockSplit extension size overflowed",
        ))?;
    let extend_mutation = MutationId::from_u128(0x20_0016);
    let extended = writer
        .prepare_truncate(published.content(), extend_mutation, 0, extend_size)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    model.resize(
        usize::try_from(extend_size).map_err(|_| {
            RepositoryConformanceError::Assertion("BlockSplit extension is not representable")
        })?,
        0,
    );
    publish_replace(&writer, &published, extend_mutation, &extended).await?;
    let final_published = assert_visible(&observer, file_id, &model).await?;
    let final_manifest = observer
        .load_manifest(final_published.content())
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    if final_manifest.method() != StorageMethod::BlockSplit {
        return Err(RepositoryConformanceError::Assertion(
            "creation-default change reinterpreted persisted BlockSplit content",
        ));
    }
    Ok(())
}

async fn check_publication_boundaries<S: TargetStore + Clone>(
    first: S,
    second: S,
    namespace: &str,
    method: StorageMethod,
    file_id: FileId,
    mutation_base: u128,
) -> Result<(), RepositoryConformanceError<S::Error>> {
    let method_name = match method {
        StorageMethod::Raw => "raw",
        StorageMethod::BlockSplit => "block-split",
    };
    let prefix = format!("{namespace}/{method_name}/publication");
    let writer = open_repository(first, &prefix, method)?;
    let competitor = open_repository(second.clone(), &prefix, method)?;
    let observer = open_repository(second, &prefix, opposite(method))?;
    let mut model = match method {
        StorageMethod::Raw => b"old-published-version".to_vec(),
        StorageMethod::BlockSplit => {
            let block = usize::try_from(BLOCK_SIZE_V1).map_err(|_| {
                RepositoryConformanceError::Assertion("publication block size is not representable")
            })?;
            let length = block
                .checked_add(13)
                .ok_or(RepositoryConformanceError::Assertion(
                    "publication model length overflowed",
                ))?;
            (0..length)
                .map(|index| u8::try_from(index % 239 + 1).expect("model byte is bounded"))
                .collect()
        }
    };

    let create_mutation = MutationId::from_u128(mutation_base + 1);
    let initial = writer
        .prepare_create(file_id, create_mutation, 0, &model)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    let Publication::Published(created) = writer
        .publisher()
        .create(file_id, create_mutation, &initial)
        .await
        .map_err(RepositoryConformanceError::Storage)?
    else {
        return Err(RepositoryConformanceError::Assertion(
            "publication-boundary base conflicted",
        ));
    };
    assert_visible(&observer, file_id, &model).await?;

    let abandoned_mutation = MutationId::from_u128(mutation_base + 2);
    let abandoned = writer
        .prepare_write(created.content(), abandoned_mutation, 0, 0, b"abandoned")
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    drop(abandoned);
    let still_old = assert_visible(&observer, file_id, &model).await?;
    if still_old.content() != created.content() {
        return Err(RepositoryConformanceError::Assertion(
            "abandoned preparation changed the published head",
        ));
    }

    let publish_mutation = MutationId::from_u128(mutation_base + 3);
    let publish_bytes = b"published";
    let publish_offset = 2_u64;
    let prepared = writer
        .prepare_write(
            created.content(),
            publish_mutation,
            0,
            publish_offset,
            publish_bytes,
        )
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    model_write(&mut model, publish_offset, publish_bytes)?;
    let published = publish_replace(&writer, &created, publish_mutation, &prepared).await?;
    drop(published);
    drop(prepared);
    let base = assert_visible(&observer, file_id, &model).await?;

    let winner_mutation = MutationId::from_u128(mutation_base + 4);
    let loser_mutation = MutationId::from_u128(mutation_base + 5);
    let winner_offset = 0_u64;
    let loser_offset = 1_u64;
    let winner_prepared = writer
        .prepare_write(base.content(), winner_mutation, 0, winner_offset, b"A")
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    let loser_prepared = competitor
        .prepare_write(base.content(), loser_mutation, 0, loser_offset, b"B")
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    let winner = publish_replace(&writer, &base, winner_mutation, &winner_prepared).await?;
    model_write(&mut model, winner_offset, b"A")?;
    if competitor
        .publisher()
        .replace(&base, loser_mutation, &loser_prepared)
        .await
        .map_err(RepositoryConformanceError::Storage)?
        != Publication::Conflict
    {
        return Err(RepositoryConformanceError::Assertion(
            "stale repository publication did not conflict",
        ));
    }
    let observed_winner = assert_visible(&observer, file_id, &model).await?;
    if observed_winner.content() != winner.content() {
        return Err(RepositoryConformanceError::Assertion(
            "independent client did not observe the publication winner",
        ));
    }

    let rebased = competitor
        .prepare_write(
            observed_winner.content(),
            loser_mutation,
            1,
            loser_offset,
            b"B",
        )
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    let final_published =
        publish_replace(&competitor, &observed_winner, loser_mutation, &rebased).await?;
    model_write(&mut model, loser_offset, b"B")?;
    let final_observed = assert_visible(&observer, file_id, &model).await?;
    if final_observed.content() != final_published.content() {
        return Err(RepositoryConformanceError::Assertion(
            "rebased serial publication was not independently visible",
        ));
    }
    Ok(())
}

async fn publish_replace<S: TargetStore>(
    repository: &ContentRepository<S>,
    current: &PublishedContent,
    mutation_id: MutationId,
    prepared: &crate::PreparedContent,
) -> Result<PublishedContent, RepositoryConformanceError<S::Error>> {
    match repository
        .publisher()
        .replace(current, mutation_id, prepared)
        .await
        .map_err(RepositoryConformanceError::Storage)?
    {
        Publication::Published(published) => Ok(published),
        Publication::Conflict => Err(RepositoryConformanceError::Assertion(
            "uncontended repository publication conflicted",
        )),
    }
}

async fn assert_visible<S: TargetStore>(
    repository: &ContentRepository<S>,
    file_id: FileId,
    expected: &[u8],
) -> Result<PublishedContent, RepositoryConformanceError<S::Error>> {
    let published = repository
        .publisher()
        .load(file_id)
        .await
        .map_err(RepositoryConformanceError::Storage)?
        .ok_or(RepositoryConformanceError::Assertion(
            "published repository head was not independently visible",
        ))?;
    let requested = expected
        .len()
        .checked_add(17)
        .ok_or(RepositoryConformanceError::Assertion(
            "visibility read length overflowed",
        ))?;
    let actual = repository
        .read(published.content(), 0, requested)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    if actual != expected {
        return Err(RepositoryConformanceError::Assertion(
            "independently reopened bytes differ from byte-vector model",
        ));
    }
    let eof = u64::try_from(expected.len())
        .map_err(|_| RepositoryConformanceError::Assertion("visibility EOF offset overflowed"))?;
    if !repository
        .read(published.content(), eof, 1)
        .await
        .map_err(RepositoryConformanceError::Storage)?
        .is_empty()
    {
        return Err(RepositoryConformanceError::Assertion(
            "repository exposed bytes beyond logical EOF",
        ));
    }
    Ok(published)
}

fn model_write<E>(
    model: &mut Vec<u8>,
    offset: u64,
    data: &[u8],
) -> Result<(), RepositoryConformanceError<E>> {
    let offset = usize::try_from(offset)
        .map_err(|_| RepositoryConformanceError::Assertion("model offset overflowed"))?;
    let end = offset
        .checked_add(data.len())
        .ok_or(RepositoryConformanceError::Assertion(
            "model write end overflowed",
        ))?;
    if model.len() < end {
        model.resize(end, 0);
    }
    model[offset..end].copy_from_slice(data);
    Ok(())
}

async fn assert_range<S: TargetStore>(
    repository: &ContentRepository<S>,
    content: &crate::ContentRef,
    offset: u64,
    length: usize,
    expected: &[u8],
) -> Result<(), RepositoryConformanceError<S::Error>> {
    let actual = repository
        .read(content, offset, length)
        .await
        .map_err(RepositoryConformanceError::Storage)?;
    if actual != expected {
        return Err(RepositoryConformanceError::Assertion(
            "BlockSplit boundary read differs from byte-vector model",
        ));
    }
    Ok(())
}

fn open_repository<S: TargetStore>(
    target: S,
    prefix: &str,
    method: StorageMethod,
) -> Result<ContentRepository<S>, RepositoryConformanceError<S::Error>> {
    ContentRepository::new(
        target,
        prefix,
        CreationDefaults::new(method),
        StorageLimits::default(),
    )
    .map_err(RepositoryConformanceError::Configuration)
}

const fn opposite(method: StorageMethod) -> StorageMethod {
    match method {
        StorageMethod::Raw => StorageMethod::BlockSplit,
        StorageMethod::BlockSplit => StorageMethod::Raw,
    }
}
