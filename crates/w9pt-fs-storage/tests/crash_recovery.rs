#![allow(missing_docs)]

use w9pt_fs_storage::{
    BLOCK_SIZE, ContentRepository, CreationDefaults, FileId, MutationId, Publication,
    StorageLimits, StorageMethod, TargetOperation,
    testing::{FailureTiming, MemoryTarget, block_on},
};

#[derive(Clone, Copy, Debug)]
enum FailureStage {
    Payload(FailureTiming),
    Manifest(FailureTiming),
    HeadBefore,
    HeadAfter,
}

fn repository(target: MemoryTarget) -> ContentRepository<MemoryTarget> {
    ContentRepository::new(
        target,
        "private",
        CreationDefaults::new(StorageMethod::BlockSplit),
        StorageLimits::default(),
    )
    .unwrap()
}

fn assert_reopen_is_atomic(stage: FailureStage) {
    let target = MemoryTarget::new();
    let repo = repository(target.clone());
    let file_id = FileId::from_u128(1);
    let initial_mutation = MutationId::from_u128(1);
    let update_mutation = MutationId::from_u128(2);
    let old_bytes = vec![1; BLOCK_SIZE as usize];
    let new_bytes = vec![2; BLOCK_SIZE as usize];
    let initial = block_on(repo.prepare_create(file_id, initial_mutation, 0, &old_bytes)).unwrap();
    let Publication::Published(published) =
        block_on(repo.publisher().create(file_id, initial_mutation, &initial)).unwrap()
    else {
        panic!("fresh head must publish");
    };

    let update_identity = w9pt_fs_storage::PreparationIdentity::for_write(
        update_mutation,
        published.content(),
        0,
        &new_bytes,
    )
    .unwrap();
    let payload_key = repo.keys().block_payload(file_id, update_identity, 0, 0);
    let manifest_key = repo.keys().manifest(file_id, update_identity, 0);
    let head_key = repo.keys().head(file_id);
    match stage {
        FailureStage::Payload(timing) => target
            .inject_failure_for(TargetOperation::PutIfAbsent, timing, payload_key)
            .unwrap(),
        FailureStage::Manifest(timing) => target
            .inject_failure_for(TargetOperation::PutIfAbsent, timing, manifest_key)
            .unwrap(),
        FailureStage::HeadBefore => target
            .inject_failure_for(
                TargetOperation::CompareExchange,
                FailureTiming::Before,
                head_key.clone(),
            )
            .unwrap(),
        FailureStage::HeadAfter => {
            target
                .inject_failure_for(
                    TargetOperation::CompareExchange,
                    FailureTiming::After,
                    head_key.clone(),
                )
                .unwrap();
            target
                .inject_failure_for(TargetOperation::Get, FailureTiming::Before, head_key)
                .unwrap();
        }
    }

    let update = block_on(async {
        let prepared = repo
            .prepare_write(published.content(), update_mutation, 0, 0, &new_bytes)
            .await?;
        repo.publisher()
            .replace(&published, update_mutation, &prepared)
            .await
    });
    match stage {
        FailureStage::Payload(FailureTiming::After)
        | FailureStage::Manifest(FailureTiming::After) => {
            assert!(
                update.is_ok(),
                "exact immutable readback resolves response loss"
            )
        }
        FailureStage::HeadAfter => assert!(update.is_err(), "readback failure stays ambiguous"),
        FailureStage::Payload(FailureTiming::Before)
        | FailureStage::Manifest(FailureTiming::Before)
        | FailureStage::HeadBefore => {
            assert!(update.is_err())
        }
    }
    drop(repo);

    let reopened = repository(target);
    let current = block_on(reopened.publisher().load(file_id))
        .unwrap()
        .expect("the original head remains or the replacement committed");
    let visible = block_on(reopened.read(current.content(), 0, new_bytes.len())).unwrap();
    assert!(
        visible == old_bytes || visible == new_bytes,
        "{stage:?} exposed a mixed content version"
    );
    match stage {
        FailureStage::Payload(FailureTiming::After)
        | FailureStage::Manifest(FailureTiming::After)
        | FailureStage::HeadAfter => assert_eq!(visible, new_bytes),
        FailureStage::Payload(FailureTiming::Before)
        | FailureStage::Manifest(FailureTiming::Before)
        | FailureStage::HeadBefore => {
            assert_eq!(visible, old_bytes)
        }
    }
}

#[test]
fn immutable_failures_reopen_old_or_exactly_resolved_new_content() {
    for stage in [
        FailureStage::Payload(FailureTiming::Before),
        FailureStage::Payload(FailureTiming::After),
        FailureStage::Manifest(FailureTiming::Before),
        FailureStage::Manifest(FailureTiming::After),
    ] {
        assert_reopen_is_atomic(stage);
    }
}

#[test]
fn failures_before_and_after_head_publication_reopen_old_or_complete_new() {
    assert_reopen_is_atomic(FailureStage::HeadBefore);
    assert_reopen_is_atomic(FailureStage::HeadAfter);
}
