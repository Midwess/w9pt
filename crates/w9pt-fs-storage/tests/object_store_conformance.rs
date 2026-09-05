#![allow(missing_docs)]

use w9pt_fs_storage::{
    CompareExchange, ConfigurationError, ObjectKey, ObjectRange, ObjectVersion, PutIfAbsent,
    TargetGuarantees, TargetObject, TargetOperation, TargetStore,
    testing::{
        FailureTiming, MemoryTarget, MemoryTargetError, RepositoryConformanceError, block_on,
        check_repository_conformance, check_target_conformance, check_target_pair_conformance,
    },
};

#[derive(Clone, Debug)]
struct UnqualifiedMemoryTarget(MemoryTarget);

impl TargetStore for UnqualifiedMemoryTarget {
    type Error = MemoryTargetError;

    fn guarantees(&self) -> TargetGuarantees {
        TargetGuarantees::NONE
    }

    async fn get(
        &self,
        key: ObjectKey,
        max_bytes: usize,
    ) -> Result<Option<TargetObject>, Self::Error> {
        self.0.get(key, max_bytes).await
    }

    async fn get_range(
        &self,
        key: ObjectKey,
        range: ObjectRange,
    ) -> Result<Option<Vec<u8>>, Self::Error> {
        self.0.get_range(key, range).await
    }

    async fn put_if_absent(
        &self,
        key: ObjectKey,
        bytes: Vec<u8>,
    ) -> Result<PutIfAbsent, Self::Error> {
        self.0.put_if_absent(key, bytes).await
    }

    async fn compare_exchange(
        &self,
        key: ObjectKey,
        expected: Option<ObjectVersion>,
        bytes: Vec<u8>,
    ) -> Result<CompareExchange, Self::Error> {
        self.0.compare_exchange(key, expected, bytes).await
    }
}

#[test]
fn memory_target_passes_reusable_conformance_suite() {
    let target = MemoryTarget::new();
    block_on(check_target_conformance(&target, "conformance/run-1")).unwrap();
}

#[test]
fn memory_target_passes_reusable_repository_conformance() {
    let first = MemoryTarget::new();
    let second = first.clone();
    block_on(check_repository_conformance(
        first,
        second,
        "repository/conformance-1",
    ))
    .unwrap();
}

#[test]
fn repository_conformance_retains_the_writable_guarantee_gate() {
    let backing = MemoryTarget::new();
    let first = UnqualifiedMemoryTarget(backing.clone());
    let second = UnqualifiedMemoryTarget(backing);
    assert!(matches!(
        block_on(check_repository_conformance(
            first,
            second,
            "repository/unqualified",
        )),
        Err(RepositoryConformanceError::Configuration(
            ConfigurationError::MissingTargetGuarantee { .. }
        ))
    ));
}

#[test]
fn independent_memory_clients_pass_paired_conformance_suite() {
    let first = MemoryTarget::new();
    let second = first.clone();
    block_on(check_target_pair_conformance(
        &first,
        &second,
        "conformance/pair-1",
    ))
    .unwrap();
}

#[test]
fn ambiguous_committed_cas_can_be_resolved_by_readback() {
    let target = MemoryTarget::new();
    let key = ObjectKey::new("conformance/ambiguous").unwrap();
    let CompareExchange::Replaced { version } =
        block_on(target.compare_exchange(key.clone(), None, b"old".to_vec())).unwrap()
    else {
        panic!("fresh key must be created");
    };

    target
        .inject_failure(TargetOperation::CompareExchange, FailureTiming::After)
        .unwrap();
    assert_eq!(
        block_on(target.compare_exchange(key.clone(), Some(version), b"new".to_vec(),)).unwrap(),
        CompareExchange::Ambiguous
    );

    let readback = block_on(target.get(key, 3)).unwrap().unwrap();
    assert_eq!(readback.bytes(), b"new");
}
