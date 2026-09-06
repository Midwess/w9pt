//! Ledger-first serializable filesystem-state commits and exact recovery.

use std::collections::{BTreeMap, BTreeSet};

use sea_orm::DbErr;
use w9pt_fs_state::testing::CommitFailureTiming;
use w9pt_fs_state::{
    AdapterFailureKind, AmbiguousCommit, CommitConflict, CommitConflictKind, CommitOutcome,
    CommitRequest, CommittedMutation, FenceValidation, FilesystemId, InodeData, InodeId,
    InodeRecord, LockId, MalformedCommit, MutationRecord, MutationReplay, OpenId, Precondition,
    RecordKey, RecordRevision, RecordValidationError, StateChange, StateRecord, StateRevision,
    StateStoreAdapterError, StateStoreOperation, XattrRecord,
    validate_publish_content_with_metadata,
};
use w9pt_fs_storage::StorageMethod;

use crate::{
    LeaseClockSource, PostgresStateConfig, PostgresStateError,
    clock::capture_lease_deadline,
    config::schema_limits,
    database::{
        PostgresConnection, PostgresTransaction, classify_database_error, query, query_scalar,
    },
    key_codec::{SqlRecordKey, canonical_affected_keys},
    lease::validate_commit_writer_fence,
    numeric::{decode_u64, encode_u64},
    read::fetch_record,
    record_write::{
        RowPresence, delete_record, insert_mutation_record, insert_record, lock_existing_record,
        replace_record,
    },
    sqlstate::{ConstraintAction, SqlFailureClass, SqlOperationPhase},
    transaction::{TransactionAccess, begin_transaction, commit_read_transaction},
};

const MUTATION_ORIGIN_TAG: i16 = 1;

#[derive(Debug)]
enum CommitAttemptError {
    State(PostgresStateError),
    Sql {
        phase: SqlOperationPhase,
        source: DbErr,
    },
}

impl From<PostgresStateError> for CommitAttemptError {
    fn from(error: PostgresStateError) -> Self {
        Self::State(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetryAction {
    RetryIdentical,
    ResolveLedger,
    AmbiguousCommit,
}

enum AmbiguousRecovery {
    Resolved(CommitOutcome),
    Absent,
    Exhausted,
}

/// Executes one ledger-first serializable metadata commit.
pub(crate) async fn commit_request(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    clock: LeaseClockSource,
    injection: Option<CommitFailureTiming>,
    request: CommitRequest,
) -> Result<CommitOutcome, PostgresStateError> {
    if let Some(outcome) = probe_ledger(pool, config, &request).await? {
        return Ok(outcome);
    }
    if let Err(error) = request.validate_preflight(config.limits()) {
        return Ok(CommitOutcome::MalformedRequest(error));
    }
    if let Err(error) = canonical_affected_keys(request.filesystem_id(), request.changes()) {
        return Ok(CommitOutcome::MalformedRequest(error));
    }

    let mut definitive_retries = 0u32;
    let mut recovery_attempts = 0u32;
    loop {
        match execute_once(pool, config, clock, injection, &request).await {
            Ok(outcome) => return Ok(outcome),
            Err(CommitAttemptError::State(error)) => {
                if state_retry_action(&error).is_some()
                    && definitive_retries < config.definitive_abort_retries()
                {
                    definitive_retries += 1;
                    continue;
                }
                return Err(error);
            }
            Err(CommitAttemptError::Sql { phase, source }) => match retry_action(phase, &source) {
                Some(RetryAction::RetryIdentical | RetryAction::ResolveLedger)
                    if definitive_retries < config.definitive_abort_retries() =>
                {
                    definitive_retries += 1;
                }
                Some(RetryAction::AmbiguousCommit) => {
                    match recover_ambiguous_commit(pool, config, &request, &mut recovery_attempts)
                        .await
                    {
                        AmbiguousRecovery::Resolved(outcome) => return Ok(outcome),
                        AmbiguousRecovery::Absent => {}
                        AmbiguousRecovery::Exhausted => {
                            return Ok(CommitOutcome::Ambiguous(AmbiguousCommit {
                                mutation: request.mutation(),
                            }));
                        }
                    }
                }
                _ => {
                    return Err(PostgresStateError::from_database(
                        StateStoreOperation::Commit,
                        phase,
                        source,
                    ));
                }
            },
        }
    }
}

async fn probe_ledger(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    request: &CommitRequest,
) -> Result<Option<CommitOutcome>, PostgresStateError> {
    let mut transaction = begin_transaction(
        pool,
        config,
        TransactionAccess::ReadOnly,
        StateStoreOperation::Commit,
    )
    .await?;
    let key = mutation_key(request);
    let record = fetch_record(
        &mut transaction,
        &key,
        schema_limits(),
        StateStoreOperation::Commit,
    )
    .await?;
    commit_read_transaction(transaction, StateStoreOperation::Commit).await?;
    record
        .map(|record| classify_ledger_record(request, record))
        .transpose()
}

async fn recover_ambiguous_commit(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    request: &CommitRequest,
    attempts: &mut u32,
) -> AmbiguousRecovery {
    while *attempts < config.ambiguous_commit_recovery_attempts() {
        *attempts += 1;
        match probe_ledger(pool, config, request).await {
            Ok(Some(outcome)) => return AmbiguousRecovery::Resolved(outcome),
            Ok(None) => return AmbiguousRecovery::Absent,
            Err(_) => {}
        }
    }
    AmbiguousRecovery::Exhausted
}

fn classify_ledger_record(
    request: &CommitRequest,
    record: StateRecord,
) -> Result<CommitOutcome, PostgresStateError> {
    let StateRecord::Mutation(record) = record else {
        return Err(corruption("mutation key decoded as another record family"));
    };
    Ok(match request.mutation().classify_record(&record) {
        MutationReplay::Exact(committed) => CommitOutcome::AlreadyCommitted(committed),
        MutationReplay::Mismatch(mismatch) => CommitOutcome::MutationMismatch(mismatch),
        MutationReplay::Absent => {
            return Err(corruption("present mutation record classified as absent"));
        }
    })
}

async fn execute_once(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    clock: LeaseClockSource,
    injection: Option<CommitFailureTiming>,
    request: &CommitRequest,
) -> Result<CommitOutcome, CommitAttemptError> {
    let mut transaction = begin_transaction(
        pool,
        config,
        TransactionAccess::ReadWrite,
        StateStoreOperation::Commit,
    )
    .await
    .map_err(CommitAttemptError::State)?;

    if let Some(record) = fetch_record(
        &mut transaction,
        &mutation_key(request),
        schema_limits(),
        StateStoreOperation::Commit,
    )
    .await
    .map_err(CommitAttemptError::State)?
    {
        let outcome = classify_ledger_record(request, record).map_err(CommitAttemptError::State)?;
        rollback(transaction).await?;
        return Ok(outcome);
    }

    let current_revision = lock_authority_head(&mut transaction, request.filesystem_id()).await?;
    lock_writer_fence_row(&mut transaction, request.filesystem_id(), request.fence()).await?;
    let now = capture_lease_deadline(clock, &mut transaction, StateStoreOperation::Commit)
        .await
        .map_err(CommitAttemptError::State)?;
    match validate_commit_writer_fence(
        &mut transaction,
        request.filesystem_id(),
        request.fence(),
        now,
        config.limits(),
    )
    .await
    .map_err(CommitAttemptError::State)?
    {
        FenceValidation::Current => {}
        FenceValidation::Stale => {
            rollback(transaction).await?;
            return Ok(CommitOutcome::StaleFence);
        }
        FenceValidation::Expired => {
            rollback(transaction).await?;
            return Ok(CommitOutcome::ExpiredLease);
        }
    }

    let keys = lock_keys(request);
    let mut records = BTreeMap::new();
    for key in &keys {
        lock_existing_record(&mut transaction, key)
            .await
            .map_err(CommitAttemptError::State)?;
        let record = fetch_record(
            &mut transaction,
            key,
            config.limits(),
            StateStoreOperation::Commit,
        )
        .await
        .map_err(CommitAttemptError::State)?;
        records.insert(key.clone(), record);
    }

    if let Some(conflict) = evaluate_preconditions(&mut transaction, request, &records).await? {
        rollback(transaction).await?;
        return Ok(CommitOutcome::Conflict(conflict));
    }
    if let Err(error) = validate_transitions(request, &records, config.limits()) {
        rollback(transaction).await?;
        return Ok(CommitOutcome::MalformedRequest(error));
    }
    if let Some(conflict) = validate_lock_conflicts(&mut transaction, request).await? {
        rollback(transaction).await?;
        return Ok(CommitOutcome::Conflict(CommitConflict {
            precondition_index: request.preconditions().len(),
            kind: conflict,
        }));
    }

    let next_revision = current_revision.checked_next().map_err(|_| {
        CommitAttemptError::State(corruption("authority revision exhausted its u64 range"))
    })?;
    for change in request.changes() {
        if let Some(outcome) = apply_change(
            &mut transaction,
            request,
            change,
            &records,
            next_revision,
            config.limits(),
        )
        .await?
        {
            rollback(transaction).await?;
            return Ok(outcome);
        }
    }

    if let Some(error) =
        validate_targeted_invariants(&mut transaction, request, &records, config.limits()).await?
    {
        rollback(transaction).await?;
        return Ok(CommitOutcome::MalformedRequest(error));
    }

    if matches!(injection, Some(CommitFailureTiming::BeforePublication)) {
        rollback(transaction).await?;
        return Err(CommitAttemptError::State(PostgresStateError::new(
            StateStoreOperation::Commit,
            AdapterFailureKind::Unavailable,
            "injected failure before authoritative publication",
        )));
    }

    let mutation = request.mutation();
    let record_revision = RecordRevision::new(next_revision.get())
        .map_err(|_| CommitAttemptError::State(corruption("invalid allocated record revision")))?;
    let mutation_record = MutationRecord::new(
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
    match insert_mutation_record(
        &mut transaction,
        &mutation_key(request),
        &mutation_record,
        next_revision,
    )
    .await
    .map_err(CommitAttemptError::State)?
    {
        RowPresence::Present => {}
        RowPresence::Absent => {
            return Err(CommitAttemptError::State(internal(
                "mutation insert affected no row",
            )));
        }
    }

    let changed_keys = changed_keys(request)?;
    publish_change(
        &mut transaction,
        request,
        next_revision,
        &changed_keys,
        config.limits().max_change_history_commits(),
    )
    .await?;

    transaction
        .commit()
        .await
        .map_err(|source| CommitAttemptError::Sql {
            phase: SqlOperationPhase::Commit,
            source,
        })?;
    if matches!(injection, Some(CommitFailureTiming::AfterPublication)) {
        return Ok(CommitOutcome::Ambiguous(AmbiguousCommit {
            mutation: request.mutation(),
        }));
    }
    Ok(CommitOutcome::Committed(CommittedMutation {
        revision: next_revision,
        result: request.terminal_result().clone(),
    }))
}

async fn lock_authority_head(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
) -> Result<StateRevision, CommitAttemptError> {
    query(
        r#"INSERT INTO "public"."w9pt_fs_state_authority_heads"
           ("filesystem_id", "current_revision", "oldest_retained_revision")
           VALUES ($1, 1, 1)
           ON CONFLICT ("filesystem_id") DO NOTHING"#,
    )
    .bind(filesystem_id.as_bytes().to_vec())
    .execute(transaction)
    .await
    .map_err(statement_sql)?;
    let revision: String = query_scalar(
        r#"SELECT "current_revision"::text
           FROM "public"."w9pt_fs_state_authority_heads"
           WHERE "filesystem_id" = $1
           FOR UPDATE"#,
    )
    .bind(filesystem_id.as_bytes().to_vec())
    .fetch_one(transaction)
    .await
    .map_err(statement_sql)?;
    decode_revision("authority_heads.current_revision", &revision)
        .map_err(CommitAttemptError::State)
}

async fn lock_writer_fence_row(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
    fence: w9pt_fs_state::WriterFence,
) -> Result<(), CommitAttemptError> {
    let _: Option<i32> = query_scalar(
        r#"SELECT 1 FROM "public"."w9pt_fs_state_writer_fences"
           WHERE "filesystem_id" = $1 AND "writer_scope_id" = $2
           FOR UPDATE"#,
    )
    .bind(filesystem_id.as_bytes().to_vec())
    .bind(fence.scope.as_bytes().to_vec())
    .fetch_optional(transaction)
    .await
    .map_err(statement_sql)?;
    Ok(())
}

fn lock_keys(request: &CommitRequest) -> Vec<RecordKey> {
    let filesystem_id = request.filesystem_id();
    let mut keys = BTreeSet::new();
    for change in request.changes() {
        keys.extend(change.affected_keys(filesystem_id));
        match change {
            StateChange::Insert {
                record: StateRecord::DirectoryEntry(entry),
                ..
            }
            | StateChange::Replace {
                record: StateRecord::DirectoryEntry(entry),
                ..
            } => {
                keys.insert(RecordKey::Filesystem(filesystem_id));
                keys.insert(RecordKey::Inode(filesystem_id, entry.parent_inode_id()));
                keys.insert(RecordKey::Inode(filesystem_id, entry.child_inode_id()));
            }
            StateChange::Delete(key @ RecordKey::DirectoryEntry(_, parent, _)) => {
                keys.insert(key.clone());
                keys.insert(RecordKey::Filesystem(filesystem_id));
                keys.insert(RecordKey::Inode(filesystem_id, *parent));
            }
            _ => {}
        }
    }
    for precondition in request.preconditions() {
        match precondition {
            Precondition::RecordAbsent(key) | Precondition::RecordRevision { key, .. } => {
                keys.insert(key.clone());
            }
            Precondition::InodeGeneration { inode_id, .. }
            | Precondition::DataGeneration { inode_id, .. }
            | Precondition::DirectoryGeneration { inode_id, .. }
            | Precondition::ContentBase { inode_id, .. }
            | Precondition::LinkCount { inode_id, .. } => {
                keys.insert(RecordKey::Inode(filesystem_id, *inode_id));
            }
            Precondition::OpenPinCount { .. } | Precondition::ExactFence(_) => {}
            Precondition::FilesystemPolicyGeneration { .. } => {
                keys.insert(RecordKey::Filesystem(filesystem_id));
            }
        }
    }
    keys.remove(&mutation_key(request));
    keys.remove(&RecordKey::WriterLease(
        filesystem_id,
        request.fence().scope,
    ));
    keys.into_iter().collect()
}

async fn evaluate_preconditions(
    transaction: &mut PostgresTransaction,
    request: &CommitRequest,
    records: &BTreeMap<RecordKey, Option<StateRecord>>,
) -> Result<Option<CommitConflict>, CommitAttemptError> {
    for (index, precondition) in request.preconditions().iter().enumerate() {
        let kind = match precondition {
            Precondition::RecordAbsent(key) if present(records, key).is_some() => {
                Some(CommitConflictKind::RecordPresent(key.clone()))
            }
            Precondition::RecordRevision { key, expected } => match present(records, key) {
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
                match inode(records, request.filesystem_id(), *inode_id) {
                    Some(inode) if inode.inode_generation() == *expected => None,
                    _ => Some(CommitConflictKind::InodeGeneration),
                }
            }
            Precondition::DataGeneration { inode_id, expected } => {
                match inode(records, request.filesystem_id(), *inode_id) {
                    Some(inode) if inode.data_generation() == *expected => None,
                    _ => Some(CommitConflictKind::DataGeneration),
                }
            }
            Precondition::DirectoryGeneration { inode_id, expected } => {
                match inode(records, request.filesystem_id(), *inode_id) {
                    Some(inode) if inode.directory_generation() == Some(*expected) => None,
                    _ => Some(CommitConflictKind::DirectoryGeneration),
                }
            }
            Precondition::ContentBase { inode_id, expected } => {
                match inode(records, request.filesystem_id(), *inode_id) {
                    Some(inode) if inode.content_base() == Some(*expected) => None,
                    _ => Some(CommitConflictKind::ContentBase),
                }
            }
            Precondition::LinkCount { inode_id, expected } => {
                match inode(records, request.filesystem_id(), *inode_id) {
                    Some(inode) if inode.link_count() == *expected => None,
                    _ => Some(CommitConflictKind::LinkCount),
                }
            }
            Precondition::OpenPinCount { inode_id, expected } => {
                let count: String = query_scalar(
                    r#"SELECT pg_catalog.count(*)::numeric(20, 0)::text
                       FROM "public"."w9pt_fs_state_open_pins"
                       WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
                )
                .bind(request.filesystem_id().as_bytes().to_vec())
                .bind(inode_id.as_bytes().to_vec())
                .fetch_one(transaction)
                .await
                .map_err(statement_sql)?;
                if decode_u64("open_pin_count", &count)
                    .map_err(|error| CommitAttemptError::State(corruption(error.to_string())))?
                    == *expected
                {
                    None
                } else {
                    Some(CommitConflictKind::OpenPinCount)
                }
            }
            Precondition::FilesystemPolicyGeneration { expected } => {
                match present(records, &RecordKey::Filesystem(request.filesystem_id())) {
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
        if let Some(kind) = kind {
            return Ok(Some(CommitConflict {
                precondition_index: index,
                kind,
            }));
        }
    }
    Ok(None)
}

fn validate_transitions(
    request: &CommitRequest,
    records: &BTreeMap<RecordKey, Option<StateRecord>>,
    limits: w9pt_fs_state::StateLimits,
) -> Result<(), MalformedCommit> {
    validate_directory_transition(request, records)?;
    validate_qid_path_transition(request, records)?;
    validate_monotonic_transition(request, records)?;
    validate_xattr_transition(request, records, limits)?;
    Ok(())
}

fn validate_qid_path_transition(
    request: &CommitRequest,
    records: &BTreeMap<RecordKey, Option<StateRecord>>,
) -> Result<(), MalformedCommit> {
    let filesystem_key = RecordKey::Filesystem(request.filesystem_id());
    let Some(StateRecord::Filesystem(filesystem)) = present(records, &filesystem_key) else {
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
                if let Some(StateRecord::Inode(current)) = present(records, key)
                    && current.qid_path() != replacement.qid_path()
                {
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

fn validate_directory_transition(
    request: &CommitRequest,
    records: &BTreeMap<RecordKey, Option<StateRecord>>,
) -> Result<(), MalformedCommit> {
    let filesystem_key = RecordKey::Filesystem(request.filesystem_id());
    let Some(StateRecord::Filesystem(filesystem)) = present(records, &filesystem_key) else {
        return Ok(());
    };
    let deleted: Vec<_> = request
        .changes()
        .iter()
        .filter_map(|change| match change {
            StateChange::Delete(key) => match present(records, key) {
                Some(StateRecord::DirectoryEntry(entry)) => Some(entry),
                _ => None,
            },
            _ => None,
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
                if let Some(StateRecord::DirectoryEntry(current)) = present(records, key)
                    && current.cookie() != replacement.cookie()
                {
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

fn validate_monotonic_transition(
    request: &CommitRequest,
    records: &BTreeMap<RecordKey, Option<StateRecord>>,
) -> Result<(), MalformedCommit> {
    if present(records, &RecordKey::Filesystem(request.filesystem_id())).is_none() {
        return Ok(());
    }
    let namespace_parents: BTreeSet<_> = request
        .changes()
        .iter()
        .filter_map(|change| match change {
            StateChange::Insert {
                record: StateRecord::DirectoryEntry(entry),
                ..
            }
            | StateChange::Replace {
                record: StateRecord::DirectoryEntry(entry),
                ..
            } => Some(entry.parent_inode_id()),
            StateChange::Delete(key) => match present(records, key) {
                Some(StateRecord::DirectoryEntry(entry)) => Some(entry.parent_inode_id()),
                _ => None,
            },
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
                match present(records, key) {
                    Some(StateRecord::Inode(current)) => matches!(
                        (current.directory_generation(), replacement.directory_generation()),
                        (Some(old), Some(new)) if old.checked_next().ok() == Some(new)
                    ),
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
        let Some(current) = present(records, key) else {
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
                    && current.inode_generation().checked_next().ok()
                        == Some(replacement.inode_generation())
                    && current.content() == replacement.content()
                    && current.content_file_id() == replacement.content_file_id()
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
    records: &BTreeMap<RecordKey, Option<StateRecord>>,
    request: &CommitRequest,
    current: &w9pt_fs_state::InodeRecord,
    replacement: &w9pt_fs_state::InodeRecord,
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
        match present(records, key) {
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

fn validate_xattr_transition(
    request: &CommitRequest,
    records: &BTreeMap<RecordKey, Option<StateRecord>>,
    limits: w9pt_fs_state::StateLimits,
) -> Result<(), MalformedCommit> {
    let mut authoritative_staged_bytes = 0usize;
    for change in request.changes() {
        if let StateChange::PublishXattrStaging(publication) = change {
            let key = RecordKey::XattrStaging(request.filesystem_id(), publication.staging_id);
            let Some(StateRecord::XattrStaging(staging)) = present(records, &key) else {
                return Err(MalformedCommit::InvalidXattrStaging);
            };
            if staging.inode_id() != publication.inode_id
                || staging.name() != &publication.name
                || !staging.is_complete()
            {
                return Err(MalformedCommit::InvalidXattrStaging);
            }
            authoritative_staged_bytes = authoritative_staged_bytes
                .checked_add(staging.bytes().as_bytes().len())
                .ok_or(MalformedCommit::Arithmetic)?;
        }
        let StateChange::Delete(key @ RecordKey::XattrStaging(_, _)) = change else {
            continue;
        };
        let Some(StateRecord::XattrStaging(staging)) = present(records, key) else {
            continue;
        };
        if request.changes().iter().any(|candidate| match candidate {
            StateChange::Insert {
                record: StateRecord::Xattr(xattr),
                ..
            }
            | StateChange::Replace {
                record: StateRecord::Xattr(xattr),
                ..
            } => xattr.inode_id() == staging.inode_id() && xattr.name() == staging.name(),
            _ => false,
        }) {
            return Err(MalformedCommit::XattrPublicationRequired);
        }
    }
    let estimated = estimate_transaction_bytes(request)
        .and_then(|bytes| bytes.checked_add(authoritative_staged_bytes))
        .ok_or(MalformedCommit::Arithmetic)?;
    if estimated > limits.max_transaction_bytes() {
        return Err(MalformedCommit::Limit(w9pt_fs_state::StateLimitError::new(
            w9pt_fs_state::StateLimitKind::TransactionBytes,
            u64::try_from(estimated).unwrap_or(u64::MAX),
            u64::try_from(limits.max_transaction_bytes()).unwrap_or(u64::MAX),
        )));
    }
    Ok(())
}

async fn validate_lock_conflicts(
    transaction: &mut PostgresTransaction,
    request: &CommitRequest,
) -> Result<Option<CommitConflictKind>, CommitAttemptError> {
    let mut final_locks = Vec::new();
    let mut removed = BTreeSet::new();
    for change in request.changes() {
        match change {
            StateChange::Insert {
                key,
                record: StateRecord::Lock(lock),
            } => final_locks.push((key.clone(), lock)),
            StateChange::Replace {
                key: RecordKey::Lock(_, inode_id, lock_id),
                record: StateRecord::Lock(lock),
            } => {
                removed.insert((*inode_id, *lock_id));
                final_locks.push((change.primary_key(request.filesystem_id()), lock));
            }
            StateChange::Delete(RecordKey::Lock(_, inode_id, lock_id)) => {
                removed.insert((*inode_id, *lock_id));
            }
            _ => {}
        }
    }
    final_locks.sort_by(|left, right| left.0.cmp(&right.0));

    let mut lowest_conflict = None;
    for (index, (_, lock)) in final_locks.iter().enumerate() {
        for (_, other) in final_locks.iter().skip(index + 1) {
            if lock.conflicts_with(other) {
                retain_lowest_lock(&mut lowest_conflict, lock.lock_id().min(other.lock_id()));
            }
        }

        let end = match lock.range().end() {
            w9pt_fs_state::LockRangeEnd::Exclusive(end) => Some(encode_u64(end)),
            w9pt_fs_state::LockRangeEnd::ThroughEof => None,
        };
        let mut after = None;
        loop {
            let existing: Option<Vec<u8>> = query_scalar(
                r#"SELECT "lock_id"
                   FROM "public"."w9pt_fs_state_locks"
                   WHERE "filesystem_id" = $1 AND "inode_id" = $2
                     AND ("owner_client_incarnation_id" <> $3 OR "owner_open_id" <> $4)
                     AND ($5::numeric IS NULL OR "range_start" < $5::numeric)
                     AND ("range_end" IS NULL OR "range_end" > $6::numeric)
                     AND ("kind" = 2 OR $7::smallint = 2)
                     AND ($8::bytea IS NULL OR "lock_id" > $8)
                   ORDER BY "lock_id"
                   LIMIT 1
                   FOR SHARE"#,
            )
            .bind(request.filesystem_id().as_bytes().to_vec())
            .bind(lock.inode_id().as_bytes().to_vec())
            .bind(lock.owner().client_incarnation().as_bytes().to_vec())
            .bind(lock.owner().open_id().as_bytes().to_vec())
            .bind(end.clone())
            .bind(encode_u64(lock.range().start()))
            .bind(match lock.kind() {
                w9pt_fs_state::LockKind::Shared => 1i16,
                w9pt_fs_state::LockKind::Exclusive => 2i16,
            })
            .bind(after.clone())
            .fetch_optional(transaction)
            .await
            .map_err(statement_sql)?;
            let Some(existing) = existing else {
                break;
            };
            let actual = existing.len();
            let bytes: [u8; 16] = existing.try_into().map_err(|_| {
                CommitAttemptError::State(corruption(format!(
                    "lock_id has {actual} bytes, expected 16"
                )))
            })?;
            let lock_id = LockId::new(bytes);
            if removed.contains(&(lock.inode_id(), lock_id)) {
                after = Some(bytes.to_vec());
                continue;
            }
            retain_lowest_lock(&mut lowest_conflict, lock_id);
            break;
        }
    }
    Ok(lowest_conflict.map(|existing| CommitConflictKind::LockConflict { existing }))
}

fn retain_lowest_lock(current: &mut Option<LockId>, candidate: LockId) {
    if current.is_none_or(|existing| candidate < existing) {
        *current = Some(candidate);
    }
}

async fn apply_change(
    transaction: &mut PostgresTransaction,
    request: &CommitRequest,
    change: &StateChange,
    records: &BTreeMap<RecordKey, Option<StateRecord>>,
    revision: StateRevision,
    limits: w9pt_fs_state::StateLimits,
) -> Result<Option<CommitOutcome>, CommitAttemptError> {
    let key = change.primary_key(request.filesystem_id());
    let presence = match change {
        StateChange::Insert { record, .. } => {
            if present(records, &key).is_some() {
                return Ok(Some(CommitOutcome::Conflict(CommitConflict {
                    precondition_index: request.preconditions().len(),
                    kind: CommitConflictKind::RecordPresent(key),
                })));
            }
            return apply_presence(
                insert_record(transaction, &key, record, revision)
                    .await
                    .map_err(CommitAttemptError::State)?,
                true,
                request,
                key,
            );
        }
        StateChange::Replace { record, .. } => {
            if let (Some(StateRecord::Inode(current)), StateRecord::Inode(replacement)) =
                (present(records, &key), record)
                && (current.content() != replacement.content()
                    || current.content_file_id() != replacement.content_file_id()
                    || current.content_context_id() != replacement.content_context_id())
            {
                return Ok(Some(CommitOutcome::MalformedRequest(
                    MalformedCommit::ContentReplacementRequiresPreparedPublication,
                )));
            }
            return apply_presence(
                replace_record(transaction, &key, record, revision)
                    .await
                    .map_err(CommitAttemptError::State)?,
                false,
                request,
                key,
            );
        }
        StateChange::Delete(_) => {
            return apply_presence(
                delete_record(transaction, &key)
                    .await
                    .map_err(CommitAttemptError::State)?,
                false,
                request,
                key,
            );
        }
        StateChange::AdvanceDirectoryCookie { count } => {
            let current = match present(records, &key) {
                Some(StateRecord::Filesystem(record)) => record.next_directory_cookie(),
                _ => return Ok(Some(missing_conflict(request, key))),
            };
            let Ok(next) = current.checked_advance(count.get()) else {
                return Ok(Some(malformed_arithmetic()));
            };
            update_numeric(
                transaction,
                r#"UPDATE "public"."w9pt_fs_state_filesystem_records"
                   SET "next_directory_cookie" = $2::numeric,
                       "state_revision" = $3::numeric, "record_revision" = $3::numeric
                   WHERE "filesystem_id" = $1"#,
                &[request.filesystem_id().as_bytes().to_vec()],
                next.get(),
                revision,
            )
            .await?
        }
        StateChange::AdvanceQidPath { count } => {
            let current = match present(records, &key) {
                Some(StateRecord::Filesystem(record)) => record.next_qid_path(),
                _ => return Ok(Some(missing_conflict(request, key))),
            };
            let Ok(next) = current.checked_advance(count.get()) else {
                return Ok(Some(malformed_arithmetic()));
            };
            update_numeric(
                transaction,
                r#"UPDATE "public"."w9pt_fs_state_filesystem_records"
                   SET "next_qid_path" = $2::numeric,
                       "state_revision" = $3::numeric, "record_revision" = $3::numeric
                   WHERE "filesystem_id" = $1"#,
                &[request.filesystem_id().as_bytes().to_vec()],
                next.get(),
                revision,
            )
            .await?
        }
        StateChange::BumpFilesystemPolicyGeneration => {
            let current = match present(records, &key) {
                Some(StateRecord::Filesystem(record)) => record.policy_generation(),
                _ => return Ok(Some(missing_conflict(request, key))),
            };
            let Some(next) = current.checked_add(1) else {
                return Ok(Some(malformed_arithmetic()));
            };
            update_numeric(
                transaction,
                r#"UPDATE "public"."w9pt_fs_state_filesystem_records"
                   SET "policy_generation" = $2::numeric,
                       "state_revision" = $3::numeric, "record_revision" = $3::numeric
                   WHERE "filesystem_id" = $1"#,
                &[request.filesystem_id().as_bytes().to_vec()],
                next,
                revision,
            )
            .await?
        }
        StateChange::BumpInodeGeneration(inode_id) => {
            let current = match inode(records, request.filesystem_id(), *inode_id) {
                Some(record) => match record.inode_generation().checked_next() {
                    Ok(generation) => generation,
                    Err(_) => return Ok(Some(malformed_arithmetic())),
                },
                None => return Ok(Some(missing_conflict(request, key))),
            };
            update_inode_numbers(
                transaction,
                request.filesystem_id(),
                *inode_id,
                Some(("inode_generation", current.get())),
                None,
                revision,
            )
            .await?
        }
        StateChange::BumpDirectoryGeneration(inode_id) => {
            let current = match inode(records, request.filesystem_id(), *inode_id) {
                Some(record) => record,
                None => return Ok(Some(missing_conflict(request, key))),
            };
            let Ok(inode_generation) = current.inode_generation().checked_next() else {
                return Ok(Some(malformed_arithmetic()));
            };
            let Some(directory_generation) = current.directory_generation() else {
                return Ok(Some(malformed_arithmetic()));
            };
            let Ok(directory_generation) = directory_generation.checked_next() else {
                return Ok(Some(malformed_arithmetic()));
            };
            let result = query(
                r#"UPDATE "public"."w9pt_fs_state_inodes"
                   SET "inode_generation" = $3::numeric,
                       "directory_generation" = $4::numeric,
                       "record_revision" = $5::numeric
                   WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
            )
            .bind(request.filesystem_id().as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .bind(encode_u64(inode_generation.get()))
            .bind(encode_u64(directory_generation.get()))
            .bind(encode_u64(revision.get()))
            .execute(transaction)
            .await
            .map_err(state_statement)?;
            result.rows_affected()
        }
        StateChange::AdjustLinkCount {
            inode_id,
            adjustment,
        } => {
            let current = match inode(records, request.filesystem_id(), *inode_id) {
                Some(record) => record,
                None => return Ok(Some(missing_conflict(request, key))),
            };
            let Some(count) = adjustment.apply(current.link_count()) else {
                return Ok(Some(malformed_arithmetic()));
            };
            let Ok(generation) = current.inode_generation().checked_next() else {
                return Ok(Some(malformed_arithmetic()));
            };
            let result = query(
                r#"UPDATE "public"."w9pt_fs_state_inodes"
                   SET "link_count" = $3::numeric, "inode_generation" = $4::numeric,
                       "record_revision" = $5::numeric
                   WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
            )
            .bind(request.filesystem_id().as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .bind(encode_u64(count))
            .bind(encode_u64(generation.get()))
            .bind(encode_u64(revision.get()))
            .execute(transaction)
            .await
            .map_err(state_statement)?;
            result.rows_affected()
        }
        StateChange::AdjustOpenPinCount {
            inode_id,
            adjustment,
        } => {
            let current = match present(records, &key) {
                Some(StateRecord::Orphan(record)) => record.open_pin_count(),
                _ => return Ok(Some(missing_conflict(request, key))),
            };
            let Some(count) = adjustment.apply(current).filter(|count| *count != 0) else {
                return Ok(Some(malformed_arithmetic()));
            };
            update_numeric(
                transaction,
                r#"UPDATE "public"."w9pt_fs_state_orphans"
                   SET "open_pin_count" = $3::numeric, "record_revision" = $4::numeric
                   WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
                &[
                    request.filesystem_id().as_bytes().to_vec(),
                    inode_id.as_bytes().to_vec(),
                ],
                count,
                revision,
            )
            .await?
        }
        StateChange::BumpLockGeneration { inode_id, lock_id } => {
            let current = match present(records, &key) {
                Some(StateRecord::Lock(record)) => match record.generation().checked_next() {
                    Ok(generation) => generation,
                    Err(_) => return Ok(Some(malformed_arithmetic())),
                },
                _ => return Ok(Some(missing_conflict(request, key))),
            };
            let result = query(
                r#"UPDATE "public"."w9pt_fs_state_locks"
                   SET "lock_generation" = $4::numeric, "record_revision" = $5::numeric
                   WHERE "filesystem_id" = $1 AND "inode_id" = $2 AND "lock_id" = $3"#,
            )
            .bind(request.filesystem_id().as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .bind(lock_id.as_bytes().to_vec())
            .bind(encode_u64(current.get()))
            .bind(encode_u64(revision.get()))
            .execute(transaction)
            .await
            .map_err(state_statement)?;
            result.rows_affected()
        }
        StateChange::PublishContent(publication) => {
            let current = match inode(records, request.filesystem_id(), publication.inode_id) {
                Some(record) => record,
                None => return Ok(Some(missing_conflict(request, key))),
            };
            let metadata_key = RecordKey::ContentMetadata(
                request.filesystem_id(),
                publication.prepared.context_binding().file_id(),
            );
            let metadata = fetch_record(
                transaction,
                &metadata_key,
                schema_limits(),
                w9pt_fs_state::StateStoreOperation::Commit,
            )
            .await
            .map_err(CommitAttemptError::State)?;
            let validation = match metadata {
                Some(StateRecord::ContentMetadata(metadata)) => {
                    validate_publish_content_with_metadata(
                        publication,
                        &request.mutation(),
                        current,
                        &metadata,
                    )
                }
                _ => Err(w9pt_fs_state::PublishContentError::ContentContextMismatch),
            };
            if let Err(error) = validation {
                return Ok(Some(CommitOutcome::MalformedRequest(
                    MalformedCommit::InvalidPublication(error),
                )));
            }
            let content = publication.prepared.content();
            let times = publication.attributes.apply_times(current.times());
            let mode = publication.attributes.mode.unwrap_or(current.mode());
            let owner = publication
                .attributes
                .owner
                .as_ref()
                .unwrap_or_else(|| current.owner());
            let group = publication
                .attributes
                .group
                .as_ref()
                .unwrap_or_else(|| current.group());
            let result = query(
                r#"UPDATE "public"."w9pt_fs_state_inodes" SET
                   "record_revision" = $3::numeric, "logical_size" = $4::numeric,
                   "inode_generation" = $5::numeric, "content_file_id" = $6,
                   "data_generation" = $7::numeric, "content_generation" = $7::numeric,
                   "content_logical_size" = $4::numeric, "content_manifest_key" = $8,
                   "content_manifest_digest" = $9, "content_storage_method" = $10,
                   "accessed_seconds" = $11, "accessed_nanoseconds" = $12,
                   "modified_seconds" = $13, "modified_nanoseconds" = $14,
                   "changed_seconds" = $15, "changed_nanoseconds" = $16,
                   "created_seconds" = $17, "created_nanoseconds" = $18,
                   "mode" = $19, "owner" = $20, "group_id" = $21
                   WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
            )
            .bind(request.filesystem_id().as_bytes().to_vec())
            .bind(publication.inode_id.as_bytes().to_vec())
            .bind(encode_u64(revision.get()))
            .bind(encode_u64(publication.logical_size))
            .bind(encode_u64(publication.inode_generation.get()))
            .bind(content.file_id().as_bytes().to_vec())
            .bind(encode_u64(publication.data_generation.get()))
            .bind(content.manifest_key().as_str())
            .bind(content.manifest_digest().as_bytes().to_vec())
            .bind(match content.method() {
                StorageMethod::Raw => 1i16,
                StorageMethod::BlockSplit => 2i16,
            })
            .bind(times.accessed.seconds())
            .bind(i32::try_from(times.accessed.nanoseconds()).expect("valid nanoseconds"))
            .bind(times.modified.seconds())
            .bind(i32::try_from(times.modified.nanoseconds()).expect("valid nanoseconds"))
            .bind(times.changed.seconds())
            .bind(i32::try_from(times.changed.nanoseconds()).expect("valid nanoseconds"))
            .bind(times.created.seconds())
            .bind(i32::try_from(times.created.nanoseconds()).expect("valid nanoseconds"))
            .bind(i32::try_from(mode).expect("validated inode mode fits i32"))
            .bind(owner.as_bytes().to_vec())
            .bind(group.as_bytes().to_vec())
            .execute(transaction)
            .await
            .map_err(state_statement)?;
            result.rows_affected()
        }
        StateChange::RewrapContentMetadata(rewrap) => {
            let current = match present(records, &key) {
                Some(StateRecord::ContentMetadata(metadata)) => metadata,
                _ => return Ok(Some(missing_conflict(request, key))),
            };
            if current.context_id() != rewrap.expected_context_id
                || current.revision() != rewrap.expected_revision
                || current.wrapped_key_bytes().is_none()
            {
                return Ok(Some(CommitOutcome::Conflict(CommitConflict {
                    precondition_index: request.preconditions().len(),
                    kind: CommitConflictKind::RecordRevision {
                        key,
                        expected: rewrap.expected_revision,
                        actual: current.revision(),
                    },
                })));
            }
            if let Err(error) = current.validate_rewrap(
                rewrap.expected_context_id,
                &rewrap.wrapped_key_bytes,
                limits,
            ) {
                let malformed = match error {
                    w9pt_fs_state::ContentMetadataError::Limit(error) => {
                        MalformedCommit::Limit(error)
                    }
                    _ => MalformedCommit::InvalidContentMetadataRewrap,
                };
                return Ok(Some(CommitOutcome::MalformedRequest(malformed)));
            }
            let result = query(
                r#"UPDATE "public"."w9pt_fs_state_content_metadata"
                   SET "wrapped_key_bytes" = $3, "record_revision" = $4::numeric
                   WHERE "filesystem_id" = $1 AND "content_file_id" = $2
                     AND "context_id" = $5 AND "record_revision" = $6::numeric"#,
            )
            .bind(request.filesystem_id().as_bytes().to_vec())
            .bind(rewrap.content_file_id.as_bytes().to_vec())
            .bind(rewrap.wrapped_key_bytes.clone())
            .bind(encode_u64(revision.get()))
            .bind(rewrap.expected_context_id.as_bytes().to_vec())
            .bind(encode_u64(rewrap.expected_revision.get()))
            .execute(transaction)
            .await
            .map_err(state_statement)?;
            result.rows_affected()
        }
        StateChange::PublishXattrStaging(publication) => {
            let staging_key =
                RecordKey::XattrStaging(request.filesystem_id(), publication.staging_id);
            let Some(StateRecord::XattrStaging(staging)) = present(records, &staging_key) else {
                return Ok(Some(CommitOutcome::MalformedRequest(
                    MalformedCommit::InvalidXattrStaging,
                )));
            };
            let deleted = delete_record(transaction, &staging_key)
                .await
                .map_err(CommitAttemptError::State)?;
            if deleted == RowPresence::Absent {
                return Ok(Some(missing_conflict(request, staging_key)));
            }
            let xattr_key = RecordKey::Xattr(
                request.filesystem_id(),
                publication.inode_id,
                publication.name.clone(),
            );
            let xattr = StateRecord::Xattr(XattrRecord::new(
                publication.inode_id,
                publication.name.clone(),
                staging.bytes().clone(),
                RecordRevision::new(revision.get()).map_err(|_| {
                    CommitAttemptError::State(corruption("invalid xattr record revision"))
                })?,
            ));
            let existing = present(records, &xattr_key).is_some();
            let result = if existing {
                replace_record(transaction, &xattr_key, &xattr, revision).await
            } else {
                insert_record(transaction, &xattr_key, &xattr, revision).await
            }
            .map_err(CommitAttemptError::State)?;
            if result == RowPresence::Absent {
                return Ok(Some(missing_conflict(request, xattr_key)));
            }
            return Ok(None);
        }
    };
    if presence == 1 {
        Ok(None)
    } else {
        Ok(Some(missing_conflict(request, key)))
    }
}

async fn validate_targeted_invariants(
    transaction: &mut PostgresTransaction,
    request: &CommitRequest,
    previous: &BTreeMap<RecordKey, Option<StateRecord>>,
    limits: w9pt_fs_state::StateLimits,
) -> Result<Option<MalformedCommit>, CommitAttemptError> {
    let filesystem = request.filesystem_id().as_bytes().to_vec();
    let root_inode_id: Option<Vec<u8>> = query_scalar(
        r#"SELECT "root_inode_id"
           FROM "public"."w9pt_fs_state_filesystem_records"
           WHERE "filesystem_id" = $1"#,
    )
    .bind(filesystem.clone())
    .fetch_optional(transaction)
    .await
    .map_err(state_statement)?;
    let root_inode_id = root_inode_id.map(decode_inode_identity).transpose()?;
    let root_kind: Option<Option<i16>> = query_scalar(
        r#"SELECT "root"."kind"
           FROM "public"."w9pt_fs_state_filesystem_records" AS "filesystem"
           LEFT JOIN "public"."w9pt_fs_state_inodes" AS "root"
             ON "root"."filesystem_id" = "filesystem"."filesystem_id"
            AND "root"."inode_id" = "filesystem"."root_inode_id"
           WHERE "filesystem"."filesystem_id" = $1"#,
    )
    .bind(filesystem.clone())
    .fetch_optional(transaction)
    .await
    .map_err(state_statement)?;
    match root_kind {
        None | Some(Some(2)) => {}
        Some(None) => return Ok(Some(invalid_relation("filesystem root inode"))),
        Some(Some(_)) => {
            return Ok(Some(invalid_relation("filesystem root is not a directory")));
        }
    }
    if root_kind.is_none() {
        let has_directory_entries: bool = query_scalar(
            r#"SELECT EXISTS (
                   SELECT 1 FROM "public"."w9pt_fs_state_directory_entries"
                   WHERE "filesystem_id" = $1
               )"#,
        )
        .bind(filesystem.clone())
        .fetch_one(transaction)
        .await
        .map_err(state_statement)?;
        if has_directory_entries {
            return Ok(Some(missing_relation("directory-entry filesystem header")));
        }
    }

    for change in request.changes() {
        let (StateChange::Insert {
            record: StateRecord::DirectoryEntry(entry),
            ..
        }
        | StateChange::Replace {
            record: StateRecord::DirectoryEntry(entry),
            ..
        }) = change
        else {
            continue;
        };
        let parent_kind: Option<i16> = query_scalar(
            r#"SELECT "kind" FROM "public"."w9pt_fs_state_inodes"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
        )
        .bind(filesystem.clone())
        .bind(entry.parent_inode_id().as_bytes().to_vec())
        .fetch_optional(transaction)
        .await
        .map_err(state_statement)?;
        match parent_kind {
            None => return Ok(Some(missing_relation("directory-entry parent inode"))),
            Some(2) => {}
            Some(_) => {
                return Ok(Some(invalid_relation(
                    "directory-entry parent is not a directory",
                )));
            }
        }
        let child_exists: Option<i32> = query_scalar(
            r#"SELECT 1 FROM "public"."w9pt_fs_state_inodes"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
        )
        .bind(filesystem.clone())
        .bind(entry.child_inode_id().as_bytes().to_vec())
        .fetch_optional(transaction)
        .await
        .map_err(state_statement)?;
        if child_exists.is_none() {
            return Ok(Some(missing_relation("directory-entry child inode")));
        }
        let next_cookie: Option<String> = query_scalar(
            r#"SELECT "next_directory_cookie"::text
               FROM "public"."w9pt_fs_state_filesystem_records"
               WHERE "filesystem_id" = $1"#,
        )
        .bind(filesystem.clone())
        .fetch_optional(transaction)
        .await
        .map_err(state_statement)?;
        let Some(next_cookie) = next_cookie else {
            return Ok(Some(missing_relation("directory-entry filesystem header")));
        };
        let next_cookie = decode_u64("filesystem_records.next_directory_cookie", &next_cookie)
            .map_err(|error| CommitAttemptError::State(corruption(error.to_string())))?;
        if entry.cookie().get() >= next_cookie {
            return Ok(Some(invalid_relation(
                "directory-entry cookie was not allocated before next cookie",
            )));
        }
    }

    for change in request.changes() {
        let (StateChange::Insert {
            record: StateRecord::Lock(lock),
            ..
        }
        | StateChange::Replace {
            record: StateRecord::Lock(lock),
            ..
        }) = change
        else {
            continue;
        };
        let owner: Option<Vec<u8>> = query_scalar(
            r#"SELECT "client_incarnation_id"
               FROM "public"."w9pt_fs_state_opens"
               WHERE "filesystem_id" = $1 AND "open_id" = $2 AND "inode_id" = $3"#,
        )
        .bind(filesystem.clone())
        .bind(lock.owner().open_id().as_bytes().to_vec())
        .bind(lock.inode_id().as_bytes().to_vec())
        .fetch_optional(transaction)
        .await
        .map_err(state_statement)?;
        match owner {
            None => return Ok(Some(missing_relation("lock owner open"))),
            Some(owner)
                if owner.as_slice() == lock.owner().client_incarnation().as_bytes().as_slice() => {}
            Some(_) => {
                return Ok(Some(invalid_relation(
                    "lock owner differs from portable open",
                )));
            }
        }
    }

    let inode_ids = targeted_inode_ids(request, previous);
    for inode_id in inode_ids {
        let summary = query(
            r#"SELECT "kind", "link_count"::text, "directory_parent_inode_id"
               FROM "public"."w9pt_fs_state_inodes"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
        )
        .bind(filesystem.clone())
        .bind(inode_id.as_bytes().to_vec())
        .fetch_optional(transaction)
        .await
        .map_err(state_statement)?;
        let summary: Option<(i16, String, Option<Vec<u8>>)> = summary
            .map(|row| {
                Ok::<_, CommitAttemptError>((
                    row.try_get("kind").map_err(state_statement)?,
                    row.try_get("link_count").map_err(state_statement)?,
                    row.try_get("directory_parent_inode_id")
                        .map_err(state_statement)?,
                ))
            })
            .transpose()?;
        let Some((kind, link_count, directory_parent)) = summary else {
            let dependent_exists: bool = query_scalar(
                r#"SELECT
                       EXISTS (
                           SELECT 1 FROM "public"."w9pt_fs_state_directory_entries"
                           WHERE "filesystem_id" = $1
                             AND ("parent_inode_id" = $2 OR "child_inode_id" = $2)
                       )
                       OR EXISTS (
                           SELECT 1 FROM "public"."w9pt_fs_state_opens"
                           WHERE "filesystem_id" = $1 AND "inode_id" = $2
                       )
                       OR EXISTS (
                           SELECT 1 FROM "public"."w9pt_fs_state_locks"
                           WHERE "filesystem_id" = $1 AND "inode_id" = $2
                       )
                       OR EXISTS (
                           SELECT 1 FROM "public"."w9pt_fs_state_xattrs"
                           WHERE "filesystem_id" = $1 AND "inode_id" = $2
                       )
                       OR EXISTS (
                           SELECT 1 FROM "public"."w9pt_fs_state_xattr_staging"
                           WHERE "filesystem_id" = $1 AND "inode_id" = $2
                       )
                       OR EXISTS (
                           SELECT 1 FROM "public"."w9pt_fs_state_open_pins"
                           WHERE "filesystem_id" = $1 AND "inode_id" = $2
                       )
                       OR EXISTS (
                           SELECT 1 FROM "public"."w9pt_fs_state_orphans"
                           WHERE "filesystem_id" = $1 AND "inode_id" = $2
                       )"#,
            )
            .bind(filesystem.clone())
            .bind(inode_id.as_bytes().to_vec())
            .fetch_one(transaction)
            .await
            .map_err(state_statement)?;
            if dependent_exists {
                return Ok(Some(missing_relation("authoritative inode dependency")));
            }
            continue;
        };
        let link_count = decode_u64("inodes.link_count", &link_count)
            .map_err(|error| CommitAttemptError::State(corruption(error.to_string())))?;
        let namespace_links: String = query_scalar(
            r#"SELECT pg_catalog.count(*)::numeric(20, 0)::text
               FROM "public"."w9pt_fs_state_directory_entries"
               WHERE "filesystem_id" = $1 AND "child_inode_id" = $2"#,
        )
        .bind(filesystem.clone())
        .bind(inode_id.as_bytes().to_vec())
        .fetch_one(transaction)
        .await
        .map_err(state_statement)?;
        let namespace_links = decode_u64("directory_entry_count", &namespace_links)
            .map_err(|error| CommitAttemptError::State(corruption(error.to_string())))?;
        if kind == 2 {
            let Some(directory_parent) = directory_parent else {
                return Ok(Some(missing_relation("directory parent inode")));
            };
            let directory_parent = decode_inode_identity(directory_parent)?;
            let Some(root_inode_id) = root_inode_id else {
                return Ok(Some(missing_relation("directory filesystem header")));
            };
            if inode_id == root_inode_id {
                if directory_parent != root_inode_id {
                    return Ok(Some(invalid_relation(
                        "filesystem root directory is not self-parented",
                    )));
                }
                if namespace_links != 0 {
                    return Ok(Some(invalid_relation(
                        "filesystem root directory has a namespace hard link",
                    )));
                }
            } else {
                let parent_kind: Option<i16> = query_scalar(
                    r#"SELECT "kind" FROM "public"."w9pt_fs_state_inodes"
                       WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
                )
                .bind(filesystem.clone())
                .bind(directory_parent.as_bytes().to_vec())
                .fetch_optional(transaction)
                .await
                .map_err(state_statement)?;
                match parent_kind {
                    None => return Ok(Some(missing_relation("directory parent inode"))),
                    Some(2) => {}
                    Some(_) => {
                        return Ok(Some(invalid_relation(
                            "directory parent is not a directory",
                        )));
                    }
                }
                let parent_links: String = query_scalar(
                    r#"SELECT pg_catalog.count(*)::numeric(20, 0)::text
                       FROM "public"."w9pt_fs_state_directory_entries"
                       WHERE "filesystem_id" = $1 AND "child_inode_id" = $2
                         AND "parent_inode_id" = $3"#,
                )
                .bind(filesystem.clone())
                .bind(inode_id.as_bytes().to_vec())
                .bind(directory_parent.as_bytes().to_vec())
                .fetch_one(transaction)
                .await
                .map_err(state_statement)?;
                let parent_links = decode_u64("directory_parent_entry_count", &parent_links)
                    .map_err(|error| CommitAttemptError::State(corruption(error.to_string())))?;
                if namespace_links != 1 || parent_links != 1 {
                    return Ok(Some(invalid_relation(
                        "directory must have exactly one entry in its authoritative parent",
                    )));
                }
            }
            if let Some(error) = validate_directory_ancestry(
                transaction,
                request.filesystem_id(),
                inode_id,
                root_inode_id,
                limits.max_directory_ancestor_depth(),
            )
            .await?
            {
                return Ok(Some(error));
            }
        } else if namespace_links != link_count {
            return Ok(Some(invalid_relation(
                "inode link count differs from namespace references",
            )));
        }

        let pin_count: String = query_scalar(
            r#"SELECT pg_catalog.count(*)::numeric(20, 0)::text
               FROM "public"."w9pt_fs_state_open_pins"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
        )
        .bind(filesystem.clone())
        .bind(inode_id.as_bytes().to_vec())
        .fetch_one(transaction)
        .await
        .map_err(state_statement)?;
        let pin_count = decode_u64("open_pin_count", &pin_count)
            .map_err(|error| CommitAttemptError::State(corruption(error.to_string())))?;
        let orphan_count: Option<String> = query_scalar(
            r#"SELECT "open_pin_count"::text
               FROM "public"."w9pt_fs_state_orphans"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
        )
        .bind(filesystem.clone())
        .bind(inode_id.as_bytes().to_vec())
        .fetch_optional(transaction)
        .await
        .map_err(state_statement)?;
        let orphan_count = orphan_count
            .map(|count| decode_u64("orphans.open_pin_count", &count))
            .transpose()
            .map_err(|error| CommitAttemptError::State(corruption(error.to_string())))?;
        if let Some(orphan_count) = orphan_count {
            if link_count != 0 {
                return Ok(Some(invalid_relation("orphan inode still has links")));
            }
            if orphan_count != pin_count {
                return Ok(Some(invalid_relation("orphan open-pin count mismatch")));
            }
        }
        if link_count == 0 {
            if pin_count == 0 {
                return Ok(Some(invalid_relation(
                    "unpinned zero-link inode was not retired",
                )));
            }
            if orphan_count.is_none() {
                return Ok(Some(missing_relation("pinned zero-link inode orphan")));
            }
        }
    }
    for open_id in targeted_open_ids(request, previous) {
        let invalid_dependency: bool = query_scalar(
            r#"SELECT
                   EXISTS (
                       SELECT 1
                       FROM "public"."w9pt_fs_state_open_pins" AS "pin"
                       LEFT JOIN "public"."w9pt_fs_state_opens" AS "open"
                         ON "open"."filesystem_id" = "pin"."filesystem_id"
                        AND "open"."open_id" = "pin"."open_id"
                        AND "open"."inode_id" = "pin"."inode_id"
                       WHERE "pin"."filesystem_id" = $1
                         AND "pin"."open_id" = $2
                         AND "open"."open_id" IS NULL
                   )
                   OR EXISTS (
                       SELECT 1
                       FROM "public"."w9pt_fs_state_locks" AS "lock"
                       LEFT JOIN "public"."w9pt_fs_state_opens" AS "open"
                         ON "open"."filesystem_id" = "lock"."filesystem_id"
                        AND "open"."open_id" = "lock"."owner_open_id"
                        AND "open"."inode_id" = "lock"."inode_id"
                        AND "open"."client_incarnation_id" =
                            "lock"."owner_client_incarnation_id"
                       WHERE "lock"."filesystem_id" = $1
                         AND "lock"."owner_open_id" = $2
                         AND "open"."open_id" IS NULL
                   )"#,
        )
        .bind(filesystem.clone())
        .bind(open_id.as_bytes().to_vec())
        .fetch_one(transaction)
        .await
        .map_err(state_statement)?;
        if invalid_dependency {
            return Ok(Some(missing_relation("open-dependent record")));
        }
    }
    Ok(None)
}

async fn validate_directory_ancestry(
    transaction: &mut PostgresTransaction,
    filesystem_id: w9pt_fs_state::FilesystemId,
    inode_id: InodeId,
    root_inode_id: InodeId,
    maximum_depth: u32,
) -> Result<Option<MalformedCommit>, CommitAttemptError> {
    let mut current = inode_id;
    let mut visited = BTreeSet::new();
    // The limit counts parent edges. Reaching and validating the root after
    // exactly `maximum_depth` edges therefore needs one additional visit.
    for _ in 0..=maximum_depth {
        if !visited.insert(current) {
            return Ok(Some(MalformedCommit::InvalidRecord(
                RecordValidationError::DirectoryCycle { inode_id },
            )));
        }
        let row = query(
            r#"SELECT "kind", "directory_parent_inode_id"
               FROM "public"."w9pt_fs_state_inodes"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
        )
        .bind(filesystem_id.as_bytes().to_vec())
        .bind(current.as_bytes().to_vec())
        .fetch_optional(transaction)
        .await
        .map_err(state_statement)?;
        let Some(row) = row else {
            return Ok(Some(missing_relation("directory ancestor inode")));
        };
        let kind: i16 = row.try_get("kind").map_err(state_statement)?;
        let parent: Option<Vec<u8>> = row
            .try_get("directory_parent_inode_id")
            .map_err(state_statement)?;
        if kind != 2 {
            return Ok(Some(invalid_relation(
                "directory ancestor is not a directory",
            )));
        }
        let Some(parent) = parent else {
            return Ok(Some(missing_relation("directory parent inode")));
        };
        let parent = decode_inode_identity(parent)?;
        if current == root_inode_id {
            return if parent == root_inode_id {
                Ok(None)
            } else {
                Ok(Some(invalid_relation(
                    "filesystem root directory is not self-parented",
                )))
            };
        }
        current = parent;
    }
    Ok(Some(MalformedCommit::InvalidRecord(
        RecordValidationError::DirectoryAncestorLimit {
            maximum: maximum_depth,
        },
    )))
}

fn decode_inode_identity(bytes: Vec<u8>) -> Result<InodeId, CommitAttemptError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|value: Vec<u8>| {
        CommitAttemptError::State(corruption(format!(
            "inode identity has {} bytes instead of 16",
            value.len()
        )))
    })?;
    Ok(InodeId::new(bytes))
}

fn targeted_open_ids(
    request: &CommitRequest,
    previous: &BTreeMap<RecordKey, Option<StateRecord>>,
) -> BTreeSet<OpenId> {
    let mut opens = BTreeSet::new();
    for change in request.changes() {
        match change {
            StateChange::Insert { record, .. } | StateChange::Replace { record, .. } => {
                collect_record_opens(record, &mut opens);
            }
            StateChange::Delete(key) => {
                if let Some(record) = present(previous, key) {
                    collect_record_opens(record, &mut opens);
                }
                match key {
                    RecordKey::Open(_, open_id) | RecordKey::OpenPin(_, _, open_id) => {
                        opens.insert(*open_id);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    opens
}

fn collect_record_opens(record: &StateRecord, opens: &mut BTreeSet<OpenId>) {
    match record {
        StateRecord::Open(record) => {
            opens.insert(record.open_id());
        }
        StateRecord::OpenPin(record) => {
            opens.insert(record.open_id());
        }
        StateRecord::Lock(record) => {
            opens.insert(record.owner().open_id());
        }
        _ => {}
    }
}

fn targeted_inode_ids(
    request: &CommitRequest,
    previous: &BTreeMap<RecordKey, Option<StateRecord>>,
) -> BTreeSet<InodeId> {
    let mut inodes = BTreeSet::new();
    for change in request.changes() {
        match change {
            StateChange::Insert { record, .. } | StateChange::Replace { record, .. } => {
                collect_record_inodes(record, &mut inodes);
            }
            StateChange::Delete(key) => {
                if let Some(record) = present(previous, key) {
                    collect_record_inodes(record, &mut inodes);
                }
                collect_key_inode(key, &mut inodes);
            }
            StateChange::BumpInodeGeneration(inode_id)
            | StateChange::BumpDirectoryGeneration(inode_id)
            | StateChange::AdjustLinkCount { inode_id, .. }
            | StateChange::AdjustOpenPinCount { inode_id, .. } => {
                inodes.insert(*inode_id);
            }
            StateChange::BumpLockGeneration { inode_id, .. } => {
                inodes.insert(*inode_id);
            }
            StateChange::PublishContent(publication) => {
                inodes.insert(publication.inode_id);
            }
            StateChange::RewrapContentMetadata(_) => {}
            StateChange::PublishXattrStaging(publication) => {
                inodes.insert(publication.inode_id);
            }
            StateChange::AdvanceDirectoryCookie { .. }
            | StateChange::AdvanceQidPath { .. }
            | StateChange::BumpFilesystemPolicyGeneration => {}
        }
    }
    inodes
}

fn collect_key_inode(key: &RecordKey, inodes: &mut BTreeSet<InodeId>) {
    match key {
        RecordKey::Inode(_, inode_id)
        | RecordKey::Orphan(_, inode_id)
        | RecordKey::OpenPin(_, inode_id, _)
        | RecordKey::Lock(_, inode_id, _)
        | RecordKey::Xattr(_, inode_id, _) => {
            inodes.insert(*inode_id);
        }
        _ => {}
    }
}

fn collect_record_inodes(record: &StateRecord, inodes: &mut BTreeSet<InodeId>) {
    match record {
        StateRecord::Filesystem(record) => {
            inodes.insert(record.root_inode_id());
        }
        StateRecord::Inode(record) => {
            inodes.insert(record.inode_id());
            if let Some(parent_inode_id) = record.directory_parent() {
                inodes.insert(parent_inode_id);
            }
        }
        StateRecord::ContentMetadata(record) => {
            inodes.insert(record.owner_inode_id());
        }
        StateRecord::DirectoryEntry(record) => {
            inodes.insert(record.parent_inode_id());
            inodes.insert(record.child_inode_id());
        }
        StateRecord::Open(record) => {
            inodes.insert(record.inode_id());
        }
        StateRecord::OpenPin(record) => {
            inodes.insert(record.inode_id());
        }
        StateRecord::Orphan(record) => {
            inodes.insert(record.inode_id());
        }
        StateRecord::Lock(record) => {
            inodes.insert(record.inode_id());
        }
        StateRecord::Xattr(record) => {
            inodes.insert(record.inode_id());
        }
        StateRecord::XattrStaging(record) => {
            inodes.insert(record.inode_id());
        }
        StateRecord::Mutation(_) | StateRecord::WriterLease(_) => {}
    }
}

fn missing_relation(relation: &'static str) -> MalformedCommit {
    MalformedCommit::InvalidRecord(RecordValidationError::MissingRelatedRecord { relation })
}

fn invalid_relation(relation: &'static str) -> MalformedCommit {
    MalformedCommit::InvalidRecord(RecordValidationError::InvalidRelatedRecord { relation })
}

fn malformed_arithmetic() -> CommitOutcome {
    CommitOutcome::MalformedRequest(MalformedCommit::Arithmetic)
}

fn apply_presence(
    presence: RowPresence,
    inserting: bool,
    request: &CommitRequest,
    key: RecordKey,
) -> Result<Option<CommitOutcome>, CommitAttemptError> {
    match (presence, inserting) {
        (RowPresence::Present, _) => Ok(None),
        (RowPresence::Absent, false) => Ok(Some(missing_conflict(request, key))),
        (RowPresence::Absent, true) => Err(CommitAttemptError::State(internal(
            "record insert affected no row",
        ))),
    }
}

fn missing_conflict(request: &CommitRequest, key: RecordKey) -> CommitOutcome {
    CommitOutcome::Conflict(CommitConflict {
        precondition_index: request.preconditions().len(),
        kind: CommitConflictKind::RecordMissing(key),
    })
}

async fn update_numeric(
    transaction: &mut PostgresTransaction,
    sql: &'static str,
    ids: &[Vec<u8>],
    value: u64,
    revision: StateRevision,
) -> Result<u64, CommitAttemptError> {
    let query = query(sql).bind(ids[0].clone());
    let result = if ids.len() == 1 {
        query
            .bind(encode_u64(value))
            .bind(encode_u64(revision.get()))
            .execute(transaction)
            .await
    } else {
        query
            .bind(ids[1].clone())
            .bind(encode_u64(value))
            .bind(encode_u64(revision.get()))
            .execute(transaction)
            .await
    }
    .map_err(state_statement)?;
    Ok(result.rows_affected())
}

async fn update_inode_numbers(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
    inode_id: InodeId,
    inode_generation: Option<(&'static str, u64)>,
    _second: Option<(&'static str, u64)>,
    revision: StateRevision,
) -> Result<u64, CommitAttemptError> {
    let (_, generation) = inode_generation.expect("inode generation update is present");
    let result = query(
        r#"UPDATE "public"."w9pt_fs_state_inodes"
           SET "inode_generation" = $3::numeric, "record_revision" = $4::numeric
           WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
    )
    .bind(filesystem_id.as_bytes().to_vec())
    .bind(inode_id.as_bytes().to_vec())
    .bind(encode_u64(generation))
    .bind(encode_u64(revision.get()))
    .execute(transaction)
    .await
    .map_err(state_statement)?;
    Ok(result.rows_affected())
}

async fn publish_change(
    transaction: &mut PostgresTransaction,
    request: &CommitRequest,
    revision: StateRevision,
    keys: &[RecordKey],
    history_limit: u32,
) -> Result<(), CommitAttemptError> {
    query(
        r#"INSERT INTO "public"."w9pt_fs_state_change_commits"
           ("filesystem_id", "revision", "origin_kind", "origin_id", "key_count")
           VALUES ($1, $2::numeric, $3, $4, $5)"#,
    )
    .bind(request.filesystem_id().as_bytes().to_vec())
    .bind(encode_u64(revision.get()))
    .bind(MUTATION_ORIGIN_TAG)
    .bind(request.mutation().mutation_id.as_bytes().to_vec())
    .bind(
        i32::try_from(keys.len()).map_err(|_| {
            CommitAttemptError::State(internal("change key count does not fit i32"))
        })?,
    )
    .execute(transaction)
    .await
    .map_err(state_statement)?;
    for (ordinal, key) in keys.iter().enumerate() {
        let encoded = SqlRecordKey::encode(key);
        query(
            r#"INSERT INTO "public"."w9pt_fs_state_change_keys"
               ("filesystem_id", "revision", "ordinal", "family_tag", "component_a", "component_b")
               VALUES ($1, $2::numeric, $3, $4, $5, $6)"#,
        )
        .bind(request.filesystem_id().as_bytes().to_vec())
        .bind(encode_u64(revision.get()))
        .bind(i32::try_from(ordinal).map_err(|_| {
            CommitAttemptError::State(internal("change key ordinal does not fit i32"))
        })?)
        .bind(encoded.family_tag)
        .bind(encoded.sql_component_a().map(<[u8]>::to_vec))
        .bind(encoded.sql_component_b().map(<[u8]>::to_vec))
        .execute(transaction)
        .await
        .map_err(state_statement)?;
    }
    let oldest = revision
        .get()
        .saturating_sub(u64::from(history_limit))
        .max(1);
    query(
        r#"DELETE FROM "public"."w9pt_fs_state_change_keys"
           WHERE "filesystem_id" = $1 AND "revision" <= $2::numeric"#,
    )
    .bind(request.filesystem_id().as_bytes().to_vec())
    .bind(encode_u64(oldest))
    .execute(transaction)
    .await
    .map_err(state_statement)?;
    query(
        r#"DELETE FROM "public"."w9pt_fs_state_change_commits"
           WHERE "filesystem_id" = $1 AND "revision" <= $2::numeric"#,
    )
    .bind(request.filesystem_id().as_bytes().to_vec())
    .bind(encode_u64(oldest))
    .execute(transaction)
    .await
    .map_err(state_statement)?;
    query(
        r#"UPDATE "public"."w9pt_fs_state_authority_heads"
           SET "current_revision" = $2::numeric,
               "oldest_retained_revision" = GREATEST(
                   "oldest_retained_revision", $3::numeric
               )
           WHERE "filesystem_id" = $1"#,
    )
    .bind(request.filesystem_id().as_bytes().to_vec())
    .bind(encode_u64(revision.get()))
    .bind(encode_u64(oldest))
    .execute(transaction)
    .await
    .map_err(state_statement)?;
    Ok(())
}

fn changed_keys(request: &CommitRequest) -> Result<Vec<RecordKey>, CommitAttemptError> {
    let mut keys = canonical_affected_keys(request.filesystem_id(), request.changes())
        .map_err(|error| CommitAttemptError::State(internal(error.to_string())))?;
    keys.push(mutation_key(request));
    keys.sort();
    if keys.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(CommitAttemptError::State(internal(
            "mutation ledger key duplicates a caller change",
        )));
    }
    Ok(keys)
}

fn estimate_transaction_bytes(request: &CommitRequest) -> Option<usize> {
    let mut bytes = 256usize.checked_add(request.terminal_result().bytes().len())?;
    for precondition in request.preconditions() {
        bytes = bytes.checked_add(match precondition {
            Precondition::RecordAbsent(key) | Precondition::RecordRevision { key, .. } => {
                64usize.checked_add(estimate_key(key)?)?
            }
            _ => 128,
        })?;
    }
    for change in request.changes() {
        bytes = bytes.checked_add(match change {
            StateChange::Insert { key, record } | StateChange::Replace { key, record } => 64usize
                .checked_add(estimate_key(key)?)?
                .checked_add(estimate_record(record)?)?,
            StateChange::Delete(key) => 64usize.checked_add(estimate_key(key)?)?,
            StateChange::PublishContent(publication) => {
                let mut retained = 512usize
                    .checked_add(publication.prepared.content().manifest_key().as_str().len())?;
                if let Some(owner) = &publication.attributes.owner {
                    retained = retained.checked_add(owner.as_bytes().len())?;
                }
                if let Some(group) = &publication.attributes.group {
                    retained = retained.checked_add(group.as_bytes().len())?;
                }
                retained
            }
            StateChange::PublishXattrStaging(publication) => {
                192usize.checked_add(publication.name.as_bytes().len())?
            }
            _ => 128,
        })?;
    }
    Some(bytes)
}

fn estimate_key(key: &RecordKey) -> Option<usize> {
    64usize.checked_add(match key {
        RecordKey::DirectoryEntry(_, _, name) => name.as_bytes().len(),
        RecordKey::Xattr(_, _, name) => name.as_bytes().len(),
        _ => 0,
    })
}

fn estimate_record(record: &StateRecord) -> Option<usize> {
    let (fixed, variable) = match record {
        StateRecord::Filesystem(_) => (128usize, 0usize),
        StateRecord::Inode(record) => {
            let mut variable = record.owner().as_bytes().len();
            variable = variable.checked_add(record.group().as_bytes().len())?;
            match record.data() {
                InodeData::RegularFile {
                    content: Some(content),
                    ..
                } => variable = variable.checked_add(content.manifest_key().as_str().len())?,
                InodeData::Symlink { target } => {
                    variable = variable.checked_add(target.as_bytes().len())?
                }
                _ => {}
            }
            (256, variable)
        }
        StateRecord::ContentMetadata(record) => (
            160,
            record
                .policy_bytes()
                .len()
                .checked_add(record.wrapped_key_bytes().map_or(0, <[u8]>::len))?,
        ),
        StateRecord::DirectoryEntry(record) => (96, record.name().as_bytes().len()),
        StateRecord::Open(_)
        | StateRecord::OpenPin(_)
        | StateRecord::Orphan(_)
        | StateRecord::Lock(_) => (128, 0),
        StateRecord::Xattr(record) => (
            96,
            record
                .name()
                .as_bytes()
                .len()
                .checked_add(record.value().as_bytes().len())?,
        ),
        StateRecord::XattrStaging(record) => (
            112,
            record
                .name()
                .as_bytes()
                .len()
                .checked_add(record.bytes().as_bytes().len())?,
        ),
        StateRecord::Mutation(record) => (256, record.result().bytes().len()),
        StateRecord::WriterLease(_) => (128, 0),
    };
    fixed.checked_add(variable)
}

fn mutation_key(request: &CommitRequest) -> RecordKey {
    RecordKey::Mutation(request.filesystem_id(), request.mutation().mutation_id)
}

fn present<'a>(
    records: &'a BTreeMap<RecordKey, Option<StateRecord>>,
    key: &RecordKey,
) -> Option<&'a StateRecord> {
    records.get(key).and_then(Option::as_ref)
}

fn inode(
    records: &BTreeMap<RecordKey, Option<StateRecord>>,
    filesystem_id: FilesystemId,
    inode_id: InodeId,
) -> Option<&InodeRecord> {
    match present(records, &RecordKey::Inode(filesystem_id, inode_id)) {
        Some(StateRecord::Inode(record)) => Some(record),
        _ => None,
    }
}

fn decode_revision(field: &'static str, value: &str) -> Result<StateRevision, PostgresStateError> {
    let value = decode_u64(field, value).map_err(|error| corruption(error.to_string()))?;
    StateRevision::new(value).map_err(|error| corruption(error.to_string()))
}

fn retry_action(phase: SqlOperationPhase, error: &DbErr) -> Option<RetryAction> {
    let class = classify_database_error(phase, error);
    match class {
        SqlFailureClass::RetryIdentical(_) => Some(RetryAction::RetryIdentical),
        SqlFailureClass::KnownConstraint(violation)
            if matches!(
                violation.action(),
                ConstraintAction::ResolveLedger | ConstraintAction::RecheckSemanticState
            ) =>
        {
            Some(RetryAction::ResolveLedger)
        }
        SqlFailureClass::AmbiguousCommit => Some(RetryAction::AmbiguousCommit),
        _ => None,
    }
}

fn state_retry_action(error: &PostgresStateError) -> Option<RetryAction> {
    match error.sql_failure() {
        Some(SqlFailureClass::RetryIdentical(_)) => Some(RetryAction::RetryIdentical),
        Some(SqlFailureClass::KnownConstraint(violation))
            if matches!(
                violation.action(),
                ConstraintAction::ResolveLedger | ConstraintAction::RecheckSemanticState
            ) =>
        {
            Some(RetryAction::ResolveLedger)
        }
        _ if error.kind() == AdapterFailureKind::Serialization => Some(RetryAction::RetryIdentical),
        _ => None,
    }
}

fn statement_sql(source: DbErr) -> CommitAttemptError {
    CommitAttemptError::Sql {
        phase: SqlOperationPhase::ExecuteStatement,
        source,
    }
}

fn state_statement(source: DbErr) -> CommitAttemptError {
    CommitAttemptError::State(PostgresStateError::from_database(
        StateStoreOperation::Commit,
        SqlOperationPhase::ExecuteStatement,
        source,
    ))
}

async fn rollback(transaction: PostgresTransaction) -> Result<(), CommitAttemptError> {
    transaction
        .rollback()
        .await
        .map_err(|source| CommitAttemptError::Sql {
            phase: SqlOperationPhase::Rollback,
            source,
        })
}

fn corruption(detail: impl Into<String>) -> PostgresStateError {
    PostgresStateError::new(
        StateStoreOperation::Commit,
        AdapterFailureKind::Corruption,
        detail,
    )
}

fn internal(detail: impl Into<String>) -> PostgresStateError {
    PostgresStateError::new(
        StateStoreOperation::Commit,
        AdapterFailureKind::Internal,
        detail,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt_fs_state::{
        ClientIncarnationId, FencingToken, LeaseId, MutationContext, MutationResult,
        MutationResultKind, MutationRetention, RequestFingerprint, ResultFormatVersion,
        StateLimits, WriterFence, WriterIncarnationId, WriterScopeId,
    };

    fn request(changes: Vec<StateChange>) -> CommitRequest {
        let limits = StateLimits::default();
        CommitRequest::new(
            FilesystemId::from_u128(1),
            MutationContext::new(
                w9pt_fs_storage::MutationId::from_u128(2),
                RequestFingerprint::blake3(b"commit"),
                ClientIncarnationId::from_u128(3),
                MutationRetention::new(4),
            ),
            WriterFence::new(
                WriterScopeId::from_u128(5),
                WriterIncarnationId::from_u128(6),
                LeaseId::from_u128(7),
                FencingToken::new(8).unwrap(),
            ),
            vec![],
            changes,
            MutationResult::new(
                MutationResultKind::new(1).unwrap(),
                ResultFormatVersion::new(1).unwrap(),
                b"ok".to_vec(),
                limits,
            )
            .unwrap(),
            limits,
        )
        .unwrap()
    }

    #[test]
    fn lock_order_and_change_event_keys_are_canonical_and_include_ledger() {
        let request = request(vec![
            StateChange::BumpInodeGeneration(InodeId::from_u128(9)),
            StateChange::BumpFilesystemPolicyGeneration,
        ]);
        let keys = lock_keys(&request);
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        let event = changed_keys(&request).unwrap();
        assert!(event.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(event.contains(&mutation_key(&request)));
    }

    #[test]
    fn retry_policy_distinguishes_commit_ambiguity() {
        let error = DbErr::Query(sea_orm::RuntimeErr::Internal("lost response".to_owned()));
        assert_eq!(
            retry_action(SqlOperationPhase::Commit, &error),
            Some(RetryAction::AmbiguousCommit)
        );
        assert_eq!(
            retry_action(SqlOperationPhase::ExecuteStatement, &error),
            None
        );
    }

    #[test]
    fn transaction_estimate_uses_the_finalized_fixed_costs() {
        let request = request(vec![StateChange::BumpFilesystemPolicyGeneration]);
        assert_eq!(estimate_transaction_bytes(&request), Some(256 + 2 + 128));
    }
}
