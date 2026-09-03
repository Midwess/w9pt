#![allow(missing_docs)]

use w9pt_storage::{
    BLOCK_SIZE_V1, ContentRef, ContentRepository, CreationDefaults, FileId, MutationId,
    Publication, PublishedContent, StorageLimits, StorageMethod,
    testing::{MemoryTarget, TargetTraceEvent, block_on},
};

enum Operation {
    Write { offset: usize, bytes: Vec<u8> },
    Truncate(usize),
}

struct ReplayResult {
    content: ContentRef,
    bytes: Vec<u8>,
    trace: Vec<TargetTraceEvent>,
}

fn operations() -> Vec<Operation> {
    let block = BLOCK_SIZE_V1 as usize;
    vec![
        Operation::Write {
            offset: block - 1,
            bytes: b"cross".to_vec(),
        },
        Operation::Truncate(block + 9),
        Operation::Write {
            offset: block * 2 + 3,
            bytes: b"sparse".to_vec(),
        },
        Operation::Truncate(17),
        Operation::Write {
            offset: 4,
            bytes: b"final".to_vec(),
        },
    ]
}

fn publish(
    repository: &ContentRepository<MemoryTarget>,
    current: Option<&PublishedContent>,
    mutation: MutationId,
    prepared: &w9pt_storage::PreparedContent,
) -> PublishedContent {
    let result = match current {
        Some(current) => block_on(repository.publisher().replace(current, mutation, prepared)),
        None => block_on(repository.publisher().create(
            prepared.content().file_id(),
            mutation,
            prepared,
        )),
    }
    .unwrap();
    let Publication::Published(published) = result else {
        panic!("single-writer deterministic replay must publish");
    };
    published
}

fn run(method: StorageMethod) -> ReplayResult {
    let target = MemoryTarget::new();
    let repository = ContentRepository::new(
        target.clone(),
        "replay",
        CreationDefaults::new(method),
        StorageLimits::default(),
    )
    .unwrap();
    let file_id = FileId::from_u128(1);
    let initial_mutation = MutationId::from_u128(1);
    let initial =
        block_on(repository.prepare_create(file_id, initial_mutation, 0, b"seed")).unwrap();
    let mut published = publish(&repository, None, initial_mutation, &initial);
    let mut model = b"seed".to_vec();
    assert_eq!(
        block_on(repository.read(published.content(), 0, model.len() + 1)).unwrap(),
        model
    );

    for (index, operation) in operations().into_iter().enumerate() {
        let mutation = MutationId::from_u128(u128::try_from(index).unwrap() + 2);
        let prepared = match operation {
            Operation::Write { offset, bytes } => {
                let end = offset.checked_add(bytes.len()).unwrap();
                if end > model.len() {
                    model.resize(end, 0);
                }
                model[offset..end].copy_from_slice(&bytes);
                block_on(repository.prepare_write(
                    published.content(),
                    mutation,
                    0,
                    u64::try_from(offset).unwrap(),
                    &bytes,
                ))
                .unwrap()
            }
            Operation::Truncate(size) => {
                model.resize(size, 0);
                block_on(repository.prepare_truncate(
                    published.content(),
                    mutation,
                    0,
                    u64::try_from(size).unwrap(),
                ))
                .unwrap()
            }
        };
        assert!(prepared.content_changed());
        published = publish(&repository, Some(&published), mutation, &prepared);
        assert_eq!(published.content().logical_size(), model.len() as u64);
        assert_eq!(
            block_on(repository.read(published.content(), 0, model.len() + 1)).unwrap(),
            model
        );
    }
    block_on(repository.sync_content(published.content())).unwrap();
    let content = published.content().clone();
    drop(repository);

    let reopened = ContentRepository::new(
        target.clone(),
        "replay",
        CreationDefaults::new(method),
        StorageLimits::default(),
    )
    .unwrap();
    let reopened_published = block_on(reopened.publisher().load(file_id))
        .unwrap()
        .unwrap();
    assert_eq!(reopened_published.content(), &content);
    let bytes = block_on(reopened.read(&content, 0, model.len() + 1)).unwrap();
    assert_eq!(bytes, model);
    let trace = target.trace().unwrap();

    ReplayResult {
        content,
        bytes,
        trace,
    }
}

#[test]
fn both_layouts_publish_and_replay_identical_target_sequences() {
    for method in [StorageMethod::Raw, StorageMethod::BlockSplit] {
        let first = run(method);
        let second = run(method);
        assert_eq!(
            first.content, second.content,
            "{method:?} content reference"
        );
        assert_eq!(first.bytes, second.bytes, "{method:?} logical bytes");
        assert_eq!(first.trace, second.trace, "{method:?} target trace");
    }
}
