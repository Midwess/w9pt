#![allow(missing_docs)]

use w9pt_fs_storage::{
    BLOCK_SIZE, ConflictError, ContentRepository, CreationDefaults, FileId, MutationId,
    Publication, PublishedContent, StorageError, StorageLimitValues, StorageLimits, StorageMethod,
    testing::{MemoryTarget, block_on},
};

fn repository(target: MemoryTarget, max_publish_retries: u32) -> ContentRepository<MemoryTarget> {
    let values = StorageLimitValues {
        max_publish_retries,
        ..StorageLimitValues::default()
    };
    ContentRepository::new(
        target,
        "private",
        CreationDefaults::new(StorageMethod::BlockSplit),
        StorageLimits::new(values).unwrap(),
    )
    .unwrap()
}

fn create_published(
    repository: &ContentRepository<MemoryTarget>,
    file_id: FileId,
    bytes: &[u8],
) -> PublishedContent {
    let mutation = MutationId::from_u128(1);
    let prepared = block_on(repository.prepare_create(file_id, mutation, 0, bytes)).unwrap();
    let Publication::Published(published) =
        block_on(repository.publisher().create(file_id, mutation, &prepared)).unwrap()
    else {
        panic!("fresh file must publish");
    };
    published
}

#[test]
fn disjoint_writer_conflict_rebases_without_losing_either_write() {
    let target = MemoryTarget::new();
    let writer_a = repository(target.clone(), 3);
    let writer_b = repository(target.clone(), 3);
    let reopened = repository(target, 3);
    let file_id = FileId::from_u128(1);
    let initial = create_published(&writer_a, file_id, b"");
    let mut interfere = Some(initial.clone());
    let a_mutation = MutationId::from_u128(2);
    let b_mutation = MutationId::from_u128(3);

    let published = block_on(writer_b.publisher().mutate_rebased(
        file_id,
        b_mutation,
        move |repository, base, attempt| {
            let writer_a = writer_a.clone();
            let current_for_a = if attempt == 0 { interfere.take() } else { None };
            async move {
                let prepared_b = repository
                    .prepare_write(&base, b_mutation, attempt, 8, b"right")
                    .await?;
                if let Some(current) = current_for_a {
                    let prepared_a = writer_a
                        .prepare_write(current.content(), a_mutation, 0, 0, b"left")
                        .await?;
                    assert!(matches!(
                        writer_a
                            .publisher()
                            .replace(&current, a_mutation, &prepared_a)
                            .await?,
                        Publication::Published(_)
                    ));
                }
                Ok(prepared_b)
            }
        },
    ))
    .unwrap();

    assert_eq!(published.content().generation(), 3);
    assert_eq!(
        block_on(reopened.read(published.content(), 0, 32)).unwrap(),
        b"left\0\0\0\0right"
    );
}

#[test]
fn overlapping_writer_conflict_obeys_successful_publication_order() {
    let target = MemoryTarget::new();
    let writer_a = repository(target.clone(), 3);
    let writer_b = repository(target.clone(), 3);
    let reopened = repository(target, 3);
    let file_id = FileId::from_u128(1);
    let initial = create_published(&writer_a, file_id, b"seed");
    let mut interfere = Some(initial.clone());
    let a_mutation = MutationId::from_u128(2);
    let b_mutation = MutationId::from_u128(3);

    let published = block_on(writer_b.publisher().mutate_rebased(
        file_id,
        b_mutation,
        move |repository, base, attempt| {
            let writer_a = writer_a.clone();
            let current_for_a = if attempt == 0 { interfere.take() } else { None };
            async move {
                let prepared_b = repository
                    .prepare_write(&base, b_mutation, attempt, 0, b"BBBB")
                    .await?;
                if let Some(current) = current_for_a {
                    let prepared_a = writer_a
                        .prepare_write(current.content(), a_mutation, 0, 0, b"AAAA")
                        .await?;
                    assert!(matches!(
                        writer_a
                            .publisher()
                            .replace(&current, a_mutation, &prepared_a)
                            .await?,
                        Publication::Published(_)
                    ));
                }
                Ok(prepared_b)
            }
        },
    ))
    .unwrap();

    assert_eq!(
        block_on(reopened.read(published.content(), 0, 4)).unwrap(),
        b"BBBB"
    );
    assert_eq!(published.content().generation(), 3);
}

#[test]
fn repeated_conflicts_stop_at_the_configured_bound() {
    let target = MemoryTarget::new();
    let repository = repository(target.clone(), 1);
    let file_id = FileId::from_u128(1);
    create_published(&repository, file_id, b"seed");
    let mutation = MutationId::from_u128(2);
    let head_key = repository.keys().head(file_id);

    let result = block_on(repository.publisher().mutate_rebased(
        file_id,
        mutation,
        move |repository, base, attempt| {
            let target = target.clone();
            let head_key = head_key.clone();
            async move {
                let prepared = repository
                    .prepare_write(&base, mutation, attempt, 0, b"new")
                    .await?;
                let same_head = target.inspect(&head_key).unwrap().unwrap();
                assert!(target.corrupt(&head_key, same_head).unwrap());
                Ok(prepared)
            }
        },
    ));
    assert!(matches!(
        result,
        Err(StorageError::Conflict(ConflictError { conflicts: 2 }))
    ));
}

#[test]
fn independent_clients_reprepare_disjoint_writes_across_branch_pages() {
    let target = MemoryTarget::new();
    let writer_a = repository(target.clone(), 3);
    let writer_b = repository(target.clone(), 3);
    let observer = repository(target, 3);
    let file_id = FileId::from_u128(20);
    let initial = create_published(&writer_a, file_id, b"");
    let mut interfere = Some(initial);
    let a_mutation = MutationId::from_u128(21);
    let b_mutation = MutationId::from_u128(22);
    let a_offset = u64::from(BLOCK_SIZE) * 127;
    let b_offset = u64::from(BLOCK_SIZE) * 16_384;

    let published = block_on(writer_b.publisher().mutate_rebased(
        file_id,
        b_mutation,
        move |repository, base, attempt| {
            let writer_a = writer_a.clone();
            let current_for_a = if attempt == 0 { interfere.take() } else { None };
            async move {
                let prepared_b = repository
                    .prepare_write(&base, b_mutation, attempt, b_offset, b"B")
                    .await?;
                if let Some(current) = current_for_a {
                    let prepared_a = writer_a
                        .prepare_write(current.content(), a_mutation, 0, a_offset, b"A")
                        .await?;
                    assert!(matches!(
                        writer_a
                            .publisher()
                            .replace(&current, a_mutation, &prepared_a)
                            .await?,
                        Publication::Published(_)
                    ));
                }
                Ok(prepared_b)
            }
        },
    ))
    .unwrap();
    assert_eq!(
        block_on(observer.read(published.content(), a_offset, 1)).unwrap(),
        b"A"
    );
    assert_eq!(
        block_on(observer.read(published.content(), b_offset, 1)).unwrap(),
        b"B"
    );
}
