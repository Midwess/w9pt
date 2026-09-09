//! Capability derivation from implemented semantics and backend guarantees.

use w9pt::filesystem::{Capability, CapabilitySet};
use w9pt_fs_state::StateStoreContract;
use w9pt_fs_storage::TargetGuarantees;

const SEMANTIC_SLICE: [Capability; 8] = [
    Capability::StableIdentity,
    Capability::AtomicAuthorization,
    Capability::AtomicNamespace,
    Capability::AtomicSetattr,
    Capability::PositionedIo,
    Capability::OpenUnlinked,
    Capability::DurableData,
    Capability::DurableMetadata,
];

/// Derives the capability set implemented by the first semantic-engine slice.
///
/// The result is the intersection of the implementation, export ceiling, state
/// contract, target contract, and read-only export policy. Deferred operation
/// families are never included.
pub fn derive_capabilities(
    state: StateStoreContract,
    target: TargetGuarantees,
    policy_ceiling: CapabilitySet,
    read_only: bool,
) -> CapabilitySet {
    let state_guarantees = state.guarantees();
    let semantic_state = state_guarantees.linearizable_reads
        && state_guarantees.consistent_read_batches
        && state_guarantees.serializable_commits
        && state_guarantees.atomic_result_publication
        && state_guarantees.stable_revisions
        && state_guarantees.monotonic_fencing
        && state_guarantees.bounded_operations;
    if !semantic_state {
        return CapabilitySet::NONE;
    }

    let durable_data = target.durable_writes
        && target.atomic_put_if_absent
        && target.atomic_compare_exchange
        && target.read_after_write;
    if !durable_data {
        return CapabilitySet::NONE;
    }
    let durable_metadata = state.is_production_ready()
        && state_guarantees.durable_commit_acknowledgment
        && durable_data;

    let mut semantic = CapabilitySet::NONE;
    for capability in SEMANTIC_SLICE {
        if capability == Capability::DurableMetadata && !durable_metadata {
            continue;
        }
        if policy_ceiling.contains(capability) {
            semantic = semantic.with(capability);
        }
    }

    let stable_authorized = semantic.contains(Capability::StableIdentity)
        && semantic.contains(Capability::AtomicAuthorization);
    let namespace = semantic.contains(Capability::AtomicAuthorization)
        && semantic.contains(Capability::AtomicNamespace);
    let positioned = semantic.contains(Capability::AtomicAuthorization)
        && semantic.contains(Capability::PositionedIo);
    let mut capabilities = semantic;
    for (capability, enabled) in [
        (Capability::Walk, stable_authorized),
        (
            Capability::Open,
            stable_authorized && semantic.contains(Capability::OpenUnlinked),
        ),
        (Capability::Read, positioned),
        (
            Capability::Readdir,
            semantic.contains(Capability::AtomicAuthorization),
        ),
        (
            Capability::Fsync,
            semantic.contains(Capability::DurableData),
        ),
        (
            Capability::Getattr,
            semantic.contains(Capability::AtomicAuthorization),
        ),
        (
            Capability::Readlink,
            semantic.contains(Capability::AtomicAuthorization),
        ),
        (
            Capability::Create,
            !read_only
                && stable_authorized
                && namespace
                && semantic.contains(Capability::OpenUnlinked),
        ),
        (Capability::Mkdir, !read_only && namespace),
        (Capability::Symlink, !read_only && namespace),
        (Capability::Write, !read_only && positioned),
        (
            Capability::Setattr,
            !read_only
                && semantic.contains(Capability::AtomicAuthorization)
                && semantic.contains(Capability::AtomicSetattr),
        ),
        (Capability::RenameAt, !read_only && namespace),
        (
            Capability::UnlinkAt,
            !read_only && namespace && semantic.contains(Capability::OpenUnlinked),
        ),
        (Capability::Link, !read_only && namespace),
    ] {
        if enabled && policy_ceiling.contains(capability) {
            capabilities = capabilities.with(capability);
        }
    }
    capabilities
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt_fs_state::{StateLimits, WriterTopology};

    #[test]
    fn reference_and_read_only_contracts_do_not_overclaim() {
        let state = StateStoreContract::deterministic_reference(
            WriterTopology::SerializableMultiWriter,
            StateLimits::default(),
        );
        let capabilities =
            derive_capabilities(state, TargetGuarantees::REQUIRED, CapabilitySet::ALL, true);
        assert!(capabilities.contains(Capability::Read));
        assert!(capabilities.contains(Capability::DurableData));
        assert!(!capabilities.contains(Capability::Write));
        assert!(!capabilities.contains(Capability::DurableMetadata));
        assert!(!capabilities.contains(Capability::Statfs));
        assert!(!capabilities.contains(Capability::Xattr));
        assert!(!capabilities.contains(Capability::Cancellation));

        let incomplete_ceiling = CapabilitySet::NONE.with(Capability::Open);
        assert_eq!(
            derive_capabilities(state, TargetGuarantees::REQUIRED, incomplete_ceiling, false,),
            CapabilitySet::NONE
        );
    }

    #[test]
    fn production_contract_exposes_only_the_complete_first_slice() {
        let state = StateStoreContract::production(
            WriterTopology::SerializableMultiWriter,
            w9pt_fs_state::StateStoreGuarantees::PRODUCTION_REQUIRED,
            StateLimits::default(),
        )
        .unwrap();
        let capabilities =
            derive_capabilities(state, TargetGuarantees::REQUIRED, CapabilitySet::ALL, false);
        for capability in [
            Capability::Walk,
            Capability::Open,
            Capability::Create,
            Capability::Write,
            Capability::AtomicNamespace,
            Capability::AtomicSetattr,
            Capability::DurableData,
            Capability::DurableMetadata,
        ] {
            assert!(capabilities.contains(capability));
        }
        for capability in [
            Capability::Mknod,
            Capability::Statfs,
            Capability::Rename,
            Capability::Remove,
            Capability::Xattr,
            Capability::Lock,
            Capability::Cancellation,
        ] {
            assert!(!capabilities.contains(capability));
        }
        assert_eq!(
            derive_capabilities(state, TargetGuarantees::NONE, CapabilitySet::ALL, false,),
            CapabilitySet::NONE
        );
    }
}
