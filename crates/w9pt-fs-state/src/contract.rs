//! Enforceable adapter guarantees and writer topology.

use core::fmt;

use crate::StateLimits;

/// Concurrency topology implemented by a state authority.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WriterTopology {
    /// Independent clients may commit; conflicts are serialized or reported.
    SerializableMultiWriter,
    /// One filesystem-wide fenced writer may commit at a time.
    SingleFencedWriter,
}

/// Explicit semantic guarantees proven by an adapter implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateStoreGuarantees {
    /// Authoritative point reads are linearizable.
    pub linearizable_reads: bool,
    /// Every read batch observes exactly one state revision.
    pub consistent_read_batches: bool,
    /// Multi-record commits are serializable and atomic.
    pub serializable_commits: bool,
    /// Commit success is acknowledged only after restart-durable persistence.
    pub durable_commit_acknowledgment: bool,
    /// State, mutation result, and change event publish atomically.
    pub atomic_result_publication: bool,
    /// Revisions and record versions remain stable and monotonic.
    pub stable_revisions: bool,
    /// Fence allocation remains monotonic across expiry, release, and restart.
    pub monotonic_fencing: bool,
    /// Reads, commits, leases, and retained history enforce configured bounds.
    pub bounded_operations: bool,
}

impl StateStoreGuarantees {
    /// Complete guarantee set required from writable production adapters.
    pub const PRODUCTION_REQUIRED: Self = Self {
        linearizable_reads: true,
        consistent_read_batches: true,
        serializable_commits: true,
        durable_commit_acknowledgment: true,
        atomic_result_publication: true,
        stable_revisions: true,
        monotonic_fencing: true,
        bounded_operations: true,
    };

    /// Semantic guarantees of the deterministic process-lifetime reference authority.
    pub const MEMORY_REFERENCE: Self = Self {
        durable_commit_acknowledgment: false,
        ..Self::PRODUCTION_REQUIRED
    };
}

/// Deployment class preventing a test authority from claiming production durability.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AuthorityClass {
    /// Restart-durable adapter eligible for production capability evaluation.
    Production,
    /// Process-lifetime deterministic semantic reference.
    DeterministicReference,
}

/// Validated, enforceable state-store contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateStoreContract {
    topology: WriterTopology,
    authority_class: AuthorityClass,
    guarantees: StateStoreGuarantees,
    limits: StateLimits,
}

impl StateStoreContract {
    /// Validates all mandatory guarantees for a writable production adapter.
    pub fn production(
        topology: WriterTopology,
        guarantees: StateStoreGuarantees,
        limits: StateLimits,
    ) -> Result<Self, InvalidStateStoreContract> {
        validate_required(guarantees)?;
        validate_expressive_limits(limits)?;
        Ok(Self {
            topology,
            authority_class: AuthorityClass::Production,
            guarantees,
            limits,
        })
    }

    /// Describes the deterministic memory authority without claiming restart durability.
    pub const fn deterministic_reference(topology: WriterTopology, limits: StateLimits) -> Self {
        Self {
            topology,
            authority_class: AuthorityClass::DeterministicReference,
            guarantees: StateStoreGuarantees::MEMORY_REFERENCE,
            limits,
        }
    }

    /// Returns the declared writer topology.
    pub const fn writer_topology(self) -> WriterTopology {
        self.topology
    }

    /// Returns whether this is a production or reference authority.
    pub const fn authority_class(self) -> AuthorityClass {
        self.authority_class
    }

    /// Returns the adapter's proven guarantees.
    pub const fn guarantees(self) -> StateStoreGuarantees {
        self.guarantees
    }

    /// Returns all request and retention bounds.
    pub const fn limits(self) -> StateLimits {
        self.limits
    }

    /// Reports whether the contract may back production durability capabilities.
    pub const fn is_production_ready(self) -> bool {
        matches!(self.authority_class, AuthorityClass::Production)
    }
}

fn validate_expressive_limits(limits: StateLimits) -> Result<(), InvalidStateStoreContract> {
    for (field, actual, minimum) in [
        ("max_changes", u64::from(limits.max_changes()), 4),
        ("max_change_keys", u64::from(limits.max_change_keys()), 5),
        (
            "max_xattrs_per_request",
            u64::from(limits.max_xattrs_per_request()),
            2,
        ),
    ] {
        if actual < minimum {
            return Err(InvalidStateStoreContract::InsufficientLimit {
                field,
                actual,
                minimum,
            });
        }
    }
    let mutation_key_minimum = u64::from(limits.max_changes()) + 1;
    if u64::from(limits.max_change_keys()) < mutation_key_minimum {
        return Err(InvalidStateStoreContract::InsufficientLimit {
            field: "max_change_keys",
            actual: u64::from(limits.max_change_keys()),
            minimum: mutation_key_minimum,
        });
    }
    Ok(())
}

fn validate_required(guarantees: StateStoreGuarantees) -> Result<(), InvalidStateStoreContract> {
    for (present, required) in [
        (
            guarantees.linearizable_reads,
            RequiredGuarantee::LinearizableReads,
        ),
        (
            guarantees.consistent_read_batches,
            RequiredGuarantee::ConsistentReadBatches,
        ),
        (
            guarantees.serializable_commits,
            RequiredGuarantee::SerializableCommits,
        ),
        (
            guarantees.durable_commit_acknowledgment,
            RequiredGuarantee::DurableCommitAcknowledgment,
        ),
        (
            guarantees.atomic_result_publication,
            RequiredGuarantee::AtomicResultPublication,
        ),
        (
            guarantees.stable_revisions,
            RequiredGuarantee::StableRevisions,
        ),
        (
            guarantees.monotonic_fencing,
            RequiredGuarantee::MonotonicFencing,
        ),
        (
            guarantees.bounded_operations,
            RequiredGuarantee::BoundedOperations,
        ),
    ] {
        if !present {
            return Err(InvalidStateStoreContract::MissingGuarantee(required));
        }
    }
    Ok(())
}

/// Mandatory guarantee categories reported by contract validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredGuarantee {
    /// Linearizable authoritative reads.
    LinearizableReads,
    /// One-revision read batches.
    ConsistentReadBatches,
    /// Serializable atomic commits.
    SerializableCommits,
    /// Restart-durable success acknowledgment.
    DurableCommitAcknowledgment,
    /// Atomic state/result/change publication.
    AtomicResultPublication,
    /// Stable monotonic revisions.
    StableRevisions,
    /// Monotonic fencing.
    MonotonicFencing,
    /// Enforced request and retention bounds.
    BoundedOperations,
}

/// A writable production adapter omitted a mandatory guarantee.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidStateStoreContract {
    /// The named normative guarantee is absent.
    MissingGuarantee(RequiredGuarantee),
    /// Configured bound cannot express required atomic filesystem transitions.
    InsufficientLimit {
        /// State-limit field.
        field: &'static str,
        /// Configured value.
        actual: u64,
        /// Minimum required value.
        minimum: u64,
    },
}

impl fmt::Display for InvalidStateStoreContract {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingGuarantee(guarantee) => {
                write!(
                    formatter,
                    "state store is missing required guarantee {guarantee:?}"
                )
            }
            Self::InsufficientLimit {
                field,
                actual,
                minimum,
            } => write!(
                formatter,
                "state limit {field} is {actual}, below required minimum {minimum}"
            ),
        }
    }
}

impl std::error::Error for InvalidStateStoreContract {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_contract_rejects_every_missing_promise() {
        let mut guarantees = StateStoreGuarantees::PRODUCTION_REQUIRED;
        guarantees.durable_commit_acknowledgment = false;
        assert_eq!(
            StateStoreContract::production(
                WriterTopology::SerializableMultiWriter,
                guarantees,
                StateLimits::default(),
            ),
            Err(InvalidStateStoreContract::MissingGuarantee(
                RequiredGuarantee::DurableCommitAcknowledgment
            ))
        );
    }

    #[test]
    fn memory_contract_is_explicitly_not_production_durable() {
        let contract = StateStoreContract::deterministic_reference(
            WriterTopology::SerializableMultiWriter,
            StateLimits::default(),
        );
        assert!(!contract.is_production_ready());
        assert!(!contract.guarantees().durable_commit_acknowledgment);
    }

    #[test]
    fn production_contract_rejects_limits_that_cannot_publish_a_ledger_key() {
        let limits = StateLimits::new(crate::StateLimitValues {
            max_changes: 4,
            max_change_keys: 4,
            ..crate::StateLimitValues::default()
        })
        .unwrap();
        assert!(matches!(
            StateStoreContract::production(
                WriterTopology::SerializableMultiWriter,
                StateStoreGuarantees::PRODUCTION_REQUIRED,
                limits,
            ),
            Err(InvalidStateStoreContract::InsufficientLimit {
                field: "max_change_keys",
                minimum: 5,
                ..
            })
        ));
    }
}
