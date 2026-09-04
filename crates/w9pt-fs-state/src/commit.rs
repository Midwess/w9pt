//! Declarative atomic commit protocol values.
//!
//! Adapters must follow [`COMMIT_PROTOCOL_ORDER`]. In particular, exact retained
//! mutations replay before fence validation, and records, the terminal result,
//! and one whole-commit change event become visible in one atomic publication.

use core::{fmt, num::NonZeroU64};
use std::collections::BTreeSet;

use crate::{
    ClientIncarnationId, DataGeneration, DirectoryGeneration, FilesystemId, InodeGeneration,
    InodeId, InodeKind, InodeRecord, InodeTimes, LockId, MutationRecord, MutationResult,
    MutationRetention, RecordFamily, RecordKey, RecordRevision, RecordValidationError,
    RequestFingerprint, StateLimitError, StateLimitKind, StateLimits, StateRecord, StateRevision,
    WriterFence,
};

/// Normative phases of every commit implementation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CommitProtocolPhase {
    /// Look up the durable mutation ledger and return exact replay or mismatch first.
    LedgerLookup,
    /// Validate all request counts, bytes, variants, and arithmetic.
    BoundedPreflight,
    /// Validate the exact current nonexpired writer authority.
    FenceValidation,
    /// Evaluate every typed authoritative observation.
    Preconditions,
    /// Stage all changes and validate the resulting cross-record state.
    StageCompleteTransition,
    /// Allocate one monotonic state and record revision.
    AllocateRevision,
    /// Atomically expose records, terminal result, and one change event.
    PublishRecordsResultAndChange,
    /// Acknowledge success only at the contract's advertised durable boundary.
    DurableAcknowledge,
}

/// Exact mandatory phase order for adapter commit implementations.
pub const COMMIT_PROTOCOL_ORDER: [CommitProtocolPhase; 8] = [
    CommitProtocolPhase::LedgerLookup,
    CommitProtocolPhase::BoundedPreflight,
    CommitProtocolPhase::FenceValidation,
    CommitProtocolPhase::Preconditions,
    CommitProtocolPhase::StageCompleteTransition,
    CommitProtocolPhase::AllocateRevision,
    CommitProtocolPhase::PublishRecordsResultAndChange,
    CommitProtocolPhase::DurableAcknowledge,
];

/// Stable idempotent identity and replay metadata for one filesystem mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationContext {
    /// Globally stable mutation identity.
    pub mutation_id: w9pt_storage::MutationId,
    /// Fingerprint of the complete semantic request.
    pub fingerprint: RequestFingerprint,
    /// Stable client/session lineage.
    pub client_incarnation: ClientIncarnationId,
    /// Caller-defined minimum retention horizon.
    pub retention: MutationRetention,
}

impl MutationContext {
    /// Creates the complete durable identity of one semantic mutation request.
    pub const fn new(
        mutation_id: w9pt_storage::MutationId,
        fingerprint: RequestFingerprint,
        client_incarnation: ClientIncarnationId,
        retention: MutationRetention,
    ) -> Self {
        Self {
            mutation_id,
            fingerprint,
            client_incarnation,
            retention,
        }
    }

    /// Classifies a retained ledger record before any fence validation.
    pub fn classify_record(&self, record: &MutationRecord) -> MutationReplay {
        if self.mutation_id != record.mutation_id() {
            return MutationReplay::Mismatch(MutationMismatch::MutationId);
        }
        if self.fingerprint != record.fingerprint() {
            return MutationReplay::Mismatch(MutationMismatch::Fingerprint);
        }
        if self.client_incarnation != record.client_incarnation() {
            return MutationReplay::Mismatch(MutationMismatch::ClientIncarnation);
        }
        if self.retention != record.retention() {
            return MutationReplay::Mismatch(MutationMismatch::Retention);
        }
        MutationReplay::Exact(CommittedMutation::from_record(record))
    }
}

/// Typed authoritative observation that must remain true at commit serialization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Precondition {
    /// Semantic record must not exist.
    RecordAbsent(RecordKey),
    /// Semantic record must exist at this exact record revision.
    RecordRevision {
        /// Record key.
        key: RecordKey,
        /// Observed revision.
        expected: RecordRevision,
    },
    /// Inode metadata generation must match exactly.
    InodeGeneration {
        /// Stable inode identity.
        inode_id: InodeId,
        /// Observed metadata generation.
        expected: InodeGeneration,
    },
    /// Regular-file data generation must match; `None` denotes unpublished content.
    DataGeneration {
        /// Stable inode identity.
        inode_id: InodeId,
        /// Observed data generation.
        expected: Option<DataGeneration>,
    },
    /// Directory namespace generation must match exactly.
    DirectoryGeneration {
        /// Stable directory inode.
        inode_id: InodeId,
        /// Observed namespace generation.
        expected: DirectoryGeneration,
    },
    /// Current immutable content base must match exactly.
    ContentBase {
        /// Stable regular-file inode.
        inode_id: InodeId,
        /// Observed content identity or distinguished new-file base.
        expected: w9pt_storage::BaseContentIdentity,
    },
    /// Inode hard-link count must match exactly.
    LinkCount {
        /// Stable inode identity.
        inode_id: InodeId,
        /// Observed link count.
        expected: u64,
    },
    /// Durable open-pin count for one inode must match exactly.
    OpenPinCount {
        /// Stable inode identity.
        inode_id: InodeId,
        /// Observed open-pin count.
        expected: u64,
    },
    /// Current active writer authority must match exactly.
    ExactFence(WriterFence),
}

impl Precondition {
    /// Returns an explicit record key when this condition addresses one full record.
    pub const fn record_key(&self) -> Option<&RecordKey> {
        match self {
            Self::RecordAbsent(key) | Self::RecordRevision { key, .. } => Some(key),
            _ => None,
        }
    }
}

/// Direction and nonzero amount of one checked persistent counter adjustment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterAdjustment {
    /// Add the nonzero amount with checked arithmetic.
    Increase(NonZeroU64),
    /// Subtract the nonzero amount with checked arithmetic.
    Decrease(NonZeroU64),
}

impl CounterAdjustment {
    /// Creates an increase by a nonzero amount.
    pub const fn increase(amount: u64) -> Option<Self> {
        match NonZeroU64::new(amount) {
            Some(amount) => Some(Self::Increase(amount)),
            None => None,
        }
    }

    /// Creates a decrease by a nonzero amount.
    pub const fn decrease(amount: u64) -> Option<Self> {
        match NonZeroU64::new(amount) {
            Some(amount) => Some(Self::Decrease(amount)),
            None => None,
        }
    }

    /// Applies this adjustment without wrapping or underflowing.
    pub const fn apply(self, current: u64) -> Option<u64> {
        match self {
            Self::Increase(amount) => current.checked_add(amount.get()),
            Self::Decrease(amount) => current.checked_sub(amount.get()),
        }
    }
}

/// Dedicated handoff from immutable content preparation to inode publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishContent {
    /// Stable regular-file inode receiving the prepared version.
    pub inode_id: InodeId,
    /// Exact authoritative base against which preparation was performed.
    pub expected_base: w9pt_storage::BaseContentIdentity,
    /// Proof and portable reference returned after immutable dependencies were stored.
    pub prepared: w9pt_storage::PreparedContent,
    /// Logical size published into the inode record.
    pub logical_size: u64,
    /// Data generation published into the inode record.
    pub data_generation: DataGeneration,
    /// Next inode metadata generation published with content.
    pub inode_generation: InodeGeneration,
    /// Complete timestamp set published atomically with content and size.
    pub times: InodeTimes,
}

/// Dedicated atomic publication of one complete xattr staging record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishXattrStaging {
    /// Stable portable staging identity to consume.
    pub staging_id: crate::XattrStagingId,
    /// Inode receiving the published xattr.
    pub inode_id: InodeId,
    /// Exact staged xattr name.
    pub name: crate::XattrName,
}

/// Structurally inconsistent immutable-content publication request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishContentError {
    /// Target inode is not a regular file.
    NotRegularFile,
    /// Preparation and filesystem commit use different mutation identities.
    MutationIdMismatch,
    /// Explicit publication base differs from the preparation identity.
    PreparationBaseMismatch,
    /// Authoritative inode content differs from the expected base.
    AuthoritativeBaseMismatch,
    /// Prepared file identity differs from the inode's explicit content binding.
    ContentFileMismatch,
    /// Requested inode logical size differs from the prepared content reference.
    LogicalSizeMismatch,
    /// Prepared content changed but did not advance exactly one data generation.
    DataGenerationMismatch,
    /// Inode metadata generation did not advance exactly once.
    InodeGenerationMismatch,
    /// Checked generation arithmetic overflowed.
    GenerationOverflow,
}

impl fmt::Display for PublishContentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid prepared-content publication: {self:?}")
    }
}

impl std::error::Error for PublishContentError {}

/// Validates all immutable preparation and authoritative inode bindings.
pub fn validate_publish_content(
    publication: &PublishContent,
    mutation: &MutationContext,
    inode: &InodeRecord,
) -> Result<(), PublishContentError> {
    if inode.kind() != InodeKind::RegularFile {
        return Err(PublishContentError::NotRegularFile);
    }
    let identity = publication.prepared.identity();
    if identity.mutation_id() != mutation.mutation_id {
        return Err(PublishContentError::MutationIdMismatch);
    }
    if identity.base() != publication.expected_base {
        return Err(PublishContentError::PreparationBaseMismatch);
    }
    if inode.content_base() != Some(publication.expected_base) {
        return Err(PublishContentError::AuthoritativeBaseMismatch);
    }
    if inode.content_file_id() != Some(publication.prepared.content().file_id()) {
        return Err(PublishContentError::ContentFileMismatch);
    }
    if publication.logical_size != publication.prepared.content().logical_size() {
        return Err(PublishContentError::LogicalSizeMismatch);
    }
    let current_generation = publication.expected_base.generation();
    let expected_generation = if publication.prepared.content_changed() {
        current_generation
            .checked_add(1)
            .ok_or(PublishContentError::GenerationOverflow)?
    } else {
        current_generation
    };
    if publication.prepared.content().generation() != expected_generation
        || publication.data_generation.get() != expected_generation
    {
        return Err(PublishContentError::DataGenerationMismatch);
    }
    let expected_inode_generation = inode
        .inode_generation()
        .checked_next()
        .map_err(|_| PublishContentError::GenerationOverflow)?;
    if publication.inode_generation != expected_inode_generation {
        return Err(PublishContentError::InodeGenerationMismatch);
    }
    Ok(())
}

/// Declarative checked state change applied only after complete preflight.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateChange {
    /// Insert a semantically keyed record that must be absent.
    Insert {
        /// Exact semantic key.
        key: RecordKey,
        /// Checked tagged value.
        record: StateRecord,
    },
    /// Replace a semantically keyed record that must exist.
    Replace {
        /// Exact semantic key.
        key: RecordKey,
        /// Complete checked replacement value.
        record: StateRecord,
    },
    /// Delete a semantically keyed record that must exist.
    Delete(RecordKey),
    /// Allocate a consecutive range of non-reused directory cookies.
    AdvanceDirectoryCookie {
        /// Number of cookies allocated from the pre-state high-water mark.
        count: NonZeroU64,
    },
    /// Increment the filesystem policy generation.
    BumpFilesystemPolicyGeneration,
    /// Increment an inode metadata generation.
    BumpInodeGeneration(InodeId),
    /// Increment a directory namespace generation.
    BumpDirectoryGeneration(InodeId),
    /// Adjust one inode hard-link count with checked arithmetic.
    AdjustLinkCount {
        /// Stable inode identity.
        inode_id: InodeId,
        /// Nonzero checked adjustment.
        adjustment: CounterAdjustment,
    },
    /// Adjust one orphan's durable open-pin count with checked arithmetic.
    AdjustOpenPinCount {
        /// Stable orphan inode.
        inode_id: InodeId,
        /// Nonzero checked adjustment.
        adjustment: CounterAdjustment,
    },
    /// Increment one lock generation.
    BumpLockGeneration {
        /// Locked inode.
        inode_id: InodeId,
        /// Stable lock identity.
        lock_id: LockId,
    },
    /// Publish already-prepared immutable content and all inode summaries together.
    PublishContent(PublishContent),
    /// Atomically consume complete staging and publish its exact xattr bytes.
    PublishXattrStaging(PublishXattrStaging),
}

impl StateChange {
    /// Returns the exact primary record key affected by this change when known.
    pub fn primary_key(&self, filesystem_id: FilesystemId) -> RecordKey {
        match self {
            Self::Insert { key, .. } | Self::Replace { key, .. } | Self::Delete(key) => key.clone(),
            Self::AdvanceDirectoryCookie { .. } | Self::BumpFilesystemPolicyGeneration => {
                RecordKey::Filesystem(filesystem_id)
            }
            Self::BumpInodeGeneration(inode_id)
            | Self::BumpDirectoryGeneration(inode_id)
            | Self::AdjustLinkCount { inode_id, .. } => RecordKey::Inode(filesystem_id, *inode_id),
            Self::AdjustOpenPinCount { inode_id, .. } => {
                RecordKey::Orphan(filesystem_id, *inode_id)
            }
            Self::BumpLockGeneration { inode_id, lock_id } => {
                RecordKey::Lock(filesystem_id, *inode_id, *lock_id)
            }
            Self::PublishContent(publication) => {
                RecordKey::Inode(filesystem_id, publication.inode_id)
            }
            Self::PublishXattrStaging(publication) => {
                RecordKey::XattrStaging(filesystem_id, publication.staging_id)
            }
        }
    }

    /// Returns every semantic record key changed by this operation.
    pub fn affected_keys(&self, filesystem_id: FilesystemId) -> Vec<RecordKey> {
        match self {
            Self::PublishXattrStaging(publication) => vec![
                RecordKey::XattrStaging(filesystem_id, publication.staging_id),
                RecordKey::Xattr(
                    filesystem_id,
                    publication.inode_id,
                    publication.name.clone(),
                ),
            ],
            _ => vec![self.primary_key(filesystem_id)],
        }
    }
}

/// Owned declarative request for one serializable all-or-nothing transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitRequest {
    /// Filesystem authority.
    filesystem_id: FilesystemId,
    /// Stable mutation replay context.
    mutation: MutationContext,
    /// Exact writer authority for non-replayed execution.
    fence: WriterFence,
    /// Typed observations protecting the mutation.
    preconditions: Box<[Precondition]>,
    /// Typed complete state changes.
    changes: Box<[StateChange]>,
    /// Exact terminal semantic result retained atomically.
    terminal_result: MutationResult,
}

impl CommitRequest {
    /// Constructs and completely shape-validates an owned commit request.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        filesystem_id: FilesystemId,
        mutation: MutationContext,
        fence: WriterFence,
        preconditions: impl Into<Vec<Precondition>>,
        changes: impl Into<Vec<StateChange>>,
        terminal_result: MutationResult,
        limits: StateLimits,
    ) -> Result<Self, MalformedCommit> {
        let request = Self {
            filesystem_id,
            mutation,
            fence,
            preconditions: preconditions.into().into_boxed_slice(),
            changes: changes.into().into_boxed_slice(),
            terminal_result,
        };
        request.validate_preflight(limits)?;
        Ok(request)
    }

    /// Returns the selected filesystem authority.
    pub const fn filesystem_id(&self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns the complete stable mutation replay context.
    pub const fn mutation(&self) -> MutationContext {
        self.mutation
    }

    /// Returns the exact writer authority presented for execution.
    pub const fn fence(&self) -> WriterFence {
        self.fence
    }

    /// Returns typed observations protecting the operation.
    pub fn preconditions(&self) -> &[Precondition] {
        &self.preconditions
    }

    /// Returns the complete declarative change set.
    pub fn changes(&self) -> &[StateChange] {
        &self.changes
    }

    /// Returns the exact terminal result to retain atomically.
    pub const fn terminal_result(&self) -> &MutationResult {
        &self.terminal_result
    }

    /// Revalidates every shape and resource bound before an adapter acts.
    pub fn validate_preflight(&self, limits: StateLimits) -> Result<(), MalformedCommit> {
        limits
            .require_count(
                StateLimitKind::Preconditions,
                self.preconditions.len(),
                limits.max_preconditions(),
            )
            .map_err(MalformedCommit::Limit)?;
        limits
            .require_count(
                StateLimitKind::Changes,
                self.changes.len(),
                limits.max_changes(),
            )
            .map_err(MalformedCommit::Limit)?;
        if self.changes.is_empty() {
            return Err(MalformedCommit::EmptyChanges);
        }
        limits
            .require_bytes(
                StateLimitKind::MutationResult,
                self.terminal_result.retained_bytes(),
                limits.max_mutation_result_bytes(),
            )
            .map_err(MalformedCommit::Limit)?;

        for precondition in &self.preconditions {
            if let Some(key) = precondition.record_key()
                && key.filesystem_id() != self.filesystem_id
            {
                return Err(MalformedCommit::KeyOutsideFilesystem(key.clone()));
            }
            if let Some(key) = precondition.record_key() {
                key.validate_against_limits(limits)
                    .map_err(MalformedCommit::Limit)?;
            }
        }

        let mut targets = BTreeSet::new();
        let mut lock_count = 0usize;
        let mut open_pin_count = 0usize;
        let mut xattr_count = 0usize;
        for change in &self.changes {
            for key in change.affected_keys(self.filesystem_id) {
                if key.filesystem_id() != self.filesystem_id {
                    return Err(MalformedCommit::KeyOutsideFilesystem(key));
                }
                key.validate_against_limits(limits)
                    .map_err(MalformedCommit::Limit)?;
                if !targets.insert(key.clone()) {
                    return Err(MalformedCommit::DuplicateChangeTarget(key));
                }
                match key.family() {
                    RecordFamily::Lock => lock_count += 1,
                    RecordFamily::OpenPin => open_pin_count += 1,
                    RecordFamily::Xattr | RecordFamily::XattrStaging => xattr_count += 1,
                    _ => {}
                }
            }
            match change {
                StateChange::Insert { key, record } => {
                    record
                        .validate_key(key)
                        .map_err(MalformedCommit::InvalidRecord)?;
                    record
                        .validate_against_limits(limits)
                        .map_err(MalformedCommit::Limit)?;
                    if matches!(
                        key.family(),
                        RecordFamily::Mutation | RecordFamily::WriterLease
                    ) {
                        return Err(MalformedCommit::StoreOwnedRecord(key.clone()));
                    }
                    if let StateRecord::Inode(inode) = record
                        && inode.content().is_some()
                    {
                        return Err(MalformedCommit::ContentReplacementRequiresPreparedPublication);
                    }
                }
                StateChange::Replace { key, record } => {
                    record
                        .validate_key(key)
                        .map_err(MalformedCommit::InvalidRecord)?;
                    record
                        .validate_against_limits(limits)
                        .map_err(MalformedCommit::Limit)?;
                    if matches!(
                        key.family(),
                        RecordFamily::Mutation | RecordFamily::WriterLease
                    ) {
                        return Err(MalformedCommit::StoreOwnedRecord(key.clone()));
                    }
                }
                StateChange::Delete(key)
                    if matches!(
                        key.family(),
                        RecordFamily::Mutation | RecordFamily::WriterLease
                    ) =>
                {
                    return Err(MalformedCommit::StoreOwnedRecord(key.clone()));
                }
                StateChange::PublishContent(publication) => {
                    validate_publish_shape(publication, &self.mutation)
                        .map_err(MalformedCommit::InvalidPublication)?;
                }
                StateChange::PublishXattrStaging(publication) => {
                    limits
                        .require_bytes(
                            StateLimitKind::XattrName,
                            publication.name.as_bytes().len(),
                            limits.max_xattr_name_bytes(),
                        )
                        .map_err(MalformedCommit::Limit)?;
                }
                _ => {}
            }
        }
        limits
            .require_count(
                StateLimitKind::ChangeKeys,
                targets
                    .len()
                    .checked_add(1)
                    .ok_or(MalformedCommit::Arithmetic)?,
                limits.max_change_keys(),
            )
            .map_err(MalformedCommit::Limit)?;
        limits
            .require_count(
                StateLimitKind::Locks,
                lock_count,
                limits.max_locks_per_request(),
            )
            .map_err(MalformedCommit::Limit)?;
        limits
            .require_count(
                StateLimitKind::OpenPins,
                open_pin_count,
                limits.max_open_pins_per_request(),
            )
            .map_err(MalformedCommit::Limit)?;
        limits
            .require_count(
                StateLimitKind::Xattrs,
                xattr_count,
                limits.max_xattrs_per_request(),
            )
            .map_err(MalformedCommit::Limit)?;

        let retained_bytes = self.retained_bytes().ok_or(MalformedCommit::Arithmetic)?;
        limits
            .require_bytes(
                StateLimitKind::TransactionBytes,
                retained_bytes,
                limits.max_transaction_bytes(),
            )
            .map_err(MalformedCommit::Limit)
    }

    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        let mut bytes = 256usize.checked_add(self.terminal_result.retained_bytes())?;
        for precondition in &self.preconditions {
            bytes = bytes.checked_add(estimate_precondition(precondition)?)?;
        }
        for change in &self.changes {
            bytes = bytes.checked_add(estimate_change(change)?)?;
        }
        Some(bytes)
    }
}

fn validate_publish_shape(
    publication: &PublishContent,
    mutation: &MutationContext,
) -> Result<(), PublishContentError> {
    let identity = publication.prepared.identity();
    if identity.mutation_id() != mutation.mutation_id {
        return Err(PublishContentError::MutationIdMismatch);
    }
    if identity.base() != publication.expected_base {
        return Err(PublishContentError::PreparationBaseMismatch);
    }
    if publication.logical_size != publication.prepared.content().logical_size() {
        return Err(PublishContentError::LogicalSizeMismatch);
    }
    if publication.data_generation.get() != publication.prepared.content().generation() {
        return Err(PublishContentError::DataGenerationMismatch);
    }
    Ok(())
}

fn estimate_precondition(precondition: &Precondition) -> Option<usize> {
    match precondition {
        Precondition::RecordAbsent(key) | Precondition::RecordRevision { key, .. } => {
            64usize.checked_add(key.retained_bytes()?)
        }
        _ => Some(128),
    }
}

fn estimate_change(change: &StateChange) -> Option<usize> {
    match change {
        StateChange::Insert { key, record } | StateChange::Replace { key, record } => 64usize
            .checked_add(key.retained_bytes()?)?
            .checked_add(record.retained_bytes()?),
        StateChange::Delete(key) => 64usize.checked_add(key.retained_bytes()?),
        StateChange::PublishContent(publication) => {
            512usize.checked_add(publication.prepared.content().manifest_key().as_str().len())
        }
        StateChange::PublishXattrStaging(publication) => {
            192usize.checked_add(publication.name.as_bytes().len())
        }
        _ => Some(128),
    }
}

/// Successfully retained mutation result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedMutation {
    /// All-record commit revision.
    pub revision: StateRevision,
    /// Exact terminal result.
    pub result: MutationResult,
}

impl CommittedMutation {
    /// Reconstructs the exact replay value from a durable ledger record.
    pub fn from_record(record: &MutationRecord) -> Self {
        Self {
            revision: record.committed_revision(),
            result: record.result().clone(),
        }
    }
}

/// Hard mismatch when a stable mutation identity is reused for other semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationMismatch {
    /// Defensive mismatch between the requested and selected ledger key.
    MutationId,
    /// Complete semantic request fingerprint differs.
    Fingerprint,
    /// Client/session lineage differs.
    ClientIncarnation,
    /// Retention horizon differs from the recorded replay identity.
    Retention,
}

/// Result of the normative ledger-first mutation lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationReplay {
    /// No retained ledger record exists and normal validation may proceed.
    Absent,
    /// Exact retained result must be returned without reapplying the mutation.
    Exact(CommittedMutation),
    /// Stable mutation identity was reused with different semantics.
    Mismatch(MutationMismatch),
}

/// Exact reason a typed precondition did not match authoritative state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommitConflictKind {
    /// A record expected absent was present.
    RecordPresent(RecordKey),
    /// A record expected present was absent.
    RecordMissing(RecordKey),
    /// A record revision differed.
    RecordRevision {
        /// Semantic record key.
        key: RecordKey,
        /// Expected revision.
        expected: RecordRevision,
        /// Actual revision.
        actual: RecordRevision,
    },
    /// Inode metadata generation differed.
    InodeGeneration,
    /// Regular-file data generation differed.
    DataGeneration,
    /// Directory namespace generation differed.
    DirectoryGeneration,
    /// Immutable content base differed.
    ContentBase,
    /// Hard-link count differed.
    LinkCount,
    /// Durable open-pin count differed.
    OpenPinCount,
    /// Explicit fence precondition differed from the request fence.
    ExactFence,
    /// A requested byte-range lock conflicts with an existing lock.
    LockConflict {
        /// Deterministically selected conflicting lock.
        existing: LockId,
    },
}

/// Positionally identified precondition conflict requiring semantic revalidation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitConflict {
    /// Zero-based precondition index.
    pub precondition_index: usize,
    /// Exact failed observation.
    pub kind: CommitConflictKind,
}

/// Structurally invalid commit request rejected before any state is changed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MalformedCommit {
    /// A configured request or aggregate bound was exceeded.
    Limit(StateLimitError),
    /// A tagged record failed structural or key validation.
    InvalidRecord(RecordValidationError),
    /// A prepared-content handoff failed validation.
    InvalidPublication(PublishContentError),
    /// A semantic record key belongs to another filesystem.
    KeyOutsideFilesystem(RecordKey),
    /// Multiple changes target the same semantic record ambiguously.
    DuplicateChangeTarget(RecordKey),
    /// Checked persistent arithmetic overflowed or underflowed.
    Arithmetic,
    /// Generic inode replacement attempted to bypass prepared-content publication.
    ContentReplacementRequiresPreparedPublication,
    /// Caller attempted to directly alter a store-owned ledger or lease record.
    StoreOwnedRecord(RecordKey),
    /// No state transition was supplied.
    EmptyChanges,
    /// Directory cookie insertion was not tied to the pre-state allocation high-water mark.
    DirectoryCookieAllocation,
    /// A persistent generation or allocation counter did not move as required.
    NonMonotonicTransition,
    /// A namespace mutation omitted the parent directory generation transition.
    NamespaceGeneration,
    /// Generic xattr/staging changes attempted to bypass checked staging publication.
    XattrPublicationRequired,
    /// Xattr staging publication referenced missing, mismatched, or incomplete staging.
    InvalidXattrStaging,
}

impl fmt::Display for MalformedCommit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit(error) => error.fmt(formatter),
            Self::InvalidRecord(error) => error.fmt(formatter),
            Self::InvalidPublication(error) => error.fmt(formatter),
            Self::KeyOutsideFilesystem(key) => {
                write!(
                    formatter,
                    "record key belongs to another filesystem: {key:?}"
                )
            }
            Self::DuplicateChangeTarget(key) => {
                write!(formatter, "duplicate change target: {key:?}")
            }
            Self::Arithmetic => formatter.write_str("checked commit arithmetic failed"),
            Self::ContentReplacementRequiresPreparedPublication => formatter.write_str(
                "regular-file content replacement requires prepared-content publication",
            ),
            Self::StoreOwnedRecord(key) => {
                write!(
                    formatter,
                    "caller cannot directly change store-owned record: {key:?}"
                )
            }
            Self::EmptyChanges => formatter.write_str("commit change set is empty"),
            Self::DirectoryCookieAllocation => formatter.write_str(
                "directory cookie insertion is not coupled to its allocation high-water mark",
            ),
            Self::NonMonotonicTransition => {
                formatter.write_str("persistent generation or counter transition is not monotonic")
            }
            Self::NamespaceGeneration => formatter
                .write_str("namespace mutation did not advance its parent directory generation"),
            Self::XattrPublicationRequired => formatter.write_str(
                "xattr staging retirement and publication require the dedicated transition",
            ),
            Self::InvalidXattrStaging => formatter
                .write_str("xattr staging publication is missing, mismatched, or incomplete"),
        }
    }
}

impl std::error::Error for MalformedCommit {}

/// Information required to resolve unknown commit status safely.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmbiguousCommit {
    /// Exact mutation identity that must be retried unchanged.
    pub mutation: MutationContext,
}

/// Semantic commit result, separate from adapter/infrastructure failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommitOutcome {
    /// Mutation was applied exactly once.
    Committed(CommittedMutation),
    /// Exact retained mutation was replayed without revalidation or reapplication.
    AlreadyCommitted(CommittedMutation),
    /// One typed observation conflicted and the complete operation must be revalidated.
    Conflict(CommitConflict),
    /// Stable mutation identity was reused with different request semantics.
    MutationMismatch(MutationMismatch),
    /// Presented lease/scope/holder/token no longer names the current writer.
    StaleFence,
    /// Presented exact lease is current but expired at adapter-authoritative time.
    ExpiredLease,
    /// Complete request preflight rejected malformed input without mutation.
    MalformedRequest(MalformedCommit),
    /// Adapter may have committed; only an exact mutation retry may resolve status.
    Ambiguous(AmbiguousCommit),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FencingToken, GroupId, InodeData, MutationResultKind, PrincipalId, RecordRevision,
        ResultFormatVersion, StateLimits, UnixTimestamp, WriterIncarnationId, WriterScopeId,
    };

    fn result(bytes: &[u8]) -> MutationResult {
        MutationResult::new(
            MutationResultKind::new(1).unwrap(),
            ResultFormatVersion::new(1).unwrap(),
            bytes.to_vec(),
            StateLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn exact_replay_includes_fingerprint_and_client_incarnation() {
        let filesystem_id = FilesystemId::from_u128(1);
        let mutation_id = w9pt_storage::MutationId::from_u128(2);
        let fingerprint = RequestFingerprint::blake3(b"complete request");
        let client = ClientIncarnationId::from_u128(3);
        let context =
            MutationContext::new(mutation_id, fingerprint, client, MutationRetention::new(10));
        let record = MutationRecord::new(
            filesystem_id,
            mutation_id,
            fingerprint,
            client,
            WriterScopeId::from_u128(4),
            WriterIncarnationId::from_u128(5),
            FencingToken::new(6).unwrap(),
            result(b"terminal"),
            StateRevision::new(7).unwrap(),
            MutationRetention::new(10),
            RecordRevision::new(7).unwrap(),
        );
        assert!(matches!(
            context.classify_record(&record),
            MutationReplay::Exact(CommittedMutation { revision, .. }) if revision.get() == 7
        ));
        let changed = MutationContext::new(
            mutation_id,
            RequestFingerprint::blake3(b"changed"),
            client,
            MutationRetention::new(10),
        );
        assert_eq!(
            changed.classify_record(&record),
            MutationReplay::Mismatch(MutationMismatch::Fingerprint)
        );
        let changed_retention =
            MutationContext::new(mutation_id, fingerprint, client, MutationRetention::new(11));
        assert_eq!(
            changed_retention.classify_record(&record),
            MutationReplay::Mismatch(MutationMismatch::Retention)
        );
    }

    #[test]
    fn counter_adjustments_never_wrap_or_underflow() {
        assert_eq!(CounterAdjustment::increase(0), None);
        assert_eq!(CounterAdjustment::increase(2).unwrap().apply(3), Some(5));
        assert_eq!(CounterAdjustment::decrease(4).unwrap().apply(3), None);
        assert_eq!(
            CounterAdjustment::increase(1).unwrap().apply(u64::MAX),
            None
        );
    }

    #[test]
    fn prepared_content_is_bound_to_mutation_base_file_size_and_generations() {
        let limits = StateLimits::default();
        let file_id = w9pt_storage::FileId::from_u128(1);
        let mutation_id = w9pt_storage::MutationId::from_u128(2);
        let repository = w9pt_storage::ContentRepository::new(
            w9pt_storage::testing::MemoryTarget::new(),
            "test",
            w9pt_storage::CreationDefaults::new(w9pt_storage::StorageMethod::Raw),
            w9pt_storage::StorageLimits::default(),
        )
        .unwrap();
        let prepared = w9pt_storage::testing::block_on(repository.prepare_create(
            file_id,
            mutation_id,
            0,
            b"abc",
        ))
        .unwrap();
        let timestamp = UnixTimestamp::new(1, 0).unwrap();
        let times = InodeTimes {
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
            created: timestamp,
        };
        let inode = InodeRecord::new(
            InodeId::from_u128(3),
            RecordRevision::new(1).unwrap(),
            0o644,
            PrincipalId::new(b"owner".to_vec(), limits).unwrap(),
            GroupId::new(b"group".to_vec(), limits).unwrap(),
            times,
            0,
            1,
            InodeGeneration::new(1).unwrap(),
            InodeData::RegularFile {
                content_file_id: file_id,
                content: None,
                data_generation: 0,
            },
        )
        .unwrap();
        let publication = PublishContent {
            inode_id: inode.inode_id(),
            expected_base: w9pt_storage::BaseContentIdentity::NEW_FILE,
            logical_size: 3,
            data_generation: DataGeneration::new(1).unwrap(),
            prepared,
            inode_generation: InodeGeneration::new(2).unwrap(),
            times,
        };
        let context = MutationContext::new(
            mutation_id,
            RequestFingerprint::blake3(b"create"),
            ClientIncarnationId::from_u128(4),
            MutationRetention::new(10),
        );
        assert_eq!(
            validate_publish_content(&publication, &context, &inode),
            Ok(())
        );
        let wrong_mutation = MutationContext::new(
            w9pt_storage::MutationId::from_u128(9),
            context.fingerprint,
            context.client_incarnation,
            context.retention,
        );
        assert_eq!(
            validate_publish_content(&publication, &wrong_mutation, &inode),
            Err(PublishContentError::MutationIdMismatch)
        );
    }

    #[test]
    fn semantic_commit_outcomes_keep_ambiguity_and_rejections_distinct() {
        let mutation = MutationContext::new(
            w9pt_storage::MutationId::from_u128(1),
            RequestFingerprint::blake3(b"request"),
            ClientIncarnationId::from_u128(2),
            MutationRetention::new(3),
        );
        assert!(matches!(
            CommitOutcome::Ambiguous(AmbiguousCommit { mutation }),
            CommitOutcome::Ambiguous(_)
        ));
        assert_ne!(CommitOutcome::StaleFence, CommitOutcome::ExpiredLease);
    }

    #[test]
    fn complete_shape_preflight_rejects_empty_and_late_invalid_changes() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let mutation = MutationContext::new(
            w9pt_storage::MutationId::from_u128(2),
            RequestFingerprint::blake3(b"delete"),
            ClientIncarnationId::from_u128(3),
            MutationRetention::new(4),
        );
        let fence = WriterFence {
            scope: WriterScopeId::from_u128(5),
            holder: WriterIncarnationId::from_u128(6),
            lease_id: crate::LeaseId::from_u128(7),
            fencing_token: FencingToken::new(8).unwrap(),
        };
        assert_eq!(
            CommitRequest::new(
                filesystem_id,
                mutation,
                fence,
                vec![],
                vec![],
                result(b"done"),
                limits,
            ),
            Err(MalformedCommit::EmptyChanges)
        );
        let changes = vec![
            StateChange::Delete(RecordKey::Inode(filesystem_id, InodeId::from_u128(9))),
            StateChange::Delete(RecordKey::Inode(
                FilesystemId::from_u128(10),
                InodeId::from_u128(11),
            )),
        ];
        assert!(matches!(
            CommitRequest::new(
                filesystem_id,
                mutation,
                fence,
                vec![],
                changes,
                result(b"done"),
                limits,
            ),
            Err(MalformedCommit::KeyOutsideFilesystem(_))
        ));

        let strict_limits = StateLimits::new(crate::StateLimitValues {
            max_entry_name_bytes: 1,
            ..crate::StateLimitValues::default()
        })
        .unwrap();
        let long_name = crate::EntryName::new(b"long".to_vec(), limits).unwrap();
        assert!(matches!(
            CommitRequest::new(
                filesystem_id,
                mutation,
                fence,
                vec![],
                vec![StateChange::Delete(RecordKey::DirectoryEntry(
                    filesystem_id,
                    InodeId::from_u128(12),
                    long_name,
                ))],
                result(b"done"),
                strict_limits,
            ),
            Err(MalformedCommit::Limit(crate::StateLimitError {
                kind: StateLimitKind::EntryName,
                ..
            }))
        ));
    }

    #[test]
    fn ledger_lookup_precedes_fencing_and_atomic_publication_is_one_phase() {
        assert_eq!(COMMIT_PROTOCOL_ORDER[0], CommitProtocolPhase::LedgerLookup);
        assert!(
            COMMIT_PROTOCOL_ORDER
                .iter()
                .position(|phase| *phase == CommitProtocolPhase::LedgerLookup)
                < COMMIT_PROTOCOL_ORDER
                    .iter()
                    .position(|phase| *phase == CommitProtocolPhase::FenceValidation)
        );
        assert_eq!(
            COMMIT_PROTOCOL_ORDER
                .iter()
                .filter(|phase| { **phase == CommitProtocolPhase::PublishRecordsResultAndChange })
                .count(),
            1
        );
    }
}
