#![allow(missing_docs)]

use w9pt_storage::{
    CompareExchange, ObjectKey, TargetOperation, TargetStore,
    testing::{FailureTiming, MemoryTarget, block_on, check_target_conformance},
};

#[test]
fn memory_target_passes_reusable_conformance_suite() {
    let target = MemoryTarget::new();
    block_on(check_target_conformance(&target, "conformance/run-1")).unwrap();
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
