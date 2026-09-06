//! Shared deterministic in-memory filesystem-state authority.

use core::{fmt, future::Future};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
};

use crate::{
    AcquireLeaseOutcome, AcquireWriterLease, AdapterFailureKind, AmbiguousCommit, ChangeBatch,
    ChangeEvent, ChangeOrigin, ChangePoll, ChangePollOutcome, CommitConflict, CommitConflictKind,
    CommitOutcome, CommitRequest, CommittedMutation, DirectoryCookie, DirectoryPage,
    DirectoryPageEntry, FenceValidation, FencingToken, FilesystemStateStore, LeaseOperationId,
    LeaseRejection, LeaseTimeAuthority, LockCursor, MalformedCommit, ManualLeaseClock,
    MutationMismatch, MutationRecord, OpenPinCursor, Precondition, PublishContentError, ReadBatch,
    ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision, RecordScan,
    ReleaseLeaseOutcome, ReleaseWriterLease, RenewLeaseOutcome, RenewWriterLease,
    RequestFingerprint, ScanPage, ScanResume, StateChange, StateLimits, StateRecord, StateRevision,
    StateSnapshot, StateStoreAdapterError, StateStoreContract, StateStoreOperation,
    WriterLeaseGrant, WriterScopeId, WriterTopology, XattrCursor, grant_writer_lease,
    renew_current_lease, validate_lease_release, validate_publish_content_with_metadata,
    validate_record_set_with_limits, validate_writer_fence,
};

#[derive(Debug)]
struct AuthorityState {
    revision: StateRevision,
    records: BTreeMap<RecordKey, StateRecord>,
    changes: VecDeque<ChangeEvent>,
    oldest_change_cursor: StateRevision,
    last_fencing_tokens: BTreeMap<(crate::FilesystemId, WriterScopeId), FencingToken>,
    lease_operations: BTreeMap<(crate::FilesystemId, LeaseOperationId), LeaseOperationResult>,
    commit_failures: VecDeque<CommitFailureTiming>,
    trace: Vec<MemoryTraceEvent>,
}

/// Deterministic one-shot failure timing for an atomic commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitFailureTiming {
    /// Fail definitively after staging but before authoritative publication.
    BeforePublication,
    /// Publish atomically, then report an ambiguous semantic outcome.
    AfterPublication,
}

/// Ordered phase captured by the deterministic authority trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryTracePhase {
    /// Commit call acquired the authority lock.
    Started,
    /// Operation completed with a stable semantic outcome.
    Completed,
    /// Operation was rejected before authoritative publication.
    Rejected,
    /// Injected failure occurred before publication; no commit is visible.
    FailedBeforePublication,
    /// Records, ledger, revision, and event were atomically published.
    Published,
    /// Injected failure occurred after publication; status must be resolved by replay.
    AmbiguousAfterPublication,
}

/// One ordered deterministic reference-authority trace event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryTraceEvent {
    /// Strictly increasing event sequence within the authority.
    pub sequence: u64,
    /// Store operation being traced.
    pub operation: StateStoreOperation,
    /// Observable phase of that operation.
    pub phase: MemoryTracePhase,
}

#[derive(Clone, Copy, Debug)]
enum LeaseOperationResult {
    Acquire {
        fingerprint: RequestFingerprint,
        outcome: AcquireLeaseOutcome,
    },
    Renew {
        fingerprint: RequestFingerprint,
        outcome: RenewLeaseOutcome,
    },
    Release {
        fingerprint: RequestFingerprint,
        outcome: ReleaseLeaseOutcome,
    },
}

/// Owner/factory of one deterministic shared state authority.
#[derive(Clone, Debug)]
pub struct MemoryAuthority {
    state: Arc<Mutex<AuthorityState>>,
    clock: ManualLeaseClock,
    contract: StateStoreContract,
}

impl MemoryAuthority {
    /// Creates an empty authority at revision one with an explicit clock and topology.
    pub fn new(topology: WriterTopology, limits: StateLimits, clock: ManualLeaseClock) -> Self {
        Self {
            state: Arc::new(Mutex::new(AuthorityState {
                revision: StateRevision::new(1).expect("one is a valid state revision"),
                records: BTreeMap::new(),
                changes: VecDeque::new(),
                oldest_change_cursor: StateRevision::new(1).expect("one is a valid state revision"),
                last_fencing_tokens: BTreeMap::new(),
                lease_operations: BTreeMap::new(),
                commit_failures: VecDeque::new(),
                trace: Vec::new(),
            })),
            clock,
            contract: StateStoreContract::deterministic_reference(topology, limits),
        }
    }

    /// Opens a new cache-free client handle over the same authoritative backing state.
    pub fn open_client(&self) -> MemoryStateStore {
        MemoryStateStore {
            state: Arc::clone(&self.state),
            clock: self.clock.clone(),
            contract: self.contract,
        }
    }

    /// Returns the explicit clock shared by all clients.
    pub fn clock(&self) -> ManualLeaseClock {
        self.clock.clone()
    }

    /// Injects one deterministic failure into the next non-replayed commit.
    pub fn inject_commit_failure(
        &self,
        timing: CommitFailureTiming,
    ) -> Result<(), MemoryStateStoreError> {
        self.state
            .lock()
            .map_err(|_| {
                MemoryStateStoreError::new(
                    StateStoreOperation::Commit,
                    AdapterFailureKind::Internal,
                )
            })?
            .commit_failures
            .push_back(timing);
        Ok(())
    }

    /// Returns all deterministic trace events in authority order.
    pub fn trace(&self) -> Result<Vec<MemoryTraceEvent>, MemoryStateStoreError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| {
                MemoryStateStoreError::new(StateStoreOperation::Read, AdapterFailureKind::Internal)
            })?
            .trace
            .clone())
    }
}

/// Independently opened client containing no record, lease, or revision cache.
#[derive(Clone, Debug)]
pub struct MemoryStateStore {
    state: Arc<Mutex<AuthorityState>>,
    clock: ManualLeaseClock,
    contract: StateStoreContract,
}

impl MemoryStateStore {
    /// Returns the validated reference contract.
    pub const fn contract_value(&self) -> StateStoreContract {
        self.contract
    }

    /// Reads the shared authoritative revision for deterministic diagnostics.
    pub fn current_revision(&self) -> Result<StateRevision, MemoryStateStoreError> {
        Ok(self.lock(StateStoreOperation::Read)?.revision)
    }

    /// Reports whether two independently opened handles use one authority.
    pub fn shares_authority_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    /// Executes a one-lock, one-revision consistent read request.
    pub async fn read_request(
        &self,
        request: ReadBatch,
    ) -> Result<ReadOutcome, MemoryStateStoreError> {
        let mut state = self.lock(StateStoreOperation::Read)?;
        record_trace(
            &mut state,
            StateStoreOperation::Read,
            MemoryTracePhase::Started,
        )?;
        if let Err(error) = request.validate(self.contract.limits()) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::Read,
                MemoryTracePhase::Rejected,
                ReadOutcome::MalformedRequest(error),
            );
        }
        if let ReadConsistency::AtLeast(required) = request.consistency()
            && state.revision < required
        {
            let current = state.revision;
            return traced_outcome(
                &mut state,
                StateStoreOperation::Read,
                MemoryTracePhase::Completed,
                ReadOutcome::RevisionUnavailable { required, current },
            );
        }
        let mut results = Vec::with_capacity(request.queries().len());
        for (query_index, query) in request.queries().iter().enumerate() {
            if let ReadQuery::OpenPinCount(inode_id) = query {
                let count = state
                    .records
                    .keys()
                    .filter(|key| {
                        matches!(
                            key,
                            RecordKey::OpenPin(filesystem_id, candidate, _)
                                if *filesystem_id == request.filesystem_id()
                                    && *candidate == *inode_id
                        )
                    })
                    .count();
                let count = u64::try_from(count).map_err(|_| {
                    MemoryStateStoreError::new(
                        StateStoreOperation::Read,
                        AdapterFailureKind::Corruption,
                    )
                })?;
                results.push(ReadResult::OpenPinCount {
                    inode_id: *inode_id,
                    count,
                });
                continue;
            }
            if let ReadQuery::DirectoryPage {
                parent_inode_id,
                after,
                bounds,
            } = query
            {
                match read_directory_page(
                    &state,
                    request.filesystem_id(),
                    *parent_inode_id,
                    *after,
                    *bounds,
                )? {
                    Ok(page) => results.push(ReadResult::DirectoryPage(page)),
                    Err(required_bytes) => {
                        return traced_outcome(
                            &mut state,
                            StateStoreOperation::Read,
                            MemoryTracePhase::Completed,
                            ReadOutcome::ScanBoundTooSmall {
                                query_index,
                                required_bytes,
                            },
                        );
                    }
                }
                continue;
            }
            if let ReadQuery::InodeByQidPath(qid_path) = query {
                let inode = state
                    .records
                    .iter()
                    .find_map(|(key, record)| match (key, record) {
                        (RecordKey::Inode(filesystem_id, _), StateRecord::Inode(inode))
                            if *filesystem_id == request.filesystem_id()
                                && inode.qid_path() == *qid_path =>
                        {
                            Some(Box::new(inode.clone()))
                        }
                        _ => None,
                    });
                results.push(ReadResult::InodeByQidPath {
                    qid_path: *qid_path,
                    inode,
                });
                continue;
            }
            if let ReadQuery::InodeWithContentMetadata(inode_id) = query {
                let inode = match state
                    .records
                    .get(&RecordKey::Inode(request.filesystem_id(), *inode_id))
                {
                    Some(StateRecord::Inode(inode)) => Some(Box::new(inode.clone())),
                    _ => None,
                };
                let metadata = inode
                    .as_ref()
                    .and_then(|inode| inode.content_file_id())
                    .and_then(|file_id| {
                        match state.records.get(&RecordKey::ContentMetadata(
                            request.filesystem_id(),
                            file_id,
                        )) {
                            Some(StateRecord::ContentMetadata(metadata)) => {
                                Some(Box::new(metadata.clone()))
                            }
                            _ => None,
                        }
                    });
                results.push(ReadResult::InodeWithContentMetadata {
                    inode_id: *inode_id,
                    inode,
                    metadata,
                });
                continue;
            }
            if let Some(key) = query.point_key(request.filesystem_id()) {
                results.push(ReadResult::Point {
                    record: state.records.get(&key).cloned().map(Box::new),
                    key,
                });
                continue;
            }
            let ReadQuery::Scan(scan) = query else {
                return Err(MemoryStateStoreError::new(
                    StateStoreOperation::Read,
                    AdapterFailureKind::Internal,
                ));
            };
            match read_scan(&state, request.filesystem_id(), scan)? {
                Ok(page) => results.push(ReadResult::Scan(page)),
                Err(required_bytes) => {
                    return traced_outcome(
                        &mut state,
                        StateStoreOperation::Read,
                        MemoryTracePhase::Completed,
                        ReadOutcome::ScanBoundTooSmall {
                            query_index,
                            required_bytes,
                        },
                    );
                }
            }
        }
        let snapshot = StateSnapshot::new(state.revision, &request, results).map_err(|_| {
            MemoryStateStoreError::new(StateStoreOperation::Read, AdapterFailureKind::Corruption)
        })?;
        traced_outcome(
            &mut state,
            StateStoreOperation::Read,
            MemoryTracePhase::Completed,
            ReadOutcome::Snapshot(snapshot),
        )
    }

    /// Applies one staged all-or-nothing transition under the authority lock.
    pub async fn commit_request(
        &self,
        request: CommitRequest,
    ) -> Result<CommitOutcome, MemoryStateStoreError> {
        let mut state = self.lock(StateStoreOperation::Commit)?;
        record_trace(
            &mut state,
            StateStoreOperation::Commit,
            MemoryTracePhase::Started,
        )?;

        // Normative ledger-first lookup: exact replay succeeds even after lease expiry.
        let mutation_key =
            RecordKey::Mutation(request.filesystem_id(), request.mutation().mutation_id);
        if let Some(StateRecord::Mutation(record)) = state.records.get(&mutation_key) {
            let outcome = match request.mutation().classify_record(record) {
                crate::MutationReplay::Exact(committed) => {
                    CommitOutcome::AlreadyCommitted(committed)
                }
                crate::MutationReplay::Mismatch(mismatch) => {
                    CommitOutcome::MutationMismatch(mismatch)
                }
                crate::MutationReplay::Absent => {
                    CommitOutcome::MutationMismatch(MutationMismatch::MutationId)
                }
            };
            return traced_outcome(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::Completed,
                outcome,
            );
        }

        if let Err(error) = request.validate_preflight(self.contract.limits()) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::Rejected,
                CommitOutcome::MalformedRequest(error),
            );
        }
        if matches!(
            self.contract.writer_topology(),
            WriterTopology::SingleFencedWriter
        ) && request.fence().scope != crate::WriterScopeId::FILESYSTEM
        {
            return traced_outcome(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::Rejected,
                CommitOutcome::StaleFence,
            );
        }
        let lease_key = RecordKey::WriterLease(request.filesystem_id(), request.fence().scope);
        let current_lease = match state.records.get(&lease_key) {
            Some(StateRecord::WriterLease(record)) => Some(record),
            _ => None,
        };
        let now = self.clock.now().map_err(|_| {
            MemoryStateStoreError::new(StateStoreOperation::Commit, AdapterFailureKind::Internal)
        })?;
        match validate_writer_fence(request.filesystem_id(), request.fence(), current_lease, now) {
            FenceValidation::Current => {}
            FenceValidation::Stale => {
                return traced_outcome(
                    &mut state,
                    StateStoreOperation::Commit,
                    MemoryTracePhase::Rejected,
                    CommitOutcome::StaleFence,
                );
            }
            FenceValidation::Expired => {
                return traced_outcome(
                    &mut state,
                    StateStoreOperation::Commit,
                    MemoryTracePhase::Rejected,
                    CommitOutcome::ExpiredLease,
                );
            }
        }
        if let Some(conflict) = evaluate_preconditions(&state.records, &request) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::Rejected,
                CommitOutcome::Conflict(conflict),
            );
        }
        if let Err(error) = validate_directory_cookie_transition(&state.records, &request) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::Rejected,
                CommitOutcome::MalformedRequest(error),
            );
        }
        if let Err(error) = validate_qid_path_transition(&state.records, &request) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::Rejected,
                CommitOutcome::MalformedRequest(error),
            );
        }
        if let Err(error) = validate_monotonic_transitions(&state.records, &request) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::Rejected,
                CommitOutcome::MalformedRequest(error),
            );
        }
        if let Err(error) =
            validate_xattr_transitions(&state.records, &request, self.contract.limits())
        {
            return traced_outcome(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::Rejected,
                CommitOutcome::MalformedRequest(error),
            );
        }

        let next_revision = state.revision.checked_next().map_err(|_| {
            MemoryStateStoreError::new(StateStoreOperation::Commit, AdapterFailureKind::Corruption)
        })?;
        let record_revision = RecordRevision::new(next_revision.get()).map_err(|_| {
            MemoryStateStoreError::new(StateStoreOperation::Commit, AdapterFailureKind::Corruption)
        })?;
        let mut staged = state.records.clone();
        let mut changed_keys = Vec::with_capacity(request.changes().len());
        for change in request.changes() {
            let affected = change.affected_keys(request.filesystem_id());
            match apply_change(
                &mut staged,
                request.filesystem_id(),
                request.mutation(),
                change,
                record_revision,
                self.contract.limits(),
            ) {
                Ok(()) => {
                    for key in affected {
                        if !changed_keys.contains(&key) {
                            changed_keys.push(key);
                        }
                    }
                }
                Err(ApplyFailure::Conflict(kind)) => {
                    return traced_outcome(
                        &mut state,
                        StateStoreOperation::Commit,
                        MemoryTracePhase::Rejected,
                        CommitOutcome::Conflict(CommitConflict {
                            precondition_index: request.preconditions().len(),
                            kind,
                        }),
                    );
                }
                Err(ApplyFailure::Malformed(error)) => {
                    return traced_outcome(
                        &mut state,
                        StateStoreOperation::Commit,
                        MemoryTracePhase::Rejected,
                        CommitOutcome::MalformedRequest(error),
                    );
                }
            }
        }
        if let Err(error) = validate_record_set_with_limits(&staged, self.contract.limits()) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::Rejected,
                CommitOutcome::MalformedRequest(MalformedCommit::InvalidRecord(error)),
            );
        }

        let mutation = request.mutation();
        let retained = MutationRecord::new(
            request.filesystem_id(),
            mutation.mutation_id,
            mutation.fingerprint,
            mutation.client_incarnation,
            request.fence().scope,
            request.fence().holder,
            request.fence().fencing_token,
            request.terminal_result().clone(),
            next_revision,
            mutation.retention,
            record_revision,
        );
        staged.insert(mutation_key.clone(), StateRecord::Mutation(retained));
        changed_keys.push(mutation_key);
        let event = ChangeEvent::new(
            request.filesystem_id(),
            next_revision,
            ChangeOrigin::Mutation(mutation.mutation_id),
            changed_keys,
            self.contract.limits(),
        )
        .map_err(|_| {
            MemoryStateStoreError::new(StateStoreOperation::Commit, AdapterFailureKind::Corruption)
        })?;

        if matches!(
            state.commit_failures.front(),
            Some(CommitFailureTiming::BeforePublication)
        ) {
            state.commit_failures.pop_front();
            record_trace(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::FailedBeforePublication,
            )?;
            return Err(MemoryStateStoreError::new(
                StateStoreOperation::Commit,
                AdapterFailureKind::Unavailable,
            ));
        }

        // One swap publishes records, ledger, revision, and whole-commit event together.
        state.records = staged;
        state.revision = next_revision;
        state.changes.push_back(event);
        while state.changes.len()
            > usize::try_from(self.contract.limits().max_change_history_commits())
                .unwrap_or(usize::MAX)
        {
            if let Some(compacted) = state.changes.pop_front() {
                state.oldest_change_cursor = compacted.revision();
            }
        }
        record_trace(
            &mut state,
            StateStoreOperation::Commit,
            MemoryTracePhase::Published,
        )?;
        if matches!(
            state.commit_failures.front(),
            Some(CommitFailureTiming::AfterPublication)
        ) {
            state.commit_failures.pop_front();
            record_trace(
                &mut state,
                StateStoreOperation::Commit,
                MemoryTracePhase::AmbiguousAfterPublication,
            )?;
            return Ok(CommitOutcome::Ambiguous(AmbiguousCommit {
                mutation: request.mutation(),
            }));
        }
        Ok(CommitOutcome::Committed(CommittedMutation {
            revision: next_revision,
            result: request.terminal_result().clone(),
        }))
    }

    /// Idempotently grants an absent or expired writer lease.
    pub async fn acquire_lease_request(
        &self,
        request: AcquireWriterLease,
    ) -> Result<AcquireLeaseOutcome, MemoryStateStoreError> {
        let fingerprint = request.operation_fingerprint();
        let operation_key = (request.filesystem_id(), request.operation_id());
        let mut state = self.lock(StateStoreOperation::AcquireLease)?;
        record_trace(
            &mut state,
            StateStoreOperation::AcquireLease,
            MemoryTracePhase::Started,
        )?;
        if let Some(result) = state.lease_operations.get(&operation_key) {
            let outcome = match result {
                LeaseOperationResult::Acquire {
                    fingerprint: retained,
                    outcome,
                } if *retained == fingerprint => match outcome {
                    AcquireLeaseOutcome::Granted(grant)
                    | AcquireLeaseOutcome::AlreadyApplied(grant) => {
                        AcquireLeaseOutcome::AlreadyApplied(*grant)
                    }
                    AcquireLeaseOutcome::Rejected(rejection) => {
                        AcquireLeaseOutcome::Rejected(*rejection)
                    }
                },
                _ => AcquireLeaseOutcome::Rejected(LeaseRejection::OperationMismatch),
            };
            return traced_outcome(
                &mut state,
                StateStoreOperation::AcquireLease,
                MemoryTracePhase::Completed,
                outcome,
            );
        }
        if request.validate(self.contract.limits()).is_err() {
            return traced_outcome(
                &mut state,
                StateStoreOperation::AcquireLease,
                MemoryTracePhase::Rejected,
                AcquireLeaseOutcome::Rejected(LeaseRejection::InvalidDuration),
            );
        }
        if lease_history_full(&state, self.contract.limits()) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::AcquireLease,
                MemoryTracePhase::Rejected,
                AcquireLeaseOutcome::Rejected(LeaseRejection::OperationHistoryFull),
            );
        }
        let key = RecordKey::WriterLease(request.filesystem_id(), request.scope());
        let current = match state.records.get(&key) {
            Some(StateRecord::WriterLease(record)) => Some(record.clone()),
            _ => None,
        };
        let last = state
            .last_fencing_tokens
            .get(&(request.filesystem_id(), request.scope()))
            .copied();
        let now = self.clock_now(StateStoreOperation::AcquireLease)?;
        let next_revision = next_authority_revision(&state, StateStoreOperation::AcquireLease)?;
        let record_revision = to_record_revision(next_revision, StateStoreOperation::AcquireLease)?;
        let outcome = match grant_writer_lease(
            request,
            current.as_ref(),
            last,
            now,
            self.contract.writer_topology(),
            record_revision,
        ) {
            Ok(record) => {
                let grant = lease_grant(&record);
                let event = lease_change_event(
                    request.filesystem_id(),
                    request.operation_id(),
                    key.clone(),
                    next_revision,
                    self.contract.limits(),
                    StateStoreOperation::AcquireLease,
                )?;
                state.records.insert(key, StateRecord::WriterLease(record));
                state.last_fencing_tokens.insert(
                    (request.filesystem_id(), request.scope()),
                    grant.fence.fencing_token,
                );
                publish_lease_revision(&mut state, next_revision, event, self.contract.limits());
                AcquireLeaseOutcome::Granted(grant)
            }
            Err(rejection) => AcquireLeaseOutcome::Rejected(rejection),
        };
        state.lease_operations.insert(
            operation_key,
            LeaseOperationResult::Acquire {
                fingerprint,
                outcome,
            },
        );
        let phase = if matches!(outcome, AcquireLeaseOutcome::Granted(_)) {
            MemoryTracePhase::Completed
        } else {
            MemoryTracePhase::Rejected
        };
        traced_outcome(
            &mut state,
            StateStoreOperation::AcquireLease,
            phase,
            outcome,
        )
    }

    /// Idempotently renews the exact current lease without changing its token.
    pub async fn renew_lease_request(
        &self,
        request: RenewWriterLease,
    ) -> Result<RenewLeaseOutcome, MemoryStateStoreError> {
        let fingerprint = request.operation_fingerprint();
        let operation_key = (request.filesystem_id(), request.operation_id());
        let mut state = self.lock(StateStoreOperation::RenewLease)?;
        record_trace(
            &mut state,
            StateStoreOperation::RenewLease,
            MemoryTracePhase::Started,
        )?;
        if let Some(result) = state.lease_operations.get(&operation_key) {
            let outcome = match result {
                LeaseOperationResult::Renew {
                    fingerprint: retained,
                    outcome,
                } if *retained == fingerprint => match outcome {
                    RenewLeaseOutcome::Renewed(grant)
                    | RenewLeaseOutcome::AlreadyApplied(grant) => {
                        RenewLeaseOutcome::AlreadyApplied(*grant)
                    }
                    RenewLeaseOutcome::Rejected(rejection) => {
                        RenewLeaseOutcome::Rejected(*rejection)
                    }
                },
                _ => RenewLeaseOutcome::Rejected(LeaseRejection::OperationMismatch),
            };
            return traced_outcome(
                &mut state,
                StateStoreOperation::RenewLease,
                MemoryTracePhase::Completed,
                outcome,
            );
        }
        if request.validate(self.contract.limits()).is_err() {
            return traced_outcome(
                &mut state,
                StateStoreOperation::RenewLease,
                MemoryTracePhase::Rejected,
                RenewLeaseOutcome::Rejected(LeaseRejection::InvalidDuration),
            );
        }
        if lease_history_full(&state, self.contract.limits()) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::RenewLease,
                MemoryTracePhase::Rejected,
                RenewLeaseOutcome::Rejected(LeaseRejection::OperationHistoryFull),
            );
        }
        let key = RecordKey::WriterLease(request.filesystem_id(), request.fence().scope);
        let current = match state.records.get(&key) {
            Some(StateRecord::WriterLease(record)) => Some(record.clone()),
            _ => None,
        };
        let now = self.clock_now(StateStoreOperation::RenewLease)?;
        let next_revision = next_authority_revision(&state, StateStoreOperation::RenewLease)?;
        let record_revision = to_record_revision(next_revision, StateStoreOperation::RenewLease)?;
        let outcome = match renew_current_lease(
            request,
            current.as_ref(),
            now,
            self.contract.writer_topology(),
            record_revision,
        ) {
            Ok(record) => {
                let grant = lease_grant(&record);
                let event = lease_change_event(
                    request.filesystem_id(),
                    request.operation_id(),
                    key.clone(),
                    next_revision,
                    self.contract.limits(),
                    StateStoreOperation::RenewLease,
                )?;
                state.records.insert(key, StateRecord::WriterLease(record));
                publish_lease_revision(&mut state, next_revision, event, self.contract.limits());
                RenewLeaseOutcome::Renewed(grant)
            }
            Err(rejection) => RenewLeaseOutcome::Rejected(rejection),
        };
        state.lease_operations.insert(
            operation_key,
            LeaseOperationResult::Renew {
                fingerprint,
                outcome,
            },
        );
        let phase = if matches!(outcome, RenewLeaseOutcome::Renewed(_)) {
            MemoryTracePhase::Completed
        } else {
            MemoryTracePhase::Rejected
        };
        traced_outcome(&mut state, StateStoreOperation::RenewLease, phase, outcome)
    }

    /// Idempotently releases the exact current lease without resetting token history.
    pub async fn release_lease_request(
        &self,
        request: ReleaseWriterLease,
    ) -> Result<ReleaseLeaseOutcome, MemoryStateStoreError> {
        let fingerprint = request.operation_fingerprint();
        let operation_key = (request.filesystem_id(), request.operation_id());
        let mut state = self.lock(StateStoreOperation::ReleaseLease)?;
        record_trace(
            &mut state,
            StateStoreOperation::ReleaseLease,
            MemoryTracePhase::Started,
        )?;
        if let Some(result) = state.lease_operations.get(&operation_key) {
            let outcome = match result {
                LeaseOperationResult::Release {
                    fingerprint: retained,
                    outcome,
                } if *retained == fingerprint => match outcome {
                    ReleaseLeaseOutcome::Released | ReleaseLeaseOutcome::AlreadyApplied => {
                        ReleaseLeaseOutcome::AlreadyApplied
                    }
                    ReleaseLeaseOutcome::Rejected(rejection) => {
                        ReleaseLeaseOutcome::Rejected(*rejection)
                    }
                },
                _ => ReleaseLeaseOutcome::Rejected(LeaseRejection::OperationMismatch),
            };
            return traced_outcome(
                &mut state,
                StateStoreOperation::ReleaseLease,
                MemoryTracePhase::Completed,
                outcome,
            );
        }
        if lease_history_full(&state, self.contract.limits()) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::ReleaseLease,
                MemoryTracePhase::Rejected,
                ReleaseLeaseOutcome::Rejected(LeaseRejection::OperationHistoryFull),
            );
        }
        let key = RecordKey::WriterLease(request.filesystem_id(), request.fence().scope);
        let current = match state.records.get(&key) {
            Some(StateRecord::WriterLease(record)) => Some(record.clone()),
            _ => None,
        };
        let now = self.clock_now(StateStoreOperation::ReleaseLease)?;
        let outcome = match validate_lease_release(
            request,
            current.as_ref(),
            now,
            self.contract.writer_topology(),
        ) {
            Ok(()) => {
                let next_revision =
                    next_authority_revision(&state, StateStoreOperation::ReleaseLease)?;
                let event = lease_change_event(
                    request.filesystem_id(),
                    request.operation_id(),
                    key.clone(),
                    next_revision,
                    self.contract.limits(),
                    StateStoreOperation::ReleaseLease,
                )?;
                state.records.remove(&key);
                publish_lease_revision(&mut state, next_revision, event, self.contract.limits());
                ReleaseLeaseOutcome::Released
            }
            Err(rejection) => ReleaseLeaseOutcome::Rejected(rejection),
        };
        state.lease_operations.insert(
            operation_key,
            LeaseOperationResult::Release {
                fingerprint,
                outcome,
            },
        );
        let phase = if matches!(outcome, ReleaseLeaseOutcome::Released) {
            MemoryTracePhase::Completed
        } else {
            MemoryTracePhase::Rejected
        };
        traced_outcome(
            &mut state,
            StateStoreOperation::ReleaseLease,
            phase,
            outcome,
        )
    }

    /// Nonblockingly returns bounded complete revision events or an explicit gap.
    pub async fn poll_changes_request(
        &self,
        request: ChangePoll,
    ) -> Result<ChangePollOutcome, MemoryStateStoreError> {
        let mut state = self.lock(StateStoreOperation::PollChanges)?;
        record_trace(
            &mut state,
            StateStoreOperation::PollChanges,
            MemoryTracePhase::Started,
        )?;
        if let Err(error) = request.validate(self.contract.limits()) {
            return traced_outcome(
                &mut state,
                StateStoreOperation::PollChanges,
                MemoryTracePhase::Rejected,
                ChangePollOutcome::MalformedRequest(error),
            );
        }
        if request.after().revision() > state.revision {
            let outcome = ChangePollOutcome::RevisionUnavailable {
                requested: request.after().revision(),
                current: state.revision,
            };
            return traced_outcome(
                &mut state,
                StateStoreOperation::PollChanges,
                MemoryTracePhase::Completed,
                outcome,
            );
        }
        if request.after().revision() < state.oldest_change_cursor {
            let outcome = ChangePollOutcome::RevisionCompacted {
                oldest_available: state.oldest_change_cursor,
                current_revision: state.revision,
            };
            return traced_outcome(
                &mut state,
                StateStoreOperation::PollChanges,
                MemoryTracePhase::Completed,
                outcome,
            );
        }
        let maximum_events = usize::try_from(request.max_events()).unwrap_or(usize::MAX);
        let maximum_keys = usize::try_from(request.max_keys()).unwrap_or(usize::MAX);
        let mut events = Vec::with_capacity(maximum_events.min(state.changes.len()));
        let mut key_count = 0usize;
        let mut next = request.after();
        let mut stopped_early = false;
        for event in state
            .changes
            .iter()
            .filter(|event| event.revision() > request.after().revision())
        {
            if event.filesystem_id() != request.filesystem_id() {
                next = crate::ChangeCursor::after(event.revision());
                continue;
            }
            if events.len() == maximum_events {
                stopped_early = true;
                break;
            }
            let next_count = key_count.checked_add(event.keys().len()).ok_or_else(|| {
                MemoryStateStoreError::new(
                    StateStoreOperation::PollChanges,
                    AdapterFailureKind::Corruption,
                )
            })?;
            if next_count > maximum_keys {
                if events.is_empty() {
                    let outcome = ChangePollOutcome::PollBoundTooSmall {
                        revision: event.revision(),
                        required_keys: u32::try_from(event.keys().len()).unwrap_or(u32::MAX),
                    };
                    return traced_outcome(
                        &mut state,
                        StateStoreOperation::PollChanges,
                        MemoryTracePhase::Completed,
                        outcome,
                    );
                }
                stopped_early = true;
                break;
            }
            key_count = next_count;
            events.push(event.clone());
            next = crate::ChangeCursor::after(event.revision());
        }
        if !stopped_early {
            next = crate::ChangeCursor::after(state.revision);
        }
        let batch = ChangeBatch::new(
            events,
            &request,
            next,
            state.revision,
            self.contract.limits(),
        )
        .map_err(|_| {
            MemoryStateStoreError::new(
                StateStoreOperation::PollChanges,
                AdapterFailureKind::Corruption,
            )
        })?;
        traced_outcome(
            &mut state,
            StateStoreOperation::PollChanges,
            MemoryTracePhase::Completed,
            ChangePollOutcome::Changes(batch),
        )
    }

    fn clock_now(
        &self,
        operation: StateStoreOperation,
    ) -> Result<crate::LeaseDeadline, MemoryStateStoreError> {
        self.clock
            .now()
            .map_err(|_| MemoryStateStoreError::new(operation, AdapterFailureKind::Internal))
    }

    fn lock(
        &self,
        operation: StateStoreOperation,
    ) -> Result<MutexGuard<'_, AuthorityState>, MemoryStateStoreError> {
        self.state
            .lock()
            .map_err(|_| MemoryStateStoreError::new(operation, AdapterFailureKind::Internal))
    }
}

fn lease_history_full(state: &AuthorityState, limits: StateLimits) -> bool {
    state.lease_operations.len()
        >= usize::try_from(limits.max_lease_operation_history()).unwrap_or(usize::MAX)
}

fn next_authority_revision(
    state: &AuthorityState,
    operation: StateStoreOperation,
) -> Result<StateRevision, MemoryStateStoreError> {
    state
        .revision
        .checked_next()
        .map_err(|_| MemoryStateStoreError::new(operation, AdapterFailureKind::Corruption))
}

fn to_record_revision(
    revision: StateRevision,
    operation: StateStoreOperation,
) -> Result<RecordRevision, MemoryStateStoreError> {
    RecordRevision::new(revision.get())
        .map_err(|_| MemoryStateStoreError::new(operation, AdapterFailureKind::Corruption))
}

fn lease_grant(record: &crate::WriterLeaseRecord) -> WriterLeaseGrant {
    WriterLeaseGrant {
        fence: crate::WriterFence::new(
            record.scope(),
            record.holder(),
            record.lease_id(),
            record.fencing_token(),
        ),
        deadline: record.deadline(),
    }
}

fn lease_change_event(
    filesystem_id: crate::FilesystemId,
    operation_id: LeaseOperationId,
    key: RecordKey,
    revision: StateRevision,
    limits: StateLimits,
    operation: StateStoreOperation,
) -> Result<ChangeEvent, MemoryStateStoreError> {
    ChangeEvent::new(
        filesystem_id,
        revision,
        ChangeOrigin::Lease(operation_id),
        vec![key],
        limits,
    )
    .map_err(|_| MemoryStateStoreError::new(operation, AdapterFailureKind::Corruption))
}

fn publish_lease_revision(
    state: &mut AuthorityState,
    revision: StateRevision,
    event: ChangeEvent,
    limits: StateLimits,
) {
    state.revision = revision;
    state.changes.push_back(event);
    while state.changes.len()
        > usize::try_from(limits.max_change_history_commits()).unwrap_or(usize::MAX)
    {
        if let Some(compacted) = state.changes.pop_front() {
            state.oldest_change_cursor = compacted.revision();
        }
    }
}

fn record_trace(
    state: &mut AuthorityState,
    operation: StateStoreOperation,
    phase: MemoryTracePhase,
) -> Result<(), MemoryStateStoreError> {
    let sequence = u64::try_from(state.trace.len())
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| MemoryStateStoreError::new(operation, AdapterFailureKind::Internal))?;
    state.trace.push(MemoryTraceEvent {
        sequence,
        operation,
        phase,
    });
    Ok(())
}

fn traced_outcome<T>(
    state: &mut AuthorityState,
    operation: StateStoreOperation,
    phase: MemoryTracePhase,
    outcome: T,
) -> Result<T, MemoryStateStoreError> {
    record_trace(state, operation, phase)?;
    Ok(outcome)
}

fn evaluate_preconditions(
    records: &BTreeMap<RecordKey, StateRecord>,
    request: &CommitRequest,
) -> Option<CommitConflict> {
    for (index, precondition) in request.preconditions().iter().enumerate() {
        let conflict = match precondition {
            Precondition::RecordAbsent(key) if records.contains_key(key) => {
                Some(CommitConflictKind::RecordPresent(key.clone()))
            }
            Precondition::RecordRevision { key, expected } => match records.get(key) {
                None => Some(CommitConflictKind::RecordMissing(key.clone())),
                Some(record) if record.revision() != *expected => {
                    Some(CommitConflictKind::RecordRevision {
                        key: key.clone(),
                        expected: *expected,
                        actual: record.revision(),
                    })
                }
                _ => None,
            },
            Precondition::InodeGeneration { inode_id, expected } => {
                match records.get(&RecordKey::Inode(request.filesystem_id(), *inode_id)) {
                    Some(StateRecord::Inode(inode)) if inode.inode_generation() == *expected => {
                        None
                    }
                    _ => Some(CommitConflictKind::InodeGeneration),
                }
            }
            Precondition::DataGeneration { inode_id, expected } => {
                match records.get(&RecordKey::Inode(request.filesystem_id(), *inode_id)) {
                    Some(StateRecord::Inode(inode)) if inode.data_generation() == *expected => None,
                    _ => Some(CommitConflictKind::DataGeneration),
                }
            }
            Precondition::DirectoryGeneration { inode_id, expected } => {
                match records.get(&RecordKey::Inode(request.filesystem_id(), *inode_id)) {
                    Some(StateRecord::Inode(inode))
                        if inode.directory_generation() == Some(*expected) =>
                    {
                        None
                    }
                    _ => Some(CommitConflictKind::DirectoryGeneration),
                }
            }
            Precondition::ContentBase { inode_id, expected } => {
                match records.get(&RecordKey::Inode(request.filesystem_id(), *inode_id)) {
                    Some(StateRecord::Inode(inode)) if inode.content_base() == Some(*expected) => {
                        None
                    }
                    _ => Some(CommitConflictKind::ContentBase),
                }
            }
            Precondition::LinkCount { inode_id, expected } => {
                match records.get(&RecordKey::Inode(request.filesystem_id(), *inode_id)) {
                    Some(StateRecord::Inode(inode)) if inode.link_count() == *expected => None,
                    _ => Some(CommitConflictKind::LinkCount),
                }
            }
            Precondition::OpenPinCount { inode_id, expected } => {
                let count = records
                    .keys()
                    .filter(|key| {
                        matches!(
                            key,
                            RecordKey::OpenPin(filesystem_id, candidate, _)
                                if *filesystem_id == request.filesystem_id()
                                    && *candidate == *inode_id
                        )
                    })
                    .count();
                if u64::try_from(count) == Ok(*expected) {
                    None
                } else {
                    Some(CommitConflictKind::OpenPinCount)
                }
            }
            Precondition::FilesystemPolicyGeneration { expected } => {
                match records.get(&RecordKey::Filesystem(request.filesystem_id())) {
                    Some(StateRecord::Filesystem(filesystem))
                        if filesystem.policy_generation() == *expected =>
                    {
                        None
                    }
                    _ => Some(CommitConflictKind::FilesystemPolicyGeneration),
                }
            }
            Precondition::ExactFence(fence) if *fence != request.fence() => {
                Some(CommitConflictKind::ExactFence)
            }
            _ => None,
        };
        if let Some(kind) = conflict {
            return Some(CommitConflict {
                precondition_index: index,
                kind,
            });
        }
    }
    None
}

fn validate_directory_cookie_transition(
    records: &BTreeMap<RecordKey, StateRecord>,
    request: &CommitRequest,
) -> Result<(), MalformedCommit> {
    let Some(StateRecord::Filesystem(filesystem)) =
        records.get(&RecordKey::Filesystem(request.filesystem_id()))
    else {
        // The bootstrap transaction is validated as a complete final record set.
        return Ok(());
    };
    let deleted: Vec<_> = request
        .changes()
        .iter()
        .filter_map(|change| {
            let StateChange::Delete(key) = change else {
                return None;
            };
            match records.get(key) {
                Some(StateRecord::DirectoryEntry(entry)) => Some(entry),
                _ => None,
            }
        })
        .collect();
    let mut next = filesystem.next_directory_cookie();
    let mut allocations = 0u64;
    let mut declared = 0u64;
    for change in request.changes() {
        match change {
            StateChange::Insert {
                record: StateRecord::DirectoryEntry(entry),
                ..
            } => {
                let transfer = deleted.iter().any(|old| {
                    old.cookie() == entry.cookie() && old.child_inode_id() == entry.child_inode_id()
                });
                if !transfer {
                    if entry.cookie() != next {
                        return Err(MalformedCommit::DirectoryCookieAllocation);
                    }
                    next = next
                        .checked_next()
                        .map_err(|_| MalformedCommit::Arithmetic)?;
                    allocations = allocations
                        .checked_add(1)
                        .ok_or(MalformedCommit::Arithmetic)?;
                }
            }
            StateChange::Replace {
                key,
                record: StateRecord::DirectoryEntry(replacement),
            } => {
                let Some(StateRecord::DirectoryEntry(current)) = records.get(key) else {
                    continue;
                };
                if current.cookie() != replacement.cookie() {
                    return Err(MalformedCommit::DirectoryCookieAllocation);
                }
            }
            StateChange::AdvanceDirectoryCookie { count } => {
                declared = declared
                    .checked_add(count.get())
                    .ok_or(MalformedCommit::Arithmetic)?;
            }
            _ => {}
        }
    }
    if allocations != declared {
        return Err(MalformedCommit::DirectoryCookieAllocation);
    }
    Ok(())
}

fn validate_qid_path_transition(
    records: &BTreeMap<RecordKey, StateRecord>,
    request: &CommitRequest,
) -> Result<(), MalformedCommit> {
    let Some(StateRecord::Filesystem(filesystem)) =
        records.get(&RecordKey::Filesystem(request.filesystem_id()))
    else {
        // Bootstrap inserts the filesystem header and root inode together.
        return Ok(());
    };
    let mut next = filesystem.next_qid_path();
    let mut allocations = 0u64;
    let mut declared = 0u64;
    for change in request.changes() {
        match change {
            StateChange::Insert {
                record: StateRecord::Inode(inode),
                ..
            } => {
                if inode.qid_path() != next {
                    return Err(MalformedCommit::QidPathAllocation);
                }
                next = next
                    .checked_next()
                    .map_err(|_| MalformedCommit::Arithmetic)?;
                allocations = allocations
                    .checked_add(1)
                    .ok_or(MalformedCommit::Arithmetic)?;
            }
            StateChange::Replace {
                key,
                record: StateRecord::Inode(replacement),
            } => {
                let Some(StateRecord::Inode(current)) = records.get(key) else {
                    continue;
                };
                if current.qid_path() != replacement.qid_path() {
                    return Err(MalformedCommit::QidPathAllocation);
                }
            }
            StateChange::AdvanceQidPath { count } => {
                declared = declared
                    .checked_add(count.get())
                    .ok_or(MalformedCommit::Arithmetic)?;
            }
            _ => {}
        }
    }
    if allocations != declared {
        return Err(MalformedCommit::QidPathAllocation);
    }
    Ok(())
}

fn validate_monotonic_transitions(
    records: &BTreeMap<RecordKey, StateRecord>,
    request: &CommitRequest,
) -> Result<(), MalformedCommit> {
    if !records.contains_key(&RecordKey::Filesystem(request.filesystem_id())) {
        return Ok(());
    }
    let namespace_parents: std::collections::BTreeSet<_> = request
        .changes()
        .iter()
        .filter_map(|change| match change {
            StateChange::Insert {
                record: StateRecord::DirectoryEntry(entry),
                ..
            } => Some(entry.parent_inode_id()),
            StateChange::Delete(key) => match records.get(key) {
                Some(StateRecord::DirectoryEntry(entry)) => Some(entry.parent_inode_id()),
                _ => None,
            },
            StateChange::Replace {
                record: StateRecord::DirectoryEntry(entry),
                ..
            } => Some(entry.parent_inode_id()),
            _ => None,
        })
        .collect();

    for parent in &namespace_parents {
        let advances = request.changes().iter().any(|change| match change {
            StateChange::BumpDirectoryGeneration(inode_id) => inode_id == parent,
            StateChange::Replace {
                key,
                record: StateRecord::Inode(replacement),
            } if *key == RecordKey::Inode(request.filesystem_id(), *parent) => {
                match records.get(key) {
                    Some(StateRecord::Inode(current)) => {
                        directory_generation_advances_once(current, replacement)
                    }
                    _ => false,
                }
            }
            _ => false,
        });
        if !advances {
            return Err(MalformedCommit::NamespaceGeneration);
        }
    }

    for change in request.changes() {
        let StateChange::Replace { key, record } = change else {
            continue;
        };
        let Some(current) = records.get(key) else {
            continue;
        };
        let valid = match (current, record) {
            (StateRecord::Filesystem(current), StateRecord::Filesystem(replacement)) => {
                current.root_inode_id() == replacement.root_inode_id()
                    && current.next_qid_path() == replacement.next_qid_path()
                    && current.next_directory_cookie() == replacement.next_directory_cookie()
                    && current.policy_generation() == replacement.policy_generation()
            }
            (StateRecord::Inode(current), StateRecord::Inode(replacement)) => {
                let expected_inode = current.inode_generation().checked_next().ok();
                let directory_valid = match (
                    current.directory_generation(),
                    replacement.directory_generation(),
                ) {
                    (Some(old), Some(new)) if namespace_parents.contains(&current.inode_id()) => {
                        old.checked_next().ok() == Some(new)
                    }
                    (Some(old), Some(new)) => old == new,
                    (None, None) => true,
                    _ => false,
                };
                current.kind() == replacement.kind()
                    && expected_inode == Some(replacement.inode_generation())
                    && current.content() == replacement.content()
                    && current.content_file_id() == replacement.content_file_id()
                    && current.content_context_id() == replacement.content_context_id()
                    && directory_valid
                    && directory_parent_transition_valid(records, request, current, replacement)
            }
            (StateRecord::DirectoryEntry(current), StateRecord::DirectoryEntry(replacement)) => {
                current.cookie() == replacement.cookie()
                    && current.parent_inode_id() == replacement.parent_inode_id()
            }
            (StateRecord::Lock(current), StateRecord::Lock(replacement)) => {
                current.generation().checked_next().ok() == Some(replacement.generation())
            }
            (StateRecord::Orphan(current), StateRecord::Orphan(replacement)) => {
                current.orphaned_revision() == replacement.orphaned_revision()
            }
            _ => true,
        };
        if !valid {
            return Err(MalformedCommit::NonMonotonicTransition);
        }
    }
    Ok(())
}

fn directory_parent_transition_valid(
    records: &BTreeMap<RecordKey, StateRecord>,
    request: &CommitRequest,
    current: &crate::InodeRecord,
    replacement: &crate::InodeRecord,
) -> bool {
    let (Some(old_parent), Some(new_parent)) =
        (current.directory_parent(), replacement.directory_parent())
    else {
        return current.directory_parent() == replacement.directory_parent();
    };
    if old_parent == new_parent {
        return true;
    }
    let deleted = request.changes().iter().find_map(|change| {
        let StateChange::Delete(key) = change else {
            return None;
        };
        match records.get(key) {
            Some(StateRecord::DirectoryEntry(entry))
                if entry.child_inode_id() == current.inode_id()
                    && entry.parent_inode_id() == old_parent =>
            {
                Some(entry)
            }
            _ => None,
        }
    });
    let inserted = request.changes().iter().find_map(|change| match change {
        StateChange::Insert {
            record: StateRecord::DirectoryEntry(entry),
            ..
        } if entry.child_inode_id() == current.inode_id()
            && entry.parent_inode_id() == new_parent =>
        {
            Some(entry)
        }
        _ => None,
    });
    matches!((deleted, inserted), (Some(old), Some(new)) if old.cookie() == new.cookie())
}

fn directory_generation_advances_once(
    current: &crate::InodeRecord,
    replacement: &crate::InodeRecord,
) -> bool {
    matches!(
        (current.directory_generation(), replacement.directory_generation()),
        (Some(old), Some(new)) if old.checked_next().ok() == Some(new)
    )
}

fn validate_xattr_transitions(
    records: &BTreeMap<RecordKey, StateRecord>,
    request: &CommitRequest,
    limits: StateLimits,
) -> Result<(), MalformedCommit> {
    let mut transaction_bytes = request
        .retained_bytes()
        .ok_or(MalformedCommit::Arithmetic)?;
    for change in request.changes() {
        if let StateChange::PublishXattrStaging(publication) = change {
            let key = RecordKey::XattrStaging(request.filesystem_id(), publication.staging_id);
            let Some(StateRecord::XattrStaging(staging)) = records.get(&key) else {
                return Err(MalformedCommit::InvalidXattrStaging);
            };
            if staging.inode_id() != publication.inode_id
                || staging.name() != &publication.name
                || !staging.is_complete()
            {
                return Err(MalformedCommit::InvalidXattrStaging);
            }
            transaction_bytes = transaction_bytes
                .checked_add(staging.bytes().as_bytes().len())
                .ok_or(MalformedCommit::Arithmetic)?;
        }
        let StateChange::Delete(staging_key @ RecordKey::XattrStaging(_, _)) = change else {
            continue;
        };
        let Some(StateRecord::XattrStaging(staging)) = records.get(staging_key) else {
            continue;
        };
        let bypass = request.changes().iter().any(|candidate| match candidate {
            StateChange::Insert {
                record: StateRecord::Xattr(xattr),
                ..
            }
            | StateChange::Replace {
                record: StateRecord::Xattr(xattr),
                ..
            } => xattr.inode_id() == staging.inode_id() && xattr.name() == staging.name(),
            _ => false,
        });
        if bypass {
            return Err(MalformedCommit::XattrPublicationRequired);
        }
    }
    limits
        .require_bytes(
            crate::StateLimitKind::TransactionBytes,
            transaction_bytes,
            limits.max_transaction_bytes(),
        )
        .map_err(MalformedCommit::Limit)?;
    Ok(())
}

enum ApplyFailure {
    Conflict(CommitConflictKind),
    Malformed(MalformedCommit),
}

fn apply_change(
    records: &mut BTreeMap<RecordKey, StateRecord>,
    filesystem_id: crate::FilesystemId,
    mutation: crate::MutationContext,
    change: &StateChange,
    revision: RecordRevision,
    limits: StateLimits,
) -> Result<(), ApplyFailure> {
    let key = change.primary_key(filesystem_id);
    match change {
        StateChange::Insert { record, .. } => {
            if records.contains_key(&key) {
                return Err(ApplyFailure::Conflict(CommitConflictKind::RecordPresent(
                    key,
                )));
            }
            if let StateRecord::Lock(requested) = record {
                let conflict = records.iter().find_map(|(candidate_key, candidate)| {
                    match (candidate_key, candidate) {
                        (RecordKey::Lock(fs, _, _), StateRecord::Lock(existing))
                            if *fs == filesystem_id && requested.conflicts_with(existing) =>
                        {
                            Some(existing.lock_id())
                        }
                        _ => None,
                    }
                });
                if let Some(existing) = conflict {
                    return Err(ApplyFailure::Conflict(CommitConflictKind::LockConflict {
                        existing,
                    }));
                }
            }
            records.insert(key, record.clone().with_revision(revision));
        }
        StateChange::Replace { record, .. } => {
            let Some(current) = records.get(&key) else {
                return Err(ApplyFailure::Conflict(CommitConflictKind::RecordMissing(
                    key,
                )));
            };
            if let (StateRecord::Inode(current), StateRecord::Inode(replacement)) =
                (current, record)
                && (current.content() != replacement.content()
                    || current.content_file_id() != replacement.content_file_id())
            {
                return Err(ApplyFailure::Malformed(
                    MalformedCommit::ContentReplacementRequiresPreparedPublication,
                ));
            }
            records.insert(key, record.clone().with_revision(revision));
        }
        StateChange::Delete(_) => {
            if records.remove(&key).is_none() {
                return Err(ApplyFailure::Conflict(CommitConflictKind::RecordMissing(
                    key,
                )));
            }
        }
        StateChange::AdvanceDirectoryCookie { count } => {
            mutate_record(records, &key, revision, |record| match record {
                StateRecord::Filesystem(record) => record.advance_directory_cookie(*count),
                _ => Err(crate::CounterOverflow {
                    field: "FilesystemRecordKind",
                }),
            })?;
        }
        StateChange::AdvanceQidPath { count } => {
            mutate_record(records, &key, revision, |record| match record {
                StateRecord::Filesystem(record) => record.advance_qid_path(*count),
                _ => Err(crate::CounterOverflow {
                    field: "FilesystemRecordKind",
                }),
            })?;
        }
        StateChange::BumpFilesystemPolicyGeneration => {
            mutate_record(records, &key, revision, |record| match record {
                StateRecord::Filesystem(record) => record.bump_policy_generation(),
                _ => Err(crate::CounterOverflow {
                    field: "FilesystemRecordKind",
                }),
            })?;
        }
        StateChange::BumpInodeGeneration(_) => {
            mutate_record(records, &key, revision, |record| match record {
                StateRecord::Inode(record) => record.bump_inode_generation(),
                _ => Err(crate::CounterOverflow {
                    field: "InodeRecordKind",
                }),
            })?;
        }
        StateChange::BumpDirectoryGeneration(_) => {
            mutate_record(records, &key, revision, |record| match record {
                StateRecord::Inode(record) => record.bump_directory_generation(),
                _ => Err(crate::CounterOverflow {
                    field: "DirectoryRecordKind",
                }),
            })?;
        }
        StateChange::AdjustLinkCount { adjustment, .. } => {
            mutate_record(records, &key, revision, |record| match record {
                StateRecord::Inode(record) => record.adjust_link_count(*adjustment),
                _ => Err(crate::CounterOverflow {
                    field: "InodeRecordKind",
                }),
            })?;
        }
        StateChange::AdjustOpenPinCount { adjustment, .. } => {
            mutate_record(records, &key, revision, |record| match record {
                StateRecord::Orphan(record) => record.adjust_open_pin_count(*adjustment),
                _ => Err(crate::CounterOverflow {
                    field: "OrphanRecordKind",
                }),
            })?;
        }
        StateChange::BumpLockGeneration { .. } => {
            mutate_record(records, &key, revision, |record| match record {
                StateRecord::Lock(record) => record.bump_generation(),
                _ => Err(crate::CounterOverflow {
                    field: "LockRecordKind",
                }),
            })?;
        }
        StateChange::PublishContent(publication) => {
            let metadata = records
                .get(&key)
                .and_then(|record| match record {
                    StateRecord::Inode(inode) => inode.content_file_id(),
                    _ => None,
                })
                .and_then(|file_id| {
                    records.get(&RecordKey::ContentMetadata(filesystem_id, file_id))
                })
                .and_then(|record| match record {
                    StateRecord::ContentMetadata(metadata) => Some(metadata.clone()),
                    _ => None,
                });
            let Some(StateRecord::Inode(inode)) = records.get_mut(&key) else {
                return Err(ApplyFailure::Conflict(CommitConflictKind::RecordMissing(
                    key,
                )));
            };
            let validation = metadata
                .as_ref()
                .ok_or(PublishContentError::ContentContextMismatch)
                .and_then(|metadata| {
                    validate_publish_content_with_metadata(publication, &mutation, inode, metadata)
                });
            validation.map_err(|error| {
                ApplyFailure::Malformed(MalformedCommit::InvalidPublication(error))
            })?;
            inode.publish_content(publication);
            let updated = records
                .remove(&key)
                .expect("publication inode remains present")
                .with_revision(revision);
            records.insert(key, updated);
        }
        StateChange::RewrapContentMetadata(rewrap) => {
            let Some(StateRecord::ContentMetadata(metadata)) = records.get_mut(&key) else {
                return Err(ApplyFailure::Conflict(CommitConflictKind::RecordMissing(
                    key,
                )));
            };
            if metadata.revision() != rewrap.expected_revision {
                return Err(ApplyFailure::Conflict(CommitConflictKind::RecordRevision {
                    key,
                    expected: rewrap.expected_revision,
                    actual: metadata.revision(),
                }));
            }
            metadata
                .rewrap(
                    rewrap.expected_context_id,
                    rewrap.wrapped_key_bytes.clone(),
                    limits,
                )
                .map_err(|_| {
                    ApplyFailure::Malformed(MalformedCommit::InvalidContentMetadataRewrap)
                })?;
            let updated = records
                .remove(&key)
                .expect("rewrapped metadata remains present")
                .with_revision(revision);
            records.insert(key, updated);
        }
        StateChange::PublishXattrStaging(publication) => {
            let staging_key = RecordKey::XattrStaging(filesystem_id, publication.staging_id);
            let Some(StateRecord::XattrStaging(staging)) = records.get(&staging_key) else {
                return Err(ApplyFailure::Malformed(
                    MalformedCommit::InvalidXattrStaging,
                ));
            };
            if staging.inode_id() != publication.inode_id
                || staging.name() != &publication.name
                || !staging.is_complete()
            {
                return Err(ApplyFailure::Malformed(
                    MalformedCommit::InvalidXattrStaging,
                ));
            }
            let xattr = crate::XattrRecord::new(
                publication.inode_id,
                publication.name.clone(),
                staging.bytes().clone(),
                revision,
            );
            records.remove(&staging_key);
            records.insert(
                RecordKey::Xattr(
                    filesystem_id,
                    publication.inode_id,
                    publication.name.clone(),
                ),
                StateRecord::Xattr(xattr),
            );
        }
    }
    Ok(())
}

fn mutate_record(
    records: &mut BTreeMap<RecordKey, StateRecord>,
    key: &RecordKey,
    revision: RecordRevision,
    mutate: impl FnOnce(&mut StateRecord) -> Result<(), crate::CounterOverflow>,
) -> Result<(), ApplyFailure> {
    let Some(mut record) = records.remove(key) else {
        return Err(ApplyFailure::Conflict(CommitConflictKind::RecordMissing(
            key.clone(),
        )));
    };
    mutate(&mut record).map_err(|_| ApplyFailure::Malformed(MalformedCommit::Arithmetic))?;
    records.insert(key.clone(), record.with_revision(revision));
    Ok(())
}

type ScanCandidate<'a> = (&'a RecordKey, &'a StateRecord, ScanResume);

fn read_directory_page(
    state: &AuthorityState,
    filesystem_id: crate::FilesystemId,
    parent_inode_id: crate::InodeId,
    after: DirectoryCookie,
    bounds: crate::ScanBounds,
) -> Result<Result<DirectoryPage, usize>, MemoryStateStoreError> {
    let maximum = usize::try_from(bounds.max_items()).unwrap_or(usize::MAX);
    let candidate_limit = maximum.checked_add(1).ok_or_else(|| {
        MemoryStateStoreError::new(StateStoreOperation::Read, AdapterFailureKind::Internal)
    })?;
    let candidates = directory_candidates(
        state,
        filesystem_id,
        parent_inode_id,
        after,
        candidate_limit,
    );
    let mut entries = Vec::with_capacity(candidates.len().min(maximum));
    let mut retained_bytes = 0usize;
    let mut consumed = 0usize;
    for (_, record, _) in &candidates {
        if entries.len() == maximum {
            break;
        }
        let StateRecord::DirectoryEntry(entry) = record else {
            return Err(MemoryStateStoreError::new(
                StateStoreOperation::Read,
                AdapterFailureKind::Corruption,
            ));
        };
        let child_key = RecordKey::Inode(filesystem_id, entry.child_inode_id());
        let Some(StateRecord::Inode(child)) = state.records.get(&child_key) else {
            return Err(MemoryStateStoreError::new(
                StateStoreOperation::Read,
                AdapterFailureKind::Corruption,
            ));
        };
        let page_entry = DirectoryPageEntry::new(
            entry.clone(),
            child.kind(),
            child.qid_path(),
            child.revision(),
        );
        let item_bytes = page_entry.retained_bytes().ok_or_else(|| {
            MemoryStateStoreError::new(StateStoreOperation::Read, AdapterFailureKind::Corruption)
        })?;
        let next_bytes = retained_bytes.checked_add(item_bytes).ok_or_else(|| {
            MemoryStateStoreError::new(StateStoreOperation::Read, AdapterFailureKind::Corruption)
        })?;
        if next_bytes > bounds.max_bytes() {
            if entries.is_empty() {
                return Ok(Err(item_bytes));
            }
            break;
        }
        retained_bytes = next_bytes;
        entries.push(page_entry);
        consumed += 1;
    }
    let resume = (consumed < candidates.len())
        .then(|| entries.last().map(|entry| entry.entry().cookie()))
        .flatten();
    Ok(Ok(DirectoryPage::new(entries, resume)))
}

fn read_scan(
    state: &AuthorityState,
    filesystem_id: crate::FilesystemId,
    scan: &RecordScan,
) -> Result<Result<ScanPage, usize>, MemoryStateStoreError> {
    let bounds = scan.bounds();
    let maximum = usize::try_from(bounds.max_items()).unwrap_or(usize::MAX);
    let candidate_limit = maximum.checked_add(1).ok_or_else(|| {
        MemoryStateStoreError::new(StateStoreOperation::Read, AdapterFailureKind::Internal)
    })?;
    let candidates = if let RecordScan::DirectoryEntries {
        parent_inode_id,
        after,
        ..
    } = scan
    {
        directory_candidates(
            state,
            filesystem_id,
            *parent_inode_id,
            *after,
            candidate_limit,
        )
    } else {
        state
            .records
            .iter()
            .filter_map(|(key, record)| {
                scan_resume(key, record, filesystem_id, scan).map(|resume| (key, record, resume))
            })
            .take(candidate_limit)
            .collect()
    };

    let mut records = Vec::with_capacity(candidates.len().min(maximum));
    let mut retained_bytes = 0usize;
    let mut last_resume = None;
    let mut consumed = 0usize;
    for (key, record, resume) in &candidates {
        if records.len() == maximum {
            break;
        }
        let item_bytes = key
            .retained_bytes()
            .and_then(|bytes| bytes.checked_add(record.retained_bytes()?))
            .ok_or_else(|| {
                MemoryStateStoreError::new(
                    StateStoreOperation::Read,
                    AdapterFailureKind::Corruption,
                )
            })?;
        let next_bytes = retained_bytes.checked_add(item_bytes).ok_or_else(|| {
            MemoryStateStoreError::new(StateStoreOperation::Read, AdapterFailureKind::Corruption)
        })?;
        if next_bytes > bounds.max_bytes() {
            if records.is_empty() {
                return Ok(Err(item_bytes));
            }
            break;
        }
        retained_bytes = next_bytes;
        records.push(((*key).clone(), (*record).clone()));
        last_resume = Some((*resume).clone());
        consumed += 1;
    }
    let more = consumed < candidates.len();
    let resume = if more { last_resume } else { None };
    let page = ScanPage::new(scan.family(), records, resume).map_err(|_| {
        MemoryStateStoreError::new(StateStoreOperation::Read, AdapterFailureKind::Corruption)
    })?;
    Ok(Ok(page))
}

fn directory_candidates(
    state: &AuthorityState,
    filesystem_id: crate::FilesystemId,
    parent_inode_id: crate::InodeId,
    after: DirectoryCookie,
    limit: usize,
) -> Vec<ScanCandidate<'_>> {
    let mut selected = BTreeMap::new();
    for (key, record) in &state.records {
        let (RecordKey::DirectoryEntry(fs, parent, name), StateRecord::DirectoryEntry(entry)) =
            (key, record)
        else {
            continue;
        };
        if *fs != filesystem_id || *parent != parent_inode_id || entry.cookie() <= after {
            continue;
        }
        selected.insert((entry.cookie(), name), (key, record));
        if selected.len() > limit {
            selected.pop_last();
        }
    }
    selected
        .into_iter()
        .map(|((cookie, _), (key, record))| (key, record, ScanResume::DirectoryEntry(cookie)))
        .collect()
}

fn scan_resume(
    key: &RecordKey,
    _record: &StateRecord,
    filesystem_id: crate::FilesystemId,
    scan: &RecordScan,
) -> Option<ScanResume> {
    match (key, scan) {
        (RecordKey::Inode(fs, inode_id), RecordScan::Inodes { after, .. })
            if *fs == filesystem_id && after.is_none_or(|cursor| *inode_id > cursor) =>
        {
            Some(ScanResume::Inode(*inode_id))
        }
        (RecordKey::ContentMetadata(fs, file_id), RecordScan::ContentMetadata { after, .. })
            if *fs == filesystem_id && after.is_none_or(|cursor| *file_id > cursor) =>
        {
            Some(ScanResume::ContentMetadata(*file_id))
        }
        (RecordKey::Open(fs, open_id), RecordScan::Opens { after, .. })
            if *fs == filesystem_id && after.is_none_or(|cursor| *open_id > cursor) =>
        {
            Some(ScanResume::Open(*open_id))
        }
        (RecordKey::OpenPin(fs, inode_id, open_id), RecordScan::OpenPins { after, .. })
            if *fs == filesystem_id
                && after.is_none_or(|cursor| {
                    (*inode_id, *open_id) > (cursor.inode_id, cursor.open_id)
                }) =>
        {
            Some(ScanResume::OpenPin(OpenPinCursor {
                inode_id: *inode_id,
                open_id: *open_id,
            }))
        }
        (RecordKey::Orphan(fs, inode_id), RecordScan::Orphans { after, .. })
            if *fs == filesystem_id && after.is_none_or(|cursor| *inode_id > cursor) =>
        {
            Some(ScanResume::Orphan(*inode_id))
        }
        (RecordKey::Lock(fs, inode_id, lock_id), RecordScan::Locks { after, .. })
            if *fs == filesystem_id
                && after.is_none_or(|cursor| {
                    (*inode_id, *lock_id) > (cursor.inode_id, cursor.lock_id)
                }) =>
        {
            Some(ScanResume::Lock(LockCursor {
                inode_id: *inode_id,
                lock_id: *lock_id,
            }))
        }
        (RecordKey::Xattr(fs, inode_id, name), RecordScan::Xattrs { after, .. })
            if *fs == filesystem_id
                && after
                    .as_ref()
                    .is_none_or(|cursor| (*inode_id, name) > (cursor.inode_id, &cursor.name)) =>
        {
            Some(ScanResume::Xattr(XattrCursor {
                inode_id: *inode_id,
                name: name.clone(),
            }))
        }
        (RecordKey::XattrStaging(fs, staging_id), RecordScan::XattrStaging { after, .. })
            if *fs == filesystem_id && after.is_none_or(|cursor| *staging_id > cursor) =>
        {
            Some(ScanResume::XattrStaging(*staging_id))
        }
        (RecordKey::Mutation(fs, mutation_id), RecordScan::Mutations { after, .. })
            if *fs == filesystem_id && after.is_none_or(|cursor| *mutation_id > cursor) =>
        {
            Some(ScanResume::Mutation(*mutation_id))
        }
        (RecordKey::WriterLease(fs, scope), RecordScan::WriterLeases { after, .. })
            if *fs == filesystem_id && after.is_none_or(|cursor| *scope > cursor) =>
        {
            Some(ScanResume::WriterLease(*scope))
        }
        _ => None,
    }
}

/// Definitive infrastructure failure from the deterministic reference authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryStateStoreError {
    operation: StateStoreOperation,
    kind: AdapterFailureKind,
}

impl MemoryStateStoreError {
    const fn new(operation: StateStoreOperation, kind: AdapterFailureKind) -> Self {
        Self { operation, kind }
    }
}

impl fmt::Display for MemoryStateStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "memory state store {:?} failure during {:?}",
            self.kind, self.operation
        )
    }
}

impl std::error::Error for MemoryStateStoreError {}

impl StateStoreAdapterError for MemoryStateStoreError {
    fn kind(&self) -> AdapterFailureKind {
        self.kind
    }

    fn operation(&self) -> StateStoreOperation {
        self.operation
    }
}

impl super::conformance::StateStoreConformanceHarness for MemoryAuthority {
    type Store = MemoryStateStore;
    type Error = MemoryStateStoreError;

    fn open_client(&self) -> Self::Store {
        MemoryAuthority::open_client(self)
    }

    fn advance_time(&self, ticks: u64) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let clock = self.clock.clone();
        async move {
            clock.advance(ticks).map(|_| ()).map_err(|_| {
                MemoryStateStoreError::new(
                    StateStoreOperation::AcquireLease,
                    AdapterFailureKind::Internal,
                )
            })
        }
    }

    fn inject_commit_failure(
        &self,
        timing: CommitFailureTiming,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let authority = self.clone();
        async move { MemoryAuthority::inject_commit_failure(&authority, timing) }
    }

    fn compact_change_history(&self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let authority = self.clone();
        async move {
            let mut state = authority.state.lock().map_err(|_| {
                MemoryStateStoreError::new(
                    StateStoreOperation::PollChanges,
                    AdapterFailureKind::Internal,
                )
            })?;
            while state.changes.len() > 1 {
                if let Some(compacted) = state.changes.pop_front() {
                    state.oldest_change_cursor = compacted.revision();
                }
            }
            Ok(())
        }
    }

    fn open_isolated_with_limits(
        &self,
        limits: StateLimits,
    ) -> impl Future<Output = Result<Self::Store, Self::Error>> + Send {
        let topology = self.contract.writer_topology();
        async move {
            Ok(MemoryAuthority::new(
                topology,
                limits,
                ManualLeaseClock::new(crate::LeaseDeadline::new(0)),
            )
            .open_client())
        }
    }
}

impl FilesystemStateStore for MemoryStateStore {
    type Error = MemoryStateStoreError;

    fn contract(&self) -> StateStoreContract {
        self.contract
    }

    fn read(
        &self,
        request: ReadBatch,
    ) -> impl Future<Output = Result<ReadOutcome, Self::Error>> + Send {
        self.read_request(request)
    }

    fn commit(
        &self,
        request: CommitRequest,
    ) -> impl Future<Output = Result<CommitOutcome, Self::Error>> + Send {
        self.commit_request(request)
    }

    fn acquire_writer_lease(
        &self,
        request: AcquireWriterLease,
    ) -> impl Future<Output = Result<AcquireLeaseOutcome, Self::Error>> + Send {
        self.acquire_lease_request(request)
    }

    fn renew_writer_lease(
        &self,
        request: RenewWriterLease,
    ) -> impl Future<Output = Result<RenewLeaseOutcome, Self::Error>> + Send {
        self.renew_lease_request(request)
    }

    fn release_writer_lease(
        &self,
        request: ReleaseWriterLease,
    ) -> impl Future<Output = Result<ReleaseLeaseOutcome, Self::Error>> + Send {
        self.release_lease_request(request)
    }

    fn poll_changes(
        &self,
        request: ChangePoll,
    ) -> impl Future<Output = Result<ChangePollOutcome, Self::Error>> + Send {
        self.poll_changes_request(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AcquireWriterLease, ClientIncarnationId, DirectoryEntryRecord, DirectoryGeneration,
        EntryName, FencingToken, FilesystemId, FilesystemRecord, GroupId, InodeData,
        InodeGeneration, InodeId, InodeRecord, InodeTimes, LeaseDeadline, LeaseDuration, LeaseId,
        LeaseOperationId, LeaseTimeAuthority, LockGeneration, LockId, LockKind, LockOwner,
        LockRange, LockRecord, MutationContext, MutationResult, MutationResultKind,
        MutationRetention, Precondition, PrincipalId, QidPath, ReadConsistency, ReadQuery,
        RecordRevision, ResultFormatVersion, ScanBounds, StateChange, UnixTimestamp, WriterFence,
        WriterIncarnationId, WriterLeaseRecord, WriterScopeId,
    };

    #[test]
    fn independently_opened_clients_share_only_authoritative_backing() {
        let clock = ManualLeaseClock::new(LeaseDeadline::new(10));
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            StateLimits::default(),
            clock,
        );
        let first = authority.open_client();
        let second = authority.open_client();
        assert!(first.shares_authority_with(&second));
        assert_eq!(
            first.current_revision().unwrap(),
            second.current_revision().unwrap()
        );
        authority.clock().advance(1).unwrap();
        assert_eq!(second.clock.now().unwrap(), LeaseDeadline::new(11));
        assert!(first.state.lock().unwrap().records.is_empty());
    }

    #[test]
    fn one_lock_reads_preserve_positions_and_directory_cookie_order() {
        let limits = StateLimits::default();
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            limits,
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        let client = authority.open_client();
        let filesystem_id = FilesystemId::from_u128(1);
        let parent = InodeId::from_u128(2);
        let child = InodeId::from_u128(3);
        let revision = RecordRevision::new(1).unwrap();
        let later_name = EntryName::new(b"a".to_vec(), limits).unwrap();
        let earlier_name = EntryName::new(b"z".to_vec(), limits).unwrap();
        {
            let mut state = client.state.lock().unwrap();
            let timestamp = UnixTimestamp::new(0, 0).unwrap();
            state.records.insert(
                RecordKey::Inode(filesystem_id, child),
                StateRecord::Inode(
                    InodeRecord::new(
                        child,
                        QidPath::new(7).unwrap(),
                        revision,
                        0o644,
                        PrincipalId::new(b"owner".to_vec(), limits).unwrap(),
                        GroupId::new(b"group".to_vec(), limits).unwrap(),
                        InodeTimes {
                            accessed: timestamp,
                            modified: timestamp,
                            changed: timestamp,
                            created: timestamp,
                        },
                        0,
                        2,
                        InodeGeneration::new(1).unwrap(),
                        InodeData::Fifo,
                    )
                    .unwrap(),
                ),
            );
            for (name, cookie) in [
                (later_name, DirectoryCookie::new(5)),
                (earlier_name, DirectoryCookie::new(3)),
            ] {
                state.records.insert(
                    RecordKey::DirectoryEntry(filesystem_id, parent, name.clone()),
                    StateRecord::DirectoryEntry(
                        DirectoryEntryRecord::new(parent, name, cookie, child, revision).unwrap(),
                    ),
                );
            }
        }
        let query = ReadQuery::Scan(RecordScan::DirectoryEntries {
            parent_inode_id: parent,
            after: DirectoryCookie::START,
            bounds: ScanBounds::new(1, 1024, limits).unwrap(),
        });
        let batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![
                ReadQuery::Inode(parent),
                query,
                ReadQuery::DirectoryPage {
                    parent_inode_id: parent,
                    after: DirectoryCookie::START,
                    bounds: ScanBounds::new(1, 1024, limits).unwrap(),
                },
            ],
            limits,
        )
        .unwrap();
        let ReadOutcome::Snapshot(snapshot) =
            w9pt_fs_storage::testing::block_on(client.read_request(batch)).unwrap()
        else {
            panic!("expected snapshot");
        };
        assert!(matches!(
            &snapshot.results()[0],
            ReadResult::Point { record: None, .. }
        ));
        let ReadResult::Scan(page) = &snapshot.results()[1] else {
            panic!("expected scan page");
        };
        let StateRecord::DirectoryEntry(first) = &page.records()[0].1 else {
            panic!("expected directory entry");
        };
        assert_eq!(first.cookie(), DirectoryCookie::new(3));
        assert_eq!(
            page.resume(),
            Some(&ScanResume::DirectoryEntry(DirectoryCookie::new(3)))
        );
        let ReadResult::DirectoryPage(page) = &snapshot.results()[2] else {
            panic!("expected semantic directory page");
        };
        assert_eq!(page.entries().len(), 1);
        assert_eq!(page.entries()[0].entry().cookie(), DirectoryCookie::new(3));
        assert_eq!(page.entries()[0].child_kind(), crate::InodeKind::Fifo);
        assert_eq!(page.entries()[0].child_qid_path(), QidPath::new(7).unwrap());
        assert_eq!(page.resume(), Some(DirectoryCookie::new(3)));

        let future = ReadBatch::new(
            filesystem_id,
            ReadConsistency::AtLeast(StateRevision::new(2).unwrap()),
            vec![],
            limits,
        )
        .unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(client.read_request(future)).unwrap(),
            ReadOutcome::RevisionUnavailable { .. }
        ));
    }

    #[test]
    fn commits_publish_records_ledger_revision_and_event_atomically() {
        let limits = StateLimits::default();
        let clock = ManualLeaseClock::new(LeaseDeadline::new(0));
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            limits,
            clock.clone(),
        );
        let client = authority.open_client();
        let observer = authority.open_client();
        let filesystem_id = FilesystemId::from_u128(1);
        let root_id = InodeId::from_u128(2);
        let fence = WriterFence::new(
            WriterScopeId::from_u128(3),
            WriterIncarnationId::from_u128(4),
            LeaseId::from_u128(5),
            FencingToken::new(1).unwrap(),
        );
        let lease = WriterLeaseRecord::new(
            filesystem_id,
            fence.scope,
            fence.holder,
            fence.lease_id,
            LeaseDeadline::new(5),
            fence.fencing_token,
            RecordRevision::new(1).unwrap(),
        );
        client.state.lock().unwrap().records.insert(
            RecordKey::WriterLease(filesystem_id, fence.scope),
            StateRecord::WriterLease(lease),
        );

        let timestamp = UnixTimestamp::new(0, 0).unwrap();
        let times = InodeTimes {
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
            created: timestamp,
        };
        let root = InodeRecord::new(
            root_id,
            crate::QidPath::new(1).unwrap(),
            RecordRevision::new(1).unwrap(),
            0o755,
            PrincipalId::new(b"root".to_vec(), limits).unwrap(),
            GroupId::new(b"root".to_vec(), limits).unwrap(),
            times,
            0,
            1,
            InodeGeneration::new(1).unwrap(),
            InodeData::Directory {
                generation: DirectoryGeneration::new(1).unwrap(),
                parent_inode_id: root_id,
            },
        )
        .unwrap();
        let filesystem = FilesystemRecord::new(
            filesystem_id,
            StateRevision::new(1).unwrap(),
            RecordRevision::new(1).unwrap(),
            root_id,
            crate::QidPath::new(2).unwrap(),
            DirectoryCookie::new(1),
            1,
        )
        .unwrap();
        let mutation = MutationContext::new(
            w9pt_fs_storage::MutationId::from_u128(6),
            crate::RequestFingerprint::blake3(b"bootstrap"),
            ClientIncarnationId::from_u128(7),
            MutationRetention::new(100),
        );
        let terminal = MutationResult::new(
            MutationResultKind::new(1).unwrap(),
            ResultFormatVersion::new(1).unwrap(),
            b"created".to_vec(),
            limits,
        )
        .unwrap();
        let request = CommitRequest::new(
            filesystem_id,
            mutation,
            fence,
            vec![
                Precondition::RecordAbsent(RecordKey::Filesystem(filesystem_id)),
                Precondition::RecordAbsent(RecordKey::Inode(filesystem_id, root_id)),
            ],
            vec![
                StateChange::Insert {
                    key: RecordKey::Filesystem(filesystem_id),
                    record: StateRecord::Filesystem(filesystem),
                },
                StateChange::Insert {
                    key: RecordKey::Inode(filesystem_id, root_id),
                    record: StateRecord::Inode(root),
                },
            ],
            terminal,
            limits,
        )
        .unwrap();
        authority
            .inject_commit_failure(CommitFailureTiming::BeforePublication)
            .unwrap();
        assert!(
            w9pt_fs_storage::testing::block_on(client.commit_request(request.clone())).is_err()
        );
        assert_eq!(observer.current_revision().unwrap().get(), 1);
        assert!(
            !observer
                .state
                .lock()
                .unwrap()
                .records
                .contains_key(&RecordKey::Filesystem(filesystem_id))
        );

        authority
            .inject_commit_failure(CommitFailureTiming::AfterPublication)
            .unwrap();
        let committed =
            w9pt_fs_storage::testing::block_on(client.commit_request(request.clone())).unwrap();
        assert!(matches!(committed, CommitOutcome::Ambiguous(_)));
        {
            let state = observer.state.lock().unwrap();
            assert_eq!(state.revision.get(), 2);
            assert!(
                state
                    .records
                    .contains_key(&RecordKey::Filesystem(filesystem_id))
            );
            assert!(
                state
                    .records
                    .contains_key(&RecordKey::Mutation(filesystem_id, mutation.mutation_id,))
            );
            assert_eq!(state.changes.len(), 1);
            assert_eq!(state.changes[0].keys().len(), 3);
            assert_eq!(
                state.changes[0].origin(),
                ChangeOrigin::Mutation(mutation.mutation_id)
            );
        }

        clock.advance_to(LeaseDeadline::new(5)).unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(client.commit_request(request)).unwrap(),
            CommitOutcome::AlreadyCommitted(_)
        ));
        assert_eq!(observer.current_revision().unwrap().get(), 2);
        let phases: Vec<_> = authority
            .trace()
            .unwrap()
            .into_iter()
            .map(|event| event.phase)
            .collect();
        assert_eq!(
            &phases[..5],
            &[
                MemoryTracePhase::Started,
                MemoryTracePhase::FailedBeforePublication,
                MemoryTracePhase::Started,
                MemoryTracePhase::Published,
                MemoryTracePhase::AmbiguousAfterPublication,
            ]
        );
    }

    #[test]
    fn memory_leases_replay_expire_take_over_release_and_keep_tokens_monotonic() {
        let limits = StateLimits::default();
        let clock = ManualLeaseClock::new(LeaseDeadline::new(0));
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            limits,
            clock.clone(),
        );
        let first_client = authority.open_client();
        let second_client = authority.open_client();
        let filesystem_id = FilesystemId::from_u128(1);
        let scope = WriterScopeId::from_u128(2);
        let first_request = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(3),
            scope,
            WriterIncarnationId::from_u128(4),
            LeaseId::from_u128(5),
            LeaseDuration::new(5).unwrap(),
            limits,
        )
        .unwrap();
        let AcquireLeaseOutcome::Granted(first) =
            w9pt_fs_storage::testing::block_on(first_client.acquire_lease_request(first_request))
                .unwrap()
        else {
            panic!("first lease must be granted");
        };
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(first_client.acquire_lease_request(first_request))
                .unwrap(),
            AcquireLeaseOutcome::AlreadyApplied(_)
        ));

        let busy_request = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(6),
            scope,
            WriterIncarnationId::from_u128(7),
            LeaseId::from_u128(8),
            LeaseDuration::new(5).unwrap(),
            limits,
        )
        .unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(second_client.acquire_lease_request(busy_request))
                .unwrap(),
            AcquireLeaseOutcome::Rejected(LeaseRejection::Busy(_))
        ));
        clock.advance_to(LeaseDeadline::new(5)).unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(second_client.acquire_lease_request(busy_request))
                .unwrap(),
            AcquireLeaseOutcome::Rejected(LeaseRejection::Busy(_))
        ));

        let takeover_request = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(9),
            scope,
            busy_request.holder(),
            busy_request.lease_id(),
            LeaseDuration::new(5).unwrap(),
            limits,
        )
        .unwrap();
        let AcquireLeaseOutcome::Granted(second) = w9pt_fs_storage::testing::block_on(
            second_client.acquire_lease_request(takeover_request),
        )
        .unwrap() else {
            panic!("expired lease must permit takeover");
        };
        assert!(second.fence.fencing_token > first.fence.fencing_token);

        let release =
            ReleaseWriterLease::new(filesystem_id, LeaseOperationId::from_u128(10), second.fence);
        assert_eq!(
            w9pt_fs_storage::testing::block_on(second_client.release_lease_request(release))
                .unwrap(),
            ReleaseLeaseOutcome::Released
        );
        assert_eq!(
            w9pt_fs_storage::testing::block_on(second_client.release_lease_request(release))
                .unwrap(),
            ReleaseLeaseOutcome::AlreadyApplied
        );
        let third_request = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(11),
            scope,
            WriterIncarnationId::from_u128(12),
            LeaseId::from_u128(13),
            LeaseDuration::new(1).unwrap(),
            limits,
        )
        .unwrap();
        let AcquireLeaseOutcome::Granted(third) =
            w9pt_fs_storage::testing::block_on(first_client.acquire_lease_request(third_request))
                .unwrap()
        else {
            panic!("released scope must be grantable");
        };
        assert!(third.fence.fencing_token > second.fence.fencing_token);
    }

    #[test]
    fn compacted_change_history_returns_an_explicit_revision_gap() {
        let limits = StateLimits::new(crate::StateLimitValues {
            max_change_history_commits: 1,
            ..crate::StateLimitValues::default()
        })
        .unwrap();
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            limits,
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        let client = authority.open_client();
        let filesystem_id = FilesystemId::from_u128(1);
        let acquire = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(2),
            WriterScopeId::from_u128(3),
            WriterIncarnationId::from_u128(4),
            LeaseId::from_u128(5),
            LeaseDuration::new(10).unwrap(),
            limits,
        )
        .unwrap();
        let AcquireLeaseOutcome::Granted(grant) =
            w9pt_fs_storage::testing::block_on(client.acquire_lease_request(acquire)).unwrap()
        else {
            panic!("lease must be granted");
        };
        let renew = RenewWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(6),
            grant.fence,
            LeaseDuration::new(10).unwrap(),
            limits,
        )
        .unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(client.renew_lease_request(renew)).unwrap(),
            RenewLeaseOutcome::Renewed(_)
        ));
        let poll = ChangePoll::new(
            filesystem_id,
            crate::ChangeCursor::after(StateRevision::new(1).unwrap()),
            1,
            1,
            limits,
        )
        .unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(client.poll_changes_request(poll)).unwrap(),
            ChangePollOutcome::RevisionCompacted {
                oldest_available,
                ..
            } if oldest_available.get() == 2
        ));
    }

    #[test]
    fn lease_entrypoints_revalidate_the_authority_duration_limit() {
        let strict_limits = StateLimits::new(crate::StateLimitValues {
            max_lease_duration_ticks: 1,
            ..crate::StateLimitValues::default()
        })
        .unwrap();
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            strict_limits,
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        let request = AcquireWriterLease::new(
            FilesystemId::from_u128(1),
            LeaseOperationId::from_u128(2),
            WriterScopeId::from_u128(3),
            WriterIncarnationId::from_u128(4),
            LeaseId::from_u128(5),
            LeaseDuration::new(2).unwrap(),
            StateLimits::default(),
        )
        .unwrap();
        assert_eq!(
            w9pt_fs_storage::testing::block_on(
                authority.open_client().acquire_lease_request(request)
            )
            .unwrap(),
            AcquireLeaseOutcome::Rejected(LeaseRejection::InvalidDuration)
        );
    }

    #[test]
    fn change_polling_is_filesystem_scoped_across_global_revisions() {
        let limits = StateLimits::default();
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            limits,
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        let client = authority.open_client();
        let first_filesystem = FilesystemId::from_u128(1);
        let second_filesystem = FilesystemId::from_u128(2);
        for (filesystem_id, operation) in [(first_filesystem, 10), (second_filesystem, 20)] {
            let request = AcquireWriterLease::new(
                filesystem_id,
                LeaseOperationId::from_u128(operation),
                WriterScopeId::from_u128(3),
                WriterIncarnationId::from_u128(4),
                LeaseId::from_u128(5),
                LeaseDuration::new(10).unwrap(),
                limits,
            )
            .unwrap();
            assert!(matches!(
                w9pt_fs_storage::testing::block_on(client.acquire_lease_request(request)).unwrap(),
                AcquireLeaseOutcome::Granted(_)
            ));
        }
        let poll = ChangePoll::new(
            first_filesystem,
            crate::ChangeCursor::after(StateRevision::new(1).unwrap()),
            10,
            10,
            limits,
        )
        .unwrap();
        let ChangePollOutcome::Changes(batch) =
            w9pt_fs_storage::testing::block_on(client.poll_changes_request(poll)).unwrap()
        else {
            panic!("expected scoped change batch");
        };
        assert_eq!(batch.events().len(), 1);
        assert_eq!(batch.events()[0].filesystem_id(), first_filesystem);
        assert_eq!(batch.next().revision().get(), 3);
    }

    #[test]
    fn lock_conflict_detection_is_scoped_by_filesystem() {
        let first_filesystem = FilesystemId::from_u128(1);
        let second_filesystem = FilesystemId::from_u128(2);
        let inode_id = InodeId::from_u128(3);
        let owner = LockOwner::new(
            ClientIncarnationId::from_u128(4),
            crate::OpenId::from_u128(5),
        );
        let revision = RecordRevision::new(1).unwrap();
        let existing = LockRecord::new(
            LockId::from_u128(6),
            inode_id,
            LockRange::finite(0, 10).unwrap(),
            LockKind::Exclusive,
            owner,
            LockGeneration::new(1).unwrap(),
            revision,
        );
        let requested = LockRecord::new(
            LockId::from_u128(7),
            inode_id,
            LockRange::finite(0, 10).unwrap(),
            LockKind::Exclusive,
            LockOwner::new(
                ClientIncarnationId::from_u128(8),
                crate::OpenId::from_u128(9),
            ),
            LockGeneration::new(1).unwrap(),
            revision,
        );
        let mut records = BTreeMap::from([(
            RecordKey::Lock(first_filesystem, inode_id, existing.lock_id()),
            StateRecord::Lock(existing),
        )]);
        let change = StateChange::Insert {
            key: RecordKey::Lock(second_filesystem, inode_id, requested.lock_id()),
            record: StateRecord::Lock(requested),
        };
        assert!(
            apply_change(
                &mut records,
                second_filesystem,
                MutationContext::new(
                    w9pt_fs_storage::MutationId::from_u128(10),
                    crate::RequestFingerprint::blake3(b"lock"),
                    ClientIncarnationId::from_u128(11),
                    crate::MutationRetention::new(12),
                ),
                &change,
                revision,
                StateLimits::default(),
            )
            .is_ok()
        );
    }

    #[test]
    fn tracing_covers_reads_leases_and_change_polls_in_authority_order() {
        let limits = StateLimits::default();
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            limits,
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        let client = authority.open_client();
        let filesystem_id = FilesystemId::from_u128(1);
        let acquire = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(2),
            WriterScopeId::from_u128(3),
            WriterIncarnationId::from_u128(4),
            LeaseId::from_u128(5),
            LeaseDuration::new(10).unwrap(),
            limits,
        )
        .unwrap();
        let AcquireLeaseOutcome::Granted(grant) =
            w9pt_fs_storage::testing::block_on(client.acquire_lease_request(acquire)).unwrap()
        else {
            panic!("lease must be granted");
        };
        let renew = RenewWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(6),
            grant.fence,
            LeaseDuration::new(10).unwrap(),
            limits,
        )
        .unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(client.renew_lease_request(renew)).unwrap(),
            RenewLeaseOutcome::Renewed(_)
        ));
        let read = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![],
            limits,
        )
        .unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(client.read_request(read)).unwrap(),
            ReadOutcome::Snapshot(_)
        ));
        let poll = ChangePoll::new(
            filesystem_id,
            crate::ChangeCursor::after(StateRevision::new(1).unwrap()),
            10,
            10,
            limits,
        )
        .unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(client.poll_changes_request(poll)).unwrap(),
            ChangePollOutcome::Changes(_)
        ));
        let release =
            ReleaseWriterLease::new(filesystem_id, LeaseOperationId::from_u128(7), grant.fence);
        assert_eq!(
            w9pt_fs_storage::testing::block_on(client.release_lease_request(release)).unwrap(),
            ReleaseLeaseOutcome::Released
        );
        let started: Vec<_> = authority
            .trace()
            .unwrap()
            .into_iter()
            .filter(|event| event.phase == MemoryTracePhase::Started)
            .map(|event| event.operation)
            .collect();
        assert_eq!(
            started,
            vec![
                StateStoreOperation::AcquireLease,
                StateStoreOperation::RenewLease,
                StateStoreOperation::Read,
                StateStoreOperation::PollChanges,
                StateStoreOperation::ReleaseLease,
            ]
        );
    }

    #[test]
    fn staged_xattr_bytes_count_toward_publication_transaction_limit() {
        let limits = StateLimits::new(crate::StateLimitValues {
            max_xattr_value_bytes: 400,
            max_mutation_result_bytes: 500,
            max_transaction_bytes: 1_000,
            ..crate::StateLimitValues::default()
        })
        .unwrap();
        let filesystem_id = FilesystemId::from_u128(1);
        let inode_id = InodeId::from_u128(2);
        let staging_id = crate::XattrStagingId::from_u128(3);
        let name = crate::XattrName::new(b"x".to_vec(), limits).unwrap();
        let staging = crate::XattrStagingRecord::new(
            staging_id,
            inode_id,
            name.clone(),
            400,
            crate::XattrValue::new(vec![0; 400], limits).unwrap(),
            RecordRevision::new(1).unwrap(),
            limits,
        )
        .unwrap();
        let records = BTreeMap::from([(
            RecordKey::XattrStaging(filesystem_id, staging_id),
            StateRecord::XattrStaging(staging),
        )]);
        let request = CommitRequest::new(
            filesystem_id,
            MutationContext::new(
                w9pt_fs_storage::MutationId::from_u128(4),
                crate::RequestFingerprint::blake3(b"publish-xattr"),
                ClientIncarnationId::from_u128(5),
                MutationRetention::new(6),
            ),
            WriterFence::new(
                WriterScopeId::from_u128(7),
                WriterIncarnationId::from_u128(8),
                LeaseId::from_u128(9),
                FencingToken::new(1).unwrap(),
            ),
            vec![],
            vec![StateChange::PublishXattrStaging(
                crate::PublishXattrStaging {
                    staging_id,
                    inode_id,
                    name,
                },
            )],
            MutationResult::new(
                MutationResultKind::new(1).unwrap(),
                ResultFormatVersion::new(1).unwrap(),
                vec![0; 300],
                limits,
            )
            .unwrap(),
            limits,
        )
        .unwrap();
        assert!(matches!(
            validate_xattr_transitions(&records, &request, limits),
            Err(MalformedCommit::Limit(crate::StateLimitError {
                kind: crate::StateLimitKind::TransactionBytes,
                ..
            }))
        ));
    }

    #[test]
    fn looser_change_poll_is_a_semantic_malformed_request() {
        let strict = StateLimits::new(crate::StateLimitValues {
            max_change_history_commits: 1,
            ..crate::StateLimitValues::default()
        })
        .unwrap();
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            strict,
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        let request = ChangePoll::new(
            FilesystemId::from_u128(1),
            crate::ChangeCursor::after(StateRevision::new(1).unwrap()),
            2,
            1,
            StateLimits::default(),
        )
        .unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(
                authority.open_client().poll_changes_request(request)
            )
            .unwrap(),
            ChangePollOutcome::MalformedRequest(crate::InvalidChangeRequest::Limit(
                crate::StateLimitError {
                    kind: crate::StateLimitKind::ChangeHistory,
                    ..
                }
            ))
        ));
    }

    #[test]
    fn future_change_cursor_is_a_semantic_revision_outcome() {
        let limits = StateLimits::default();
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            limits,
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        let request = ChangePoll::new(
            FilesystemId::from_u128(1),
            crate::ChangeCursor::after(StateRevision::new(2).unwrap()),
            1,
            1,
            limits,
        )
        .unwrap();
        assert!(matches!(
            w9pt_fs_storage::testing::block_on(
                authority.open_client().poll_changes_request(request)
            )
            .unwrap(),
            ChangePollOutcome::RevisionUnavailable {
                requested,
                current,
            } if requested.get() == 2 && current.get() == 1
        ));
    }
}
