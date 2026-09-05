//! Serializable idempotent PostgreSQL writer-lease operations.

use sea_orm::{DbErr, TryGetable};
use w9pt_fs_state::{
    AcquireLeaseOutcome, AcquireWriterLease, AdapterFailureKind, FenceValidation, FencingToken,
    FilesystemId, LeaseDeadline, LeaseOperationId, LeaseRejection, RecordRevision,
    ReleaseLeaseOutcome, ReleaseWriterLease, RenewLeaseOutcome, RenewWriterLease,
    RequestFingerprint, StateRecord, StateRevision, StateStoreAdapterError, StateStoreOperation,
    WriterFence, WriterIncarnationId, WriterLeaseGrant, WriterLeaseRecord, WriterScopeId,
    WriterTopology, grant_writer_lease, renew_current_lease, validate_lease_release,
    validate_writer_fence,
};

use crate::{
    LeaseClockSource, PostgresStateConfig, PostgresStateError,
    clock::capture_lease_deadline,
    database::{
        PostgresConnection, PostgresRow, PostgresTransaction, classify_database_error, query,
        query_scalar,
    },
    numeric::{decode_u64, encode_u64},
    row_codec::{SqlStateRecord, WriterLeaseRow},
    sqlstate::{ConstraintAction, SqlFailureClass, SqlOperationPhase},
    transaction::{TransactionAccess, begin_transaction},
};

const ACQUIRE_KIND: i16 = 1;
const RENEW_KIND: i16 = 2;
const RELEASE_KIND: i16 = 3;

const SUCCESS_TAG: i16 = 1;

const ACQUIRE_BUSY_TAG: i16 = 2;
const ACQUIRE_INVALID_SCOPE_TAG: i16 = 3;
const ACQUIRE_INVALID_DURATION_TAG: i16 = 4;
const ACQUIRE_FENCE_EXHAUSTED_TAG: i16 = 5;

const RENEW_STALE_TAG: i16 = 2;
const RENEW_EXPIRED_TAG: i16 = 3;
const RENEW_INVALID_SCOPE_TAG: i16 = 4;
const RENEW_INVALID_DURATION_TAG: i16 = 5;

const RELEASE_STALE_TAG: i16 = 2;
const RELEASE_EXPIRED_TAG: i16 = 3;
const RELEASE_INVALID_SCOPE_TAG: i16 = 4;

const LEASE_CHANGE_ORIGIN_KIND: i16 = 2;
const WRITER_LEASE_FAMILY_TAG: i16 = 11;

#[derive(Clone, Copy, Debug)]
enum LeaseRequest {
    Acquire(AcquireWriterLease),
    Renew(RenewWriterLease),
    Release(ReleaseWriterLease),
}

impl LeaseRequest {
    const fn operation(self) -> StateStoreOperation {
        match self {
            Self::Acquire(_) => StateStoreOperation::AcquireLease,
            Self::Renew(_) => StateStoreOperation::RenewLease,
            Self::Release(_) => StateStoreOperation::ReleaseLease,
        }
    }

    const fn filesystem_id(self) -> FilesystemId {
        match self {
            Self::Acquire(request) => request.filesystem_id(),
            Self::Renew(request) => request.filesystem_id(),
            Self::Release(request) => request.filesystem_id(),
        }
    }

    const fn operation_id(self) -> LeaseOperationId {
        match self {
            Self::Acquire(request) => request.operation_id(),
            Self::Renew(request) => request.operation_id(),
            Self::Release(request) => request.operation_id(),
        }
    }

    const fn scope(self) -> WriterScopeId {
        match self {
            Self::Acquire(request) => request.scope(),
            Self::Renew(request) => request.fence().scope,
            Self::Release(request) => request.fence().scope,
        }
    }

    fn fingerprint(self) -> RequestFingerprint {
        match self {
            Self::Acquire(request) => request.operation_fingerprint(),
            Self::Renew(request) => request.operation_fingerprint(),
            Self::Release(request) => request.operation_fingerprint(),
        }
    }

    const fn kind(self) -> i16 {
        match self {
            Self::Acquire(_) => ACQUIRE_KIND,
            Self::Renew(_) => RENEW_KIND,
            Self::Release(_) => RELEASE_KIND,
        }
    }

    fn validate_receiver_limits(
        self,
        config: PostgresStateConfig,
    ) -> Option<LeaseOperationOutcome> {
        match self {
            Self::Acquire(request) if request.validate(config.limits()).is_err() => {
                Some(LeaseOperationOutcome::Acquire(
                    AcquireLeaseOutcome::Rejected(LeaseRejection::InvalidDuration),
                ))
            }
            Self::Renew(request) if request.validate(config.limits()).is_err() => {
                Some(LeaseOperationOutcome::Renew(RenewLeaseOutcome::Rejected(
                    LeaseRejection::InvalidDuration,
                )))
            }
            _ => None,
        }
    }

    const fn operation_mismatch(self) -> LeaseOperationOutcome {
        match self {
            Self::Acquire(_) => LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::Rejected(
                LeaseRejection::OperationMismatch,
            )),
            Self::Renew(_) => LeaseOperationOutcome::Renew(RenewLeaseOutcome::Rejected(
                LeaseRejection::OperationMismatch,
            )),
            Self::Release(_) => LeaseOperationOutcome::Release(ReleaseLeaseOutcome::Rejected(
                LeaseRejection::OperationMismatch,
            )),
        }
    }

    const fn history_full(self) -> LeaseOperationOutcome {
        match self {
            Self::Acquire(_) => LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::Rejected(
                LeaseRejection::OperationHistoryFull,
            )),
            Self::Renew(_) => LeaseOperationOutcome::Renew(RenewLeaseOutcome::Rejected(
                LeaseRejection::OperationHistoryFull,
            )),
            Self::Release(_) => LeaseOperationOutcome::Release(ReleaseLeaseOutcome::Rejected(
                LeaseRejection::OperationHistoryFull,
            )),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum LeaseOperationOutcome {
    Acquire(AcquireLeaseOutcome),
    Renew(RenewLeaseOutcome),
    Release(ReleaseLeaseOutcome),
}

#[derive(Debug)]
enum LeaseAttemptError {
    Sql {
        phase: SqlOperationPhase,
        source: DbErr,
    },
    State(PostgresStateError),
}

impl LeaseAttemptError {
    fn sql(phase: SqlOperationPhase, source: DbErr) -> Self {
        Self::Sql { phase, source }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetryReason {
    DefinitiveAbort,
    ResolveLedger,
    AmbiguousCommit,
}

#[derive(Debug)]
struct StoredLeaseOperation {
    kind: i16,
    fingerprint: Vec<u8>,
    outcome_tag: i16,
    grant: Option<WriterLeaseGrant>,
}

#[derive(Debug)]
struct LockedFenceState {
    greatest: Option<FencingToken>,
    current: Option<WriterLeaseRecord>,
}

#[derive(Clone, Copy, Debug)]
struct RetainedOutcome {
    tag: i16,
    grant: Option<WriterLeaseGrant>,
}

/// Acquires or takes over one writer scope with exact operation replay.
pub(crate) async fn acquire_writer_lease(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    clock: LeaseClockSource,
    request: AcquireWriterLease,
) -> Result<AcquireLeaseOutcome, PostgresStateError> {
    match execute_with_retries(pool, config, clock, LeaseRequest::Acquire(request)).await? {
        LeaseOperationOutcome::Acquire(outcome) => Ok(outcome),
        _ => Err(internal(
            StateStoreOperation::AcquireLease,
            "lease executor returned the wrong outcome family",
        )),
    }
}

/// Renews the exact current lease without changing or shortening its fence.
pub(crate) async fn renew_writer_lease(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    clock: LeaseClockSource,
    request: RenewWriterLease,
) -> Result<RenewLeaseOutcome, PostgresStateError> {
    match execute_with_retries(pool, config, clock, LeaseRequest::Renew(request)).await? {
        LeaseOperationOutcome::Renew(outcome) => Ok(outcome),
        _ => Err(internal(
            StateStoreOperation::RenewLease,
            "lease executor returned the wrong outcome family",
        )),
    }
}

/// Releases the exact current lease while retaining its greatest fence forever.
pub(crate) async fn release_writer_lease(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    clock: LeaseClockSource,
    request: ReleaseWriterLease,
) -> Result<ReleaseLeaseOutcome, PostgresStateError> {
    match execute_with_retries(pool, config, clock, LeaseRequest::Release(request)).await? {
        LeaseOperationOutcome::Release(outcome) => Ok(outcome),
        _ => Err(internal(
            StateStoreOperation::ReleaseLease,
            "lease executor returned the wrong outcome family",
        )),
    }
}

/// Locks and validates the active lease used by a non-replayed filesystem commit.
///
/// The commit path must lock its authority head first and must pass the single
/// database-clock observation used by the whole transaction.
pub(crate) async fn validate_commit_writer_fence(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
    presented: WriterFence,
    now: LeaseDeadline,
    limits: w9pt_fs_state::StateLimits,
) -> Result<FenceValidation, PostgresStateError> {
    let state = load_fence_state(
        transaction,
        filesystem_id,
        presented.scope,
        limits,
        StateStoreOperation::Commit,
    )
    .await
    .map_err(attempt_into_state)?;
    Ok(validate_writer_fence(
        filesystem_id,
        presented,
        state.current.as_ref(),
        now,
    ))
}

async fn execute_with_retries(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    clock: LeaseClockSource,
    request: LeaseRequest,
) -> Result<LeaseOperationOutcome, PostgresStateError> {
    let mut definitive_retries = 0u32;
    let mut recovery_attempts = 0u32;
    loop {
        match execute_once(pool, config, clock, request).await {
            Ok(outcome) => return Ok(outcome),
            Err(LeaseAttemptError::State(error)) => {
                if error.kind() == AdapterFailureKind::Serialization
                    && definitive_retries < config.definitive_abort_retries()
                {
                    definitive_retries += 1;
                    continue;
                }
                return Err(error);
            }
            Err(LeaseAttemptError::Sql { phase, source }) => {
                let retry = classify_retry(phase, &source);
                match retry {
                    Some(RetryReason::DefinitiveAbort)
                        if definitive_retries < config.definitive_abort_retries() =>
                    {
                        definitive_retries += 1;
                    }
                    Some(RetryReason::ResolveLedger | RetryReason::AmbiguousCommit)
                        if recovery_attempts < config.ambiguous_commit_recovery_attempts() =>
                    {
                        recovery_attempts += 1;
                    }
                    _ => {
                        return Err(PostgresStateError::from_database(
                            request.operation(),
                            phase,
                            source,
                        ));
                    }
                }
            }
        }
    }
}

async fn execute_once(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    clock: LeaseClockSource,
    request: LeaseRequest,
) -> Result<LeaseOperationOutcome, LeaseAttemptError> {
    let operation = request.operation();
    let mut transaction = begin_transaction(pool, config, TransactionAccess::ReadWrite, operation)
        .await
        .map_err(LeaseAttemptError::State)?;

    if let Some(stored) = load_stored_operation(&mut transaction, request).await? {
        let outcome = replay_stored_operation(request, &stored)
            .map_err(|detail| LeaseAttemptError::State(corruption(operation, detail)))?;
        commit_attempt(transaction).await?;
        return Ok(outcome);
    }

    if let Some(outcome) = request.validate_receiver_limits(config) {
        commit_attempt(transaction).await?;
        return Ok(outcome);
    }
    if lease_history_full(&mut transaction, config, request.filesystem_id()).await? {
        let outcome = request.history_full();
        commit_attempt(transaction).await?;
        return Ok(outcome);
    }

    let current_revision = lock_authority_head(
        &mut transaction,
        request.filesystem_id(),
        request.operation(),
    )
    .await?;
    let fence_state = load_fence_state(
        &mut transaction,
        request.filesystem_id(),
        request.scope(),
        config.limits(),
        operation,
    )
    .await?;
    let now = capture_lease_deadline(clock, &mut transaction, operation)
        .await
        .map_err(LeaseAttemptError::State)?;

    let outcome = evaluate_and_stage(
        &mut transaction,
        request,
        current_revision,
        fence_state,
        now,
        config.limits().max_change_history_commits(),
    )
    .await?;
    commit_attempt(transaction).await?;
    Ok(outcome)
}

async fn evaluate_and_stage(
    transaction: &mut PostgresTransaction,
    request: LeaseRequest,
    current_revision: StateRevision,
    fence_state: LockedFenceState,
    now: LeaseDeadline,
    max_change_history: u32,
) -> Result<LeaseOperationOutcome, LeaseAttemptError> {
    let placeholder_revision = RecordRevision::new(1).expect("one is a valid record revision");
    match request {
        LeaseRequest::Acquire(acquire) => {
            let provisional = grant_writer_lease(
                acquire,
                fence_state.current.as_ref(),
                fence_state.greatest,
                now,
                WriterTopology::SerializableMultiWriter,
                placeholder_revision,
            );
            let outcome = match provisional {
                Ok(_) => {
                    let (next, record_revision) =
                        next_revision(current_revision, request.operation())?;
                    let record = grant_writer_lease(
                        acquire,
                        fence_state.current.as_ref(),
                        fence_state.greatest,
                        now,
                        WriterTopology::SerializableMultiWriter,
                        record_revision,
                    )
                    .map_err(|_| {
                        LeaseAttemptError::State(internal(
                            request.operation(),
                            "lease grant changed outcome during one transaction",
                        ))
                    })?;
                    write_active_fence(transaction, &record).await?;
                    publish_lease_revision(transaction, request, next, max_change_history).await?;
                    LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::Granted(grant(&record)))
                }
                Err(rejection) => {
                    LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::Rejected(rejection))
                }
            };
            retain_operation(transaction, request, outcome).await?;
            Ok(outcome)
        }
        LeaseRequest::Renew(renew) => {
            let provisional = renew_current_lease(
                renew,
                fence_state.current.as_ref(),
                now,
                WriterTopology::SerializableMultiWriter,
                placeholder_revision,
            );
            let outcome = match provisional {
                Ok(_) => {
                    let (next, record_revision) =
                        next_revision(current_revision, request.operation())?;
                    let record = renew_current_lease(
                        renew,
                        fence_state.current.as_ref(),
                        now,
                        WriterTopology::SerializableMultiWriter,
                        record_revision,
                    )
                    .map_err(|_| {
                        LeaseAttemptError::State(internal(
                            request.operation(),
                            "lease renewal changed outcome during one transaction",
                        ))
                    })?;
                    update_active_fence(transaction, &record, request.operation()).await?;
                    publish_lease_revision(transaction, request, next, max_change_history).await?;
                    LeaseOperationOutcome::Renew(RenewLeaseOutcome::Renewed(grant(&record)))
                }
                Err(rejection) => {
                    LeaseOperationOutcome::Renew(RenewLeaseOutcome::Rejected(rejection))
                }
            };
            retain_operation(transaction, request, outcome).await?;
            Ok(outcome)
        }
        LeaseRequest::Release(release) => {
            let outcome = match validate_lease_release(
                release,
                fence_state.current.as_ref(),
                now,
                WriterTopology::SerializableMultiWriter,
            ) {
                Ok(()) => {
                    let (next, _) = next_revision(current_revision, request.operation())?;
                    clear_active_fence(
                        transaction,
                        request.filesystem_id(),
                        request.scope(),
                        request.operation(),
                    )
                    .await?;
                    publish_lease_revision(transaction, request, next, max_change_history).await?;
                    LeaseOperationOutcome::Release(ReleaseLeaseOutcome::Released)
                }
                Err(rejection) => {
                    LeaseOperationOutcome::Release(ReleaseLeaseOutcome::Rejected(rejection))
                }
            };
            retain_operation(transaction, request, outcome).await?;
            Ok(outcome)
        }
    }
}

async fn load_stored_operation(
    transaction: &mut PostgresTransaction,
    request: LeaseRequest,
) -> Result<Option<StoredLeaseOperation>, LeaseAttemptError> {
    let row = query(
        r#"SELECT
               "operation_kind",
               "request_fingerprint",
               "outcome_tag",
               "grant_scope_id",
               "grant_holder_id",
               "grant_lease_id",
               "grant_deadline_tick"::text AS "grant_deadline_tick",
               "grant_fencing_token"::text AS "grant_fencing_token"
           FROM "public"."w9pt_fs_state_writer_lease_operations"
           WHERE "filesystem_id" = $1 AND "lease_operation_id" = $2"#,
    )
    .bind(request.filesystem_id().as_bytes().to_vec())
    .bind(request.operation_id().as_bytes().to_vec())
    .fetch_optional(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    row.map(|row| decode_stored_operation(row, request.operation()))
        .transpose()
}

fn decode_stored_operation(
    row: PostgresRow,
    operation: StateStoreOperation,
) -> Result<StoredLeaseOperation, LeaseAttemptError> {
    let kind: i16 = decode_column(&row, "operation_kind")?;
    if !(ACQUIRE_KIND..=RELEASE_KIND).contains(&kind) {
        return Err(LeaseAttemptError::State(corruption(
            operation,
            format!("stored lease operation has unknown kind tag {kind}"),
        )));
    }
    let fingerprint: Vec<u8> = decode_column(&row, "request_fingerprint")?;
    if fingerprint.len() != 32 {
        return Err(LeaseAttemptError::State(corruption(
            operation,
            "stored lease-operation fingerprint has the wrong width",
        )));
    }
    let outcome_tag: i16 = decode_column(&row, "outcome_tag")?;
    let scope: Option<Vec<u8>> = decode_column(&row, "grant_scope_id")?;
    let holder: Option<Vec<u8>> = decode_column(&row, "grant_holder_id")?;
    let lease: Option<Vec<u8>> = decode_column(&row, "grant_lease_id")?;
    let deadline: Option<String> = decode_column(&row, "grant_deadline_tick")?;
    let token: Option<String> = decode_column(&row, "grant_fencing_token")?;
    let grant = match (scope, holder, lease, deadline, token) {
        (None, None, None, None, None) => None,
        (Some(scope), Some(holder), Some(lease), Some(deadline), Some(token)) => Some(
            decode_grant(scope, holder, lease, &deadline, &token)
                .map_err(|detail| LeaseAttemptError::State(corruption(operation, detail)))?,
        ),
        _ => {
            return Err(LeaseAttemptError::State(corruption(
                operation,
                "stored lease-operation grant fields are only partially present",
            )));
        }
    };
    Ok(StoredLeaseOperation {
        kind,
        fingerprint,
        outcome_tag,
        grant,
    })
}

fn replay_stored_operation(
    request: LeaseRequest,
    stored: &StoredLeaseOperation,
) -> Result<LeaseOperationOutcome, String> {
    if stored.kind != request.kind()
        || stored.fingerprint.as_slice() != request.fingerprint().as_bytes()
    {
        return Ok(request.operation_mismatch());
    }
    match request {
        LeaseRequest::Acquire(_) => match (stored.outcome_tag, stored.grant) {
            (SUCCESS_TAG, Some(grant)) => Ok(LeaseOperationOutcome::Acquire(
                AcquireLeaseOutcome::AlreadyApplied(grant),
            )),
            (ACQUIRE_BUSY_TAG, Some(grant)) => Ok(LeaseOperationOutcome::Acquire(
                AcquireLeaseOutcome::Rejected(LeaseRejection::Busy(grant)),
            )),
            (ACQUIRE_INVALID_SCOPE_TAG, None) => Ok(LeaseOperationOutcome::Acquire(
                AcquireLeaseOutcome::Rejected(LeaseRejection::InvalidWriterScope),
            )),
            (ACQUIRE_INVALID_DURATION_TAG, None) => Ok(LeaseOperationOutcome::Acquire(
                AcquireLeaseOutcome::Rejected(LeaseRejection::InvalidDuration),
            )),
            (ACQUIRE_FENCE_EXHAUSTED_TAG, None) => Ok(LeaseOperationOutcome::Acquire(
                AcquireLeaseOutcome::Rejected(LeaseRejection::FencingTokenExhausted),
            )),
            _ => Err("stored acquire outcome has an invalid tag or grant shape".to_owned()),
        },
        LeaseRequest::Renew(_) => match (stored.outcome_tag, stored.grant) {
            (SUCCESS_TAG, Some(grant)) => Ok(LeaseOperationOutcome::Renew(
                RenewLeaseOutcome::AlreadyApplied(grant),
            )),
            (RENEW_STALE_TAG, None) => Ok(LeaseOperationOutcome::Renew(
                RenewLeaseOutcome::Rejected(LeaseRejection::StaleFence),
            )),
            (RENEW_EXPIRED_TAG, None) => Ok(LeaseOperationOutcome::Renew(
                RenewLeaseOutcome::Rejected(LeaseRejection::Expired),
            )),
            (RENEW_INVALID_SCOPE_TAG, None) => Ok(LeaseOperationOutcome::Renew(
                RenewLeaseOutcome::Rejected(LeaseRejection::InvalidWriterScope),
            )),
            (RENEW_INVALID_DURATION_TAG, None) => Ok(LeaseOperationOutcome::Renew(
                RenewLeaseOutcome::Rejected(LeaseRejection::InvalidDuration),
            )),
            _ => Err("stored renew outcome has an invalid tag or grant shape".to_owned()),
        },
        LeaseRequest::Release(_) => match (stored.outcome_tag, stored.grant) {
            (SUCCESS_TAG, None) => Ok(LeaseOperationOutcome::Release(
                ReleaseLeaseOutcome::AlreadyApplied,
            )),
            (RELEASE_STALE_TAG, None) => Ok(LeaseOperationOutcome::Release(
                ReleaseLeaseOutcome::Rejected(LeaseRejection::StaleFence),
            )),
            (RELEASE_EXPIRED_TAG, None) => Ok(LeaseOperationOutcome::Release(
                ReleaseLeaseOutcome::Rejected(LeaseRejection::Expired),
            )),
            (RELEASE_INVALID_SCOPE_TAG, None) => Ok(LeaseOperationOutcome::Release(
                ReleaseLeaseOutcome::Rejected(LeaseRejection::InvalidWriterScope),
            )),
            _ => Err("stored release outcome has an invalid tag or grant shape".to_owned()),
        },
    }
}

async fn lease_history_full(
    transaction: &mut PostgresTransaction,
    config: PostgresStateConfig,
    filesystem_id: FilesystemId,
) -> Result<bool, LeaseAttemptError> {
    let maximum = i64::from(config.limits().max_lease_operation_history());
    let retained: i64 = query_scalar(
        r#"SELECT count(*)::bigint
           FROM (
               SELECT 1
               FROM "public"."w9pt_fs_state_writer_lease_operations"
               WHERE "filesystem_id" = $1
               LIMIT $2
           ) AS "bounded_history""#,
    )
    .bind(filesystem_id.as_bytes().to_vec())
    .bind(maximum)
    .fetch_one(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    Ok(retained >= maximum)
}

async fn lock_authority_head(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
    operation: StateStoreOperation,
) -> Result<StateRevision, LeaseAttemptError> {
    let filesystem_bytes = filesystem_id.as_bytes().to_vec();
    query(
        r#"INSERT INTO "public"."w9pt_fs_state_authority_heads" (
               "filesystem_id", "current_revision", "oldest_retained_revision"
           ) VALUES ($1, 1, 1)
           ON CONFLICT ("filesystem_id") DO NOTHING"#,
    )
    .bind(filesystem_bytes.clone())
    .execute(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    let revision: String = query_scalar(
        r#"SELECT "current_revision"::text
           FROM "public"."w9pt_fs_state_authority_heads"
           WHERE "filesystem_id" = $1
           FOR UPDATE"#,
    )
    .bind(filesystem_bytes)
    .fetch_one(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    decode_u64("authority_head.current_revision", &revision)
        .and_then(|value| {
            StateRevision::new(value).map_err(|_| crate::numeric::NumericCodecError {
                field: "authority_head.current_revision",
                value: revision,
            })
        })
        .map_err(|error| LeaseAttemptError::State(corruption(operation, error.to_string())))
}

async fn load_fence_state(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
    scope: WriterScopeId,
    limits: w9pt_fs_state::StateLimits,
    operation: StateStoreOperation,
) -> Result<LockedFenceState, LeaseAttemptError> {
    let row = query(
        r#"SELECT
               "greatest_fencing_token"::text AS "greatest_fencing_token",
               "active_holder_id",
               "active_lease_id",
               "active_deadline_tick"::text AS "active_deadline_tick",
               "active_record_revision"::text AS "active_record_revision"
           FROM "public"."w9pt_fs_state_writer_fences"
           WHERE "filesystem_id" = $1 AND "writer_scope_id" = $2
           FOR UPDATE"#,
    )
    .bind(filesystem_id.as_bytes().to_vec())
    .bind(scope.as_bytes().to_vec())
    .fetch_optional(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    let Some(row) = row else {
        return Ok(LockedFenceState {
            greatest: None,
            current: None,
        });
    };
    let greatest_text: String = decode_column(&row, "greatest_fencing_token")?;
    let greatest_value = decode_u64("writer_fences.greatest_fencing_token", &greatest_text)
        .map_err(|error| LeaseAttemptError::State(corruption(operation, error.to_string())))?;
    let greatest = FencingToken::new(greatest_value)
        .map_err(|error| LeaseAttemptError::State(corruption(operation, error.to_string())))?;
    let holder: Option<Vec<u8>> = decode_column(&row, "active_holder_id")?;
    let lease_id: Option<Vec<u8>> = decode_column(&row, "active_lease_id")?;
    let deadline: Option<String> = decode_column(&row, "active_deadline_tick")?;
    let revision: Option<String> = decode_column(&row, "active_record_revision")?;
    let current = match (holder, lease_id, deadline, revision) {
        (None, None, None, None) => None,
        (Some(holder), Some(lease_id), Some(deadline), Some(revision)) => {
            let decoded = SqlStateRecord::WriterLease(WriterLeaseRow {
                filesystem_id: filesystem_id.as_bytes().to_vec(),
                writer_scope_id: scope.as_bytes().to_vec(),
                greatest_fencing_token: greatest_text,
                active_holder_id: holder,
                active_lease_id: lease_id,
                active_deadline_tick: deadline,
                active_record_revision: revision,
            })
            .decode(limits)
            .map_err(|error| LeaseAttemptError::State(corruption(operation, error.to_string())))?;
            match decoded.1 {
                StateRecord::WriterLease(record) => Some(record),
                _ => {
                    return Err(LeaseAttemptError::State(internal(
                        operation,
                        "writer-fence row decoded to another record family",
                    )));
                }
            }
        }
        _ => {
            return Err(LeaseAttemptError::State(corruption(
                operation,
                "writer-fence active fields are only partially present",
            )));
        }
    };
    Ok(LockedFenceState {
        greatest: Some(greatest),
        current,
    })
}

async fn write_active_fence(
    transaction: &mut PostgresTransaction,
    record: &WriterLeaseRecord,
) -> Result<(), LeaseAttemptError> {
    query(
        r#"INSERT INTO "public"."w9pt_fs_state_writer_fences" (
               "filesystem_id", "writer_scope_id", "greatest_fencing_token",
               "active_holder_id", "active_lease_id", "active_deadline_tick",
               "active_record_revision"
           ) VALUES ($1, $2, $3::numeric, $4, $5, $6::numeric, $7::numeric)
           ON CONFLICT ("filesystem_id", "writer_scope_id") DO UPDATE SET
               "greatest_fencing_token" = EXCLUDED."greatest_fencing_token",
               "active_holder_id" = EXCLUDED."active_holder_id",
               "active_lease_id" = EXCLUDED."active_lease_id",
               "active_deadline_tick" = EXCLUDED."active_deadline_tick",
               "active_record_revision" = EXCLUDED."active_record_revision""#,
    )
    .bind(record.filesystem_id().as_bytes().to_vec())
    .bind(record.scope().as_bytes().to_vec())
    .bind(encode_u64(record.fencing_token().get()))
    .bind(record.holder().as_bytes().to_vec())
    .bind(record.lease_id().as_bytes().to_vec())
    .bind(encode_u64(record.deadline().ticks()))
    .bind(encode_u64(record.revision().get()))
    .execute(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    Ok(())
}

async fn update_active_fence(
    transaction: &mut PostgresTransaction,
    record: &WriterLeaseRecord,
    operation: StateStoreOperation,
) -> Result<(), LeaseAttemptError> {
    let result = query(
        r#"UPDATE "public"."w9pt_fs_state_writer_fences" SET
               "active_holder_id" = $3,
               "active_lease_id" = $4,
               "active_deadline_tick" = $5::numeric,
               "active_record_revision" = $6::numeric
           WHERE "filesystem_id" = $1 AND "writer_scope_id" = $2"#,
    )
    .bind(record.filesystem_id().as_bytes().to_vec())
    .bind(record.scope().as_bytes().to_vec())
    .bind(record.holder().as_bytes().to_vec())
    .bind(record.lease_id().as_bytes().to_vec())
    .bind(encode_u64(record.deadline().ticks()))
    .bind(encode_u64(record.revision().get()))
    .execute(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    require_one_row(
        result.rows_affected(),
        operation,
        "renewing active writer fence",
    )
}

async fn clear_active_fence(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
    scope: WriterScopeId,
    operation: StateStoreOperation,
) -> Result<(), LeaseAttemptError> {
    let result = query(
        r#"UPDATE "public"."w9pt_fs_state_writer_fences" SET
               "active_holder_id" = NULL,
               "active_lease_id" = NULL,
               "active_deadline_tick" = NULL,
               "active_record_revision" = NULL
           WHERE "filesystem_id" = $1 AND "writer_scope_id" = $2"#,
    )
    .bind(filesystem_id.as_bytes().to_vec())
    .bind(scope.as_bytes().to_vec())
    .execute(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    require_one_row(
        result.rows_affected(),
        operation,
        "releasing active writer fence",
    )
}

async fn retain_operation(
    transaction: &mut PostgresTransaction,
    request: LeaseRequest,
    outcome: LeaseOperationOutcome,
) -> Result<(), LeaseAttemptError> {
    let retained = retained_outcome(request, outcome)
        .map_err(|detail| LeaseAttemptError::State(internal(request.operation(), detail)))?;
    let (scope, holder, lease, deadline, token) = match retained.grant {
        Some(grant) => (
            Some(grant.fence.scope.as_bytes().to_vec()),
            Some(grant.fence.holder.as_bytes().to_vec()),
            Some(grant.fence.lease_id.as_bytes().to_vec()),
            Some(encode_u64(grant.deadline.ticks())),
            Some(encode_u64(grant.fence.fencing_token.get())),
        ),
        None => (None, None, None, None, None),
    };
    query(
        r#"INSERT INTO "public"."w9pt_fs_state_writer_lease_operations" (
               "filesystem_id", "lease_operation_id", "operation_kind",
               "request_fingerprint", "outcome_tag", "grant_scope_id",
               "grant_holder_id", "grant_lease_id", "grant_deadline_tick",
               "grant_fencing_token"
           ) VALUES (
               $1, $2, $3, $4, $5, $6, $7, $8, $9::numeric, $10::numeric
           )"#,
    )
    .bind(request.filesystem_id().as_bytes().to_vec())
    .bind(request.operation_id().as_bytes().to_vec())
    .bind(request.kind())
    .bind(request.fingerprint().as_bytes().to_vec())
    .bind(retained.tag)
    .bind(scope)
    .bind(holder)
    .bind(lease)
    .bind(deadline)
    .bind(token)
    .execute(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    Ok(())
}

fn retained_outcome(
    request: LeaseRequest,
    outcome: LeaseOperationOutcome,
) -> Result<RetainedOutcome, &'static str> {
    match (request, outcome) {
        (LeaseRequest::Acquire(_), LeaseOperationOutcome::Acquire(outcome)) => match outcome {
            AcquireLeaseOutcome::Granted(grant) => Ok(RetainedOutcome {
                tag: SUCCESS_TAG,
                grant: Some(grant),
            }),
            AcquireLeaseOutcome::Rejected(LeaseRejection::Busy(grant)) => Ok(RetainedOutcome {
                tag: ACQUIRE_BUSY_TAG,
                grant: Some(grant),
            }),
            AcquireLeaseOutcome::Rejected(LeaseRejection::InvalidWriterScope) => {
                Ok(RetainedOutcome {
                    tag: ACQUIRE_INVALID_SCOPE_TAG,
                    grant: None,
                })
            }
            AcquireLeaseOutcome::Rejected(LeaseRejection::InvalidDuration) => Ok(RetainedOutcome {
                tag: ACQUIRE_INVALID_DURATION_TAG,
                grant: None,
            }),
            AcquireLeaseOutcome::Rejected(LeaseRejection::FencingTokenExhausted) => {
                Ok(RetainedOutcome {
                    tag: ACQUIRE_FENCE_EXHAUSTED_TAG,
                    grant: None,
                })
            }
            _ => Err("attempted to retain an invalid acquire outcome"),
        },
        (LeaseRequest::Renew(_), LeaseOperationOutcome::Renew(outcome)) => match outcome {
            RenewLeaseOutcome::Renewed(grant) => Ok(RetainedOutcome {
                tag: SUCCESS_TAG,
                grant: Some(grant),
            }),
            RenewLeaseOutcome::Rejected(LeaseRejection::StaleFence) => Ok(RetainedOutcome {
                tag: RENEW_STALE_TAG,
                grant: None,
            }),
            RenewLeaseOutcome::Rejected(LeaseRejection::Expired) => Ok(RetainedOutcome {
                tag: RENEW_EXPIRED_TAG,
                grant: None,
            }),
            RenewLeaseOutcome::Rejected(LeaseRejection::InvalidWriterScope) => {
                Ok(RetainedOutcome {
                    tag: RENEW_INVALID_SCOPE_TAG,
                    grant: None,
                })
            }
            RenewLeaseOutcome::Rejected(LeaseRejection::InvalidDuration) => Ok(RetainedOutcome {
                tag: RENEW_INVALID_DURATION_TAG,
                grant: None,
            }),
            _ => Err("attempted to retain an invalid renew outcome"),
        },
        (LeaseRequest::Release(_), LeaseOperationOutcome::Release(outcome)) => match outcome {
            ReleaseLeaseOutcome::Released => Ok(RetainedOutcome {
                tag: SUCCESS_TAG,
                grant: None,
            }),
            ReleaseLeaseOutcome::Rejected(LeaseRejection::StaleFence) => Ok(RetainedOutcome {
                tag: RELEASE_STALE_TAG,
                grant: None,
            }),
            ReleaseLeaseOutcome::Rejected(LeaseRejection::Expired) => Ok(RetainedOutcome {
                tag: RELEASE_EXPIRED_TAG,
                grant: None,
            }),
            ReleaseLeaseOutcome::Rejected(LeaseRejection::InvalidWriterScope) => {
                Ok(RetainedOutcome {
                    tag: RELEASE_INVALID_SCOPE_TAG,
                    grant: None,
                })
            }
            _ => Err("attempted to retain an invalid release outcome"),
        },
        _ => Err("lease request and outcome families differ"),
    }
}

async fn publish_lease_revision(
    transaction: &mut PostgresTransaction,
    request: LeaseRequest,
    revision: StateRevision,
    max_change_history: u32,
) -> Result<(), LeaseAttemptError> {
    let revision_text = encode_u64(revision.get());
    let oldest_retained = retained_change_cursor(revision, max_change_history);
    let oldest_retained_text = encode_u64(oldest_retained);
    let updated = query(
        r#"UPDATE "public"."w9pt_fs_state_authority_heads"
           SET "current_revision" = $2::numeric,
               "oldest_retained_revision" = GREATEST(
                   "oldest_retained_revision", $3::numeric
               )
           WHERE "filesystem_id" = $1"#,
    )
    .bind(request.filesystem_id().as_bytes().to_vec())
    .bind(revision_text.clone())
    .bind(oldest_retained_text.clone())
    .execute(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    require_one_row(
        updated.rows_affected(),
        request.operation(),
        "advancing authority revision",
    )?;

    query(
        r#"INSERT INTO "public"."w9pt_fs_state_change_commits" (
               "filesystem_id", "revision", "origin_kind", "origin_id", "key_count"
           ) VALUES ($1, $2::numeric, $3, $4, 1)"#,
    )
    .bind(request.filesystem_id().as_bytes().to_vec())
    .bind(revision_text.clone())
    .bind(LEASE_CHANGE_ORIGIN_KIND)
    .bind(request.operation_id().as_bytes().to_vec())
    .execute(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    query(
        r#"INSERT INTO "public"."w9pt_fs_state_change_keys" (
               "filesystem_id", "revision", "ordinal", "family_tag",
               "component_a", "component_b"
           ) VALUES ($1, $2::numeric, 0, $3, $4, NULL)"#,
    )
    .bind(request.filesystem_id().as_bytes().to_vec())
    .bind(revision_text)
    .bind(WRITER_LEASE_FAMILY_TAG)
    .bind(request.scope().as_bytes().to_vec())
    .execute(transaction)
    .await
    .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;

    if oldest_retained > 1 {
        query(
            r#"DELETE FROM "public"."w9pt_fs_state_change_keys"
               WHERE "filesystem_id" = $1 AND "revision" <= $2::numeric"#,
        )
        .bind(request.filesystem_id().as_bytes().to_vec())
        .bind(oldest_retained_text.clone())
        .execute(transaction)
        .await
        .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
        query(
            r#"DELETE FROM "public"."w9pt_fs_state_change_commits"
               WHERE "filesystem_id" = $1 AND "revision" <= $2::numeric"#,
        )
        .bind(request.filesystem_id().as_bytes().to_vec())
        .bind(oldest_retained_text)
        .execute(transaction)
        .await
        .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::ExecuteStatement, source))?;
    }
    Ok(())
}

fn next_revision(
    current: StateRevision,
    operation: StateStoreOperation,
) -> Result<(StateRevision, RecordRevision), LeaseAttemptError> {
    let next = current
        .checked_next()
        .map_err(|error| LeaseAttemptError::State(internal(operation, error.to_string())))?;
    let record = RecordRevision::new(next.get())
        .map_err(|error| LeaseAttemptError::State(corruption(operation, error.to_string())))?;
    Ok((next, record))
}

fn retained_change_cursor(current: StateRevision, maximum_events: u32) -> u64 {
    current
        .get()
        .saturating_sub(u64::from(maximum_events))
        .max(1)
}

fn grant(record: &WriterLeaseRecord) -> WriterLeaseGrant {
    WriterLeaseGrant {
        fence: WriterFence::new(
            record.scope(),
            record.holder(),
            record.lease_id(),
            record.fencing_token(),
        ),
        deadline: record.deadline(),
    }
}

fn decode_grant(
    scope: Vec<u8>,
    holder: Vec<u8>,
    lease: Vec<u8>,
    deadline: &str,
    token: &str,
) -> Result<WriterLeaseGrant, String> {
    let scope = WriterScopeId::new(fixed_16("grant_scope_id", scope)?);
    let holder = WriterIncarnationId::new(fixed_16("grant_holder_id", holder)?);
    let lease_id = w9pt_fs_state::LeaseId::new(fixed_16("grant_lease_id", lease)?);
    let deadline = LeaseDeadline::new(
        decode_u64("grant_deadline_tick", deadline).map_err(|error| error.to_string())?,
    );
    let token = FencingToken::new(
        decode_u64("grant_fencing_token", token).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(WriterLeaseGrant {
        fence: WriterFence::new(scope, holder, lease_id, token),
        deadline,
    })
}

fn fixed_16(field: &'static str, bytes: Vec<u8>) -> Result<[u8; 16], String> {
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| format!("{field} has width {}, expected 16", bytes.len()))
}

fn decode_column<T>(row: &PostgresRow, column: &'static str) -> Result<T, LeaseAttemptError>
where
    T: TryGetable,
{
    row.try_get(column)
        .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::DecodeRow, source))
}

async fn commit_attempt(transaction: PostgresTransaction) -> Result<(), LeaseAttemptError> {
    transaction
        .commit()
        .await
        .map_err(|source| LeaseAttemptError::sql(SqlOperationPhase::Commit, source))
}

fn require_one_row(
    affected: u64,
    operation: StateStoreOperation,
    action: &'static str,
) -> Result<(), LeaseAttemptError> {
    if affected == 1 {
        Ok(())
    } else {
        Err(LeaseAttemptError::State(corruption(
            operation,
            format!("{action} affected {affected} rows, expected one"),
        )))
    }
}

fn classify_retry(phase: SqlOperationPhase, error: &DbErr) -> Option<RetryReason> {
    let class = classify_database_error(phase, error);
    match class {
        SqlFailureClass::RetryIdentical(_) => Some(RetryReason::DefinitiveAbort),
        SqlFailureClass::KnownConstraint(violation)
            if violation.action() == ConstraintAction::ResolveLedger =>
        {
            Some(RetryReason::ResolveLedger)
        }
        SqlFailureClass::AmbiguousCommit => Some(RetryReason::AmbiguousCommit),
        _ => None,
    }
}

fn attempt_into_state(error: LeaseAttemptError) -> PostgresStateError {
    match error {
        LeaseAttemptError::Sql { phase, source } => {
            PostgresStateError::from_database(StateStoreOperation::Commit, phase, source)
        }
        LeaseAttemptError::State(error) => error,
    }
}

fn internal(operation: StateStoreOperation, detail: impl Into<String>) -> PostgresStateError {
    PostgresStateError::new(operation, AdapterFailureKind::Internal, detail)
}

fn corruption(operation: StateStoreOperation, detail: impl Into<String>) -> PostgresStateError {
    PostgresStateError::new(operation, AdapterFailureKind::Corruption, detail)
}

#[cfg(test)]
mod tests {
    use crate::sqlstate::classify_sqlstate;
    use sea_orm::{ConnectOptions, Database};
    use w9pt_fs_state::{LeaseDuration, LeaseId, StateLimits, WriterIncarnationId};

    use super::*;

    fn acquire_request() -> AcquireWriterLease {
        AcquireWriterLease::new(
            FilesystemId::from_u128(1),
            LeaseOperationId::from_u128(2),
            WriterScopeId::from_u128(3),
            WriterIncarnationId::from_u128(4),
            LeaseId::from_u128(5),
            LeaseDuration::new(10).unwrap(),
            StateLimits::default(),
        )
        .unwrap()
    }

    fn sample_grant() -> WriterLeaseGrant {
        WriterLeaseGrant {
            fence: WriterFence::new(
                WriterScopeId::from_u128(3),
                WriterIncarnationId::from_u128(4),
                LeaseId::from_u128(5),
                FencingToken::new(6).unwrap(),
            ),
            deadline: LeaseDeadline::new(10),
        }
    }

    fn renew_request(grant: WriterLeaseGrant) -> RenewWriterLease {
        RenewWriterLease::new(
            FilesystemId::from_u128(1),
            LeaseOperationId::from_u128(7),
            grant.fence,
            LeaseDuration::new(10).unwrap(),
            StateLimits::default(),
        )
        .unwrap()
    }

    fn release_request(grant: WriterLeaseGrant) -> ReleaseWriterLease {
        ReleaseWriterLease::new(
            FilesystemId::from_u128(1),
            LeaseOperationId::from_u128(8),
            grant.fence,
        )
    }

    fn stored(
        request: LeaseRequest,
        outcome_tag: i16,
        grant: Option<WriterLeaseGrant>,
    ) -> StoredLeaseOperation {
        StoredLeaseOperation {
            kind: request.kind(),
            fingerprint: request.fingerprint().as_bytes().to_vec(),
            outcome_tag,
            grant,
        }
    }

    #[test]
    fn exact_success_and_busy_replays_preserve_retained_grants() {
        let request = LeaseRequest::Acquire(acquire_request());
        let grant = sample_grant();
        assert!(matches!(
            replay_stored_operation(request, &stored(request, SUCCESS_TAG, Some(grant))).unwrap(),
            LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::AlreadyApplied(replayed))
                if replayed == grant
        ));
        assert!(matches!(
            replay_stored_operation(
                request,
                &stored(request, ACQUIRE_BUSY_TAG, Some(grant))
            )
            .unwrap(),
            LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::Rejected(
                LeaseRejection::Busy(replayed)
            )) if replayed == grant
        ));
    }

    #[test]
    fn one_operation_id_namespace_rejects_kind_or_fingerprint_reuse() {
        let request = LeaseRequest::Acquire(acquire_request());
        let mut wrong_kind = stored(request, SUCCESS_TAG, Some(sample_grant()));
        wrong_kind.kind = RENEW_KIND;
        assert!(matches!(
            replay_stored_operation(request, &wrong_kind).unwrap(),
            LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::Rejected(
                LeaseRejection::OperationMismatch
            ))
        ));
        let mut wrong_fingerprint = stored(request, SUCCESS_TAG, Some(sample_grant()));
        wrong_fingerprint.fingerprint[0] ^= 0xff;
        assert!(matches!(
            replay_stored_operation(request, &wrong_fingerprint).unwrap(),
            LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::Rejected(
                LeaseRejection::OperationMismatch
            ))
        ));
    }

    #[test]
    fn retained_tags_round_trip_every_persistable_rejection() {
        let request = LeaseRequest::Acquire(acquire_request());
        for rejection in [
            LeaseRejection::InvalidWriterScope,
            LeaseRejection::InvalidDuration,
            LeaseRejection::FencingTokenExhausted,
        ] {
            let outcome = LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::Rejected(rejection));
            let retained = retained_outcome(request, outcome).unwrap();
            assert!(matches!(
                replay_stored_operation(
                    request,
                    &stored(request, retained.tag, retained.grant)
                )
                .unwrap(),
                LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::Rejected(actual))
                    if actual == rejection
            ));
        }
    }

    #[test]
    fn nonretained_limit_and_identity_outcomes_have_no_sql_tag() {
        let request = LeaseRequest::Acquire(acquire_request());
        for rejection in [
            LeaseRejection::OperationMismatch,
            LeaseRejection::OperationHistoryFull,
        ] {
            let outcome = LeaseOperationOutcome::Acquire(AcquireLeaseOutcome::Rejected(rejection));
            assert!(retained_outcome(request, outcome).is_err());
        }
    }

    #[test]
    fn renew_and_release_tags_replay_exact_semantic_outcomes() {
        let grant = sample_grant();
        let renew = LeaseRequest::Renew(renew_request(grant));
        for rejection in [
            LeaseRejection::StaleFence,
            LeaseRejection::Expired,
            LeaseRejection::InvalidWriterScope,
            LeaseRejection::InvalidDuration,
        ] {
            let outcome = LeaseOperationOutcome::Renew(RenewLeaseOutcome::Rejected(rejection));
            let retained = retained_outcome(renew, outcome).unwrap();
            assert!(matches!(
                replay_stored_operation(
                    renew,
                    &stored(renew, retained.tag, retained.grant)
                )
                .unwrap(),
                LeaseOperationOutcome::Renew(RenewLeaseOutcome::Rejected(actual))
                    if actual == rejection
            ));
        }

        let release = LeaseRequest::Release(release_request(grant));
        for rejection in [
            LeaseRejection::StaleFence,
            LeaseRejection::Expired,
            LeaseRejection::InvalidWriterScope,
        ] {
            let outcome = LeaseOperationOutcome::Release(ReleaseLeaseOutcome::Rejected(rejection));
            let retained = retained_outcome(release, outcome).unwrap();
            assert!(matches!(
                replay_stored_operation(
                    release,
                    &stored(release, retained.tag, retained.grant)
                )
                .unwrap(),
                LeaseOperationOutcome::Release(ReleaseLeaseOutcome::Rejected(actual))
                    if actual == rejection
            ));
        }
        let success = LeaseOperationOutcome::Release(ReleaseLeaseOutcome::Released);
        let retained = retained_outcome(release, success).unwrap();
        assert!(matches!(
            replay_stored_operation(release, &stored(release, retained.tag, retained.grant))
                .unwrap(),
            LeaseOperationOutcome::Release(ReleaseLeaseOutcome::AlreadyApplied)
        ));
    }

    #[test]
    fn retained_change_cursor_keeps_only_the_configured_whole_events() {
        assert_eq!(retained_change_cursor(StateRevision::new(2).unwrap(), 1), 1);
        assert_eq!(retained_change_cursor(StateRevision::new(3).unwrap(), 1), 2);
        assert_eq!(
            retained_change_cursor(StateRevision::new(11).unwrap(), 4),
            7
        );
    }

    #[test]
    fn retry_classifier_is_exact_and_commit_errors_are_conservative() {
        assert_eq!(
            classify_sqlstate(SqlOperationPhase::ExecuteStatement, "40001", None),
            SqlFailureClass::RetryIdentical(crate::sqlstate::DefinitiveAbort::SerializationFailure)
        );
        assert_eq!(
            classify_sqlstate(SqlOperationPhase::ExecuteStatement, "40P01", None),
            SqlFailureClass::RetryIdentical(crate::sqlstate::DefinitiveAbort::DeadlockDetected)
        );
        assert_eq!(
            classify_sqlstate(SqlOperationPhase::Commit, "99999", None),
            SqlFailureClass::AmbiguousCommit
        );
    }

    #[test]
    fn all_state_sql_is_fully_qualified() {
        let source = include_str!("lease.rs");
        for table in [
            "authority_heads",
            "writer_fences",
            "writer_lease_operations",
            "change_commits",
            "change_keys",
        ] {
            assert!(source.contains(&format!("\"public\".\"w9pt_fs_state_{table}\"")));
        }
    }

    #[tokio::test]
    async fn live_lease_lifecycle_replays_and_never_reuses_a_fence()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("W9PT_POSTGRES_TEST_DSN") else {
            return Ok(());
        };
        let mut options = ConnectOptions::new(dsn);
        options.max_connections(4).sqlx_logging(false);
        let database = Database::connect(options).await?;
        crate::migration::migrate(&database, PostgresStateConfig::default()).await?;
        let config = PostgresStateConfig::default();
        let limits = config.limits();
        let filesystem_id = FilesystemId::from_u128(u128::MAX - 202);
        for table in [
            "change_keys",
            "change_commits",
            "writer_lease_operations",
            "writer_fences",
            "authority_heads",
        ] {
            query(format!(
                "DELETE FROM \"public\".\"w9pt_fs_state_{table}\" WHERE \"filesystem_id\" = $1"
            ))
            .bind(filesystem_id.as_bytes().to_vec())
            .execute(&database)
            .await?;
        }
        let scope = WriterScopeId::from_u128(1);
        let holder = WriterIncarnationId::from_u128(2);
        let first = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(3),
            scope,
            holder,
            LeaseId::from_u128(4),
            LeaseDuration::new(10_000_000)?,
            limits,
        )?;
        let AcquireLeaseOutcome::Granted(first_grant) =
            acquire_writer_lease(&database, config, LeaseClockSource::Database, first).await?
        else {
            panic!("first acquire was not granted");
        };
        assert_eq!(
            acquire_writer_lease(&database, config, LeaseClockSource::Database, first).await?,
            AcquireLeaseOutcome::AlreadyApplied(first_grant)
        );

        let renew = RenewWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(5),
            first_grant.fence,
            LeaseDuration::new(10_000_000)?,
            limits,
        )?;
        let RenewLeaseOutcome::Renewed(renewed) =
            renew_writer_lease(&database, config, LeaseClockSource::Database, renew).await?
        else {
            panic!("renew was not applied");
        };
        assert_eq!(renewed.fence, first_grant.fence);
        assert!(renewed.deadline >= first_grant.deadline);

        let release =
            ReleaseWriterLease::new(filesystem_id, LeaseOperationId::from_u128(6), renewed.fence);
        assert_eq!(
            release_writer_lease(&database, config, LeaseClockSource::Database, release).await?,
            ReleaseLeaseOutcome::Released
        );
        assert_eq!(
            release_writer_lease(&database, config, LeaseClockSource::Database, release).await?,
            ReleaseLeaseOutcome::AlreadyApplied
        );

        let second = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(7),
            scope,
            holder,
            LeaseId::from_u128(8),
            LeaseDuration::new(10_000_000)?,
            limits,
        )?;
        let AcquireLeaseOutcome::Granted(second_grant) =
            acquire_writer_lease(&database, config, LeaseClockSource::Database, second).await?
        else {
            panic!("second acquire was not granted");
        };
        assert!(second_grant.fence.fencing_token.get() > first_grant.fence.fencing_token.get());
        Ok(())
    }
}
