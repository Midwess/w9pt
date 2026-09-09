//! Stateless orchestration over caller-owned state, content, policy, and identity providers.

use core::fmt;
use core::future::Future;

use w9pt::filesystem::{FilesystemResult, FilesystemResultKind};
use w9pt_fs_state::{
    CommitConflict, CommitOutcome, CommitRequest, FilesystemId, FilesystemStateStore,
    MalformedCommit, MutationContext, MutationMismatch, MutationRecord, ReadBatch, ReadConsistency,
    ReadOutcome, ReadQuery, ReadResult, StateLimitError, StateRecord, WriterFence,
};

use crate::{AuthorityFailure, EngineLimits, ResultCodecError, decode_mutation_result};

/// Result of the mandatory mutation-ledger probe performed before all other effects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LedgerReplay {
    /// No retained mutation exists; semantic planning may begin.
    Absent,
    /// Exact retained success reconstructed without allocating or uploading.
    Exact(FilesystemResult),
}

/// Failure probing or decoding authoritative mutation replay state.
#[derive(Debug)]
pub enum LedgerProbeError<S> {
    /// State adapter failed before a semantic replay answer was available.
    State(S),
    /// Stable mutation identity was reused for another complete request.
    MutationMismatch(MutationMismatch),
    /// Retained bytes were unknown, malformed, excessive, or noncanonical.
    ResultCodec(ResultCodecError),
    /// Retained success kind does not match the originating filesystem operation.
    ResultKindMismatch {
        /// Exact kind required by the original request.
        expected: FilesystemResultKind,
        /// Kind decoded from the retained result.
        actual: FilesystemResultKind,
    },
    /// The adapter unexpectedly could not satisfy a latest linearizable read.
    RevisionUnavailable,
    /// The adapter rejected a request constructed within its advertised limits.
    MalformedRead(StateLimitError),
    /// A point lookup unexpectedly reported a scan-only byte-bound result.
    UnexpectedReadOutcome,
}

impl<S: fmt::Display> fmt::Display for LedgerProbeError<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(error) => write!(formatter, "mutation ledger read failed: {error}"),
            Self::MutationMismatch(mismatch) => {
                write!(formatter, "mutation ledger identity mismatch: {mismatch:?}")
            }
            Self::ResultCodec(error) => error.fmt(formatter),
            Self::ResultKindMismatch { expected, actual } => write!(
                formatter,
                "retained result kind {actual:?} does not match {expected:?}"
            ),
            Self::RevisionUnavailable => {
                formatter.write_str("latest mutation ledger revision is unavailable")
            }
            Self::MalformedRead(error) => error.fmt(formatter),
            Self::UnexpectedReadOutcome => {
                formatter.write_str("unexpected mutation ledger read outcome")
            }
        }
    }
}

impl<S> std::error::Error for LedgerProbeError<S> where S: std::error::Error + 'static {}

/// Planner output that does not preserve the runner-owned mutation authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlannedCommitMismatch {
    /// The planner selected another filesystem authority.
    FilesystemId,
    /// The planner changed stable mutation identity, fingerprint, client, or retention.
    MutationContext,
    /// The planner selected another writer scope, holder, lease, or fencing token.
    WriterFence,
}

impl fmt::Display for PlannedCommitMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "planned commit changed runner authority: {self:?}"
        )
    }
}

impl std::error::Error for PlannedCommitMismatch {}

/// Probes exact retained mutation state before identity allocation or target access.
pub async fn probe_mutation_ledger<S: FilesystemStateStore>(
    state: &S,
    filesystem_id: FilesystemId,
    mutation: MutationContext,
    expected_result: FilesystemResultKind,
    engine_limits: EngineLimits,
) -> Result<LedgerReplay, LedgerProbeError<S::Error>> {
    let state_limits = state.contract().limits();
    let request = ReadBatch::new(
        filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Mutation(mutation.mutation_id)],
        state_limits,
    )
    .map_err(LedgerProbeError::MalformedRead)?;
    let outcome = state.read(request).await.map_err(LedgerProbeError::State)?;
    let record = match outcome {
        ReadOutcome::Snapshot(snapshot) => match &snapshot.results()[0] {
            ReadResult::Point {
                record: Some(record),
                ..
            } => match record.as_ref() {
                StateRecord::Mutation(record) => Some(record.clone()),
                _ => return Err(LedgerProbeError::UnexpectedReadOutcome),
            },
            ReadResult::Point { record: None, .. } => None,
            _ => return Err(LedgerProbeError::UnexpectedReadOutcome),
        },
        ReadOutcome::RevisionUnavailable { .. } => {
            return Err(LedgerProbeError::RevisionUnavailable);
        }
        ReadOutcome::MalformedRequest(error) => return Err(LedgerProbeError::MalformedRead(error)),
        ReadOutcome::ScanBoundTooSmall { .. } => {
            return Err(LedgerProbeError::UnexpectedReadOutcome);
        }
    };
    classify_retained_mutation(mutation, record.as_ref(), expected_result, engine_limits)
}

fn classify_retained_mutation<S>(
    mutation: MutationContext,
    record: Option<&MutationRecord>,
    expected_result: FilesystemResultKind,
    engine_limits: EngineLimits,
) -> Result<LedgerReplay, LedgerProbeError<S>> {
    let Some(record) = record else {
        return Ok(LedgerReplay::Absent);
    };
    let committed = match mutation.classify_record(record) {
        w9pt_fs_state::MutationReplay::Exact(committed) => committed,
        w9pt_fs_state::MutationReplay::Mismatch(mismatch) => {
            return Err(LedgerProbeError::MutationMismatch(mismatch));
        }
        w9pt_fs_state::MutationReplay::Absent => {
            return Err(LedgerProbeError::MutationMismatch(
                MutationMismatch::MutationId,
            ));
        }
    };
    let result = decode_mutation_result(&committed.result, engine_limits)
        .map_err(LedgerProbeError::ResultCodec)?;
    if result.kind() != expected_result {
        return Err(LedgerProbeError::ResultKindMismatch {
            expected: expected_result,
            actual: result.kind(),
        });
    }
    Ok(LedgerReplay::Exact(result))
}

/// Failure from bounded mutation planning, commit, replay, or conflict handling.
#[derive(Debug)]
pub enum MutationRunnerError<S, P> {
    /// Mandatory ledger-first probing failed.
    Ledger(LedgerProbeError<S>),
    /// Full semantic reread/authorization/preparation failed.
    Plan(P),
    /// Planner output changed the runner-owned filesystem, mutation, or writer authority.
    PlannedCommit(PlannedCommitMismatch),
    /// State adapter failed without a definitive semantic outcome.
    State(S),
    /// Exact writer or commit authority requires host action.
    Authority(AuthorityFailure),
    /// Stable mutation identity was reused for another request.
    MutationMismatch(MutationMismatch),
    /// Adapter rejected a structurally invalid planned commit.
    MalformedCommit(MalformedCommit),
    /// Committed/replayed terminal bytes were malformed or the wrong result kind.
    ResultCodec(ResultCodecError),
    /// Definitive conflicts exhausted the configured semantic retry bound.
    ConflictExhausted(CommitConflict),
}

impl<S: fmt::Display, P: fmt::Display> fmt::Display for MutationRunnerError<S, P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ledger(error) => error.fmt(formatter),
            Self::Plan(error) => write!(formatter, "semantic mutation planning failed: {error}"),
            Self::PlannedCommit(error) => error.fmt(formatter),
            Self::State(error) => write!(formatter, "semantic mutation commit failed: {error}"),
            Self::Authority(error) => write!(formatter, "mutation authority failed: {error:?}"),
            Self::MutationMismatch(error) => write!(formatter, "mutation mismatch: {error:?}"),
            Self::MalformedCommit(error) => error.fmt(formatter),
            Self::ResultCodec(error) => error.fmt(formatter),
            Self::ConflictExhausted(error) => {
                write!(formatter, "semantic conflict retry exhausted: {error:?}")
            }
        }
    }
}

impl<S, P> std::error::Error for MutationRunnerError<S, P>
where
    S: std::error::Error + 'static,
    P: std::error::Error + 'static,
{
}

/// Runs one ledger-first mutation with bounded full replanning and exact ambiguity replay.
///
/// `plan` must perform a fresh one-revision read, authorization, and optional immutable
/// preparation on every call. The runner never reuses a plan after a definitive conflict and
/// never changes a commit request while its outcome is ambiguous.
pub async fn run_mutation<S, P, F, Fut>(
    state: &S,
    filesystem_id: FilesystemId,
    mutation: MutationContext,
    fence: WriterFence,
    expected_result: FilesystemResultKind,
    limits: EngineLimits,
    mut plan: F,
) -> Result<FilesystemResult, MutationRunnerError<S::Error, P>>
where
    S: FilesystemStateStore,
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<CommitRequest, P>>,
{
    match probe_mutation_ledger(state, filesystem_id, mutation, expected_result, limits)
        .await
        .map_err(MutationRunnerError::Ledger)?
    {
        LedgerReplay::Exact(result) => return Ok(result),
        LedgerReplay::Absent => {}
    }

    let mut semantic_attempt = 0u32;
    loop {
        let request = plan(semantic_attempt)
            .await
            .map_err(MutationRunnerError::Plan)?;
        validate_planned_commit(&request, filesystem_id, mutation, fence)
            .map_err(MutationRunnerError::PlannedCommit)?;
        let outcome = state
            .commit(request.clone())
            .await
            .map_err(MutationRunnerError::State)?;
        match outcome {
            CommitOutcome::Committed(committed) | CommitOutcome::AlreadyCommitted(committed) => {
                return decode_committed(committed.result, expected_result, limits)
                    .map_err(MutationRunnerError::ResultCodec);
            }
            CommitOutcome::MutationMismatch(mismatch) => {
                return Err(MutationRunnerError::MutationMismatch(mismatch));
            }
            CommitOutcome::StaleFence => {
                return Err(MutationRunnerError::Authority(AuthorityFailure::StaleFence));
            }
            CommitOutcome::ExpiredLease => {
                return Err(MutationRunnerError::Authority(
                    AuthorityFailure::ExpiredLease,
                ));
            }
            CommitOutcome::MalformedRequest(error) => {
                return Err(MutationRunnerError::MalformedCommit(error));
            }
            CommitOutcome::Conflict(conflict) => {
                if semantic_attempt >= limits.max_conflict_retries() {
                    return Err(MutationRunnerError::ConflictExhausted(conflict));
                }
                semantic_attempt = semantic_attempt
                    .checked_add(1)
                    .expect("validated retry bound fits u32");
            }
            CommitOutcome::Ambiguous(_) => {
                let mut resolved_not_committed = false;
                for ambiguity_attempt in 0..limits.max_ambiguity_attempts() {
                    validate_planned_commit(&request, filesystem_id, mutation, fence)
                        .map_err(MutationRunnerError::PlannedCommit)?;
                    let retried = match state.commit(request.clone()).await {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            if ambiguity_attempt + 1 == limits.max_ambiguity_attempts() {
                                return Err(MutationRunnerError::State(error));
                            }
                            continue;
                        }
                    };
                    match retried {
                        CommitOutcome::Committed(committed)
                        | CommitOutcome::AlreadyCommitted(committed) => {
                            return decode_committed(committed.result, expected_result, limits)
                                .map_err(MutationRunnerError::ResultCodec);
                        }
                        CommitOutcome::Ambiguous(_) => {}
                        CommitOutcome::Conflict(_) => {
                            resolved_not_committed = true;
                            break;
                        }
                        CommitOutcome::MutationMismatch(mismatch) => {
                            return Err(MutationRunnerError::MutationMismatch(mismatch));
                        }
                        CommitOutcome::StaleFence => {
                            return Err(MutationRunnerError::Authority(
                                AuthorityFailure::StaleFence,
                            ));
                        }
                        CommitOutcome::ExpiredLease => {
                            return Err(MutationRunnerError::Authority(
                                AuthorityFailure::ExpiredLease,
                            ));
                        }
                        CommitOutcome::MalformedRequest(error) => {
                            return Err(MutationRunnerError::MalformedCommit(error));
                        }
                    }
                }
                if !resolved_not_committed {
                    return Err(MutationRunnerError::Authority(
                        AuthorityFailure::AmbiguityResolutionExhausted { mutation },
                    ));
                }
                if semantic_attempt >= limits.max_conflict_retries() {
                    return Err(MutationRunnerError::Authority(
                        AuthorityFailure::AmbiguityResolutionExhausted { mutation },
                    ));
                }
                semantic_attempt = semantic_attempt
                    .checked_add(1)
                    .expect("validated retry bound fits u32");
            }
        }
    }
}

fn validate_planned_commit(
    request: &CommitRequest,
    filesystem_id: FilesystemId,
    mutation: MutationContext,
    fence: WriterFence,
) -> Result<(), PlannedCommitMismatch> {
    if request.filesystem_id() != filesystem_id {
        return Err(PlannedCommitMismatch::FilesystemId);
    }
    if request.mutation() != mutation {
        return Err(PlannedCommitMismatch::MutationContext);
    }
    if request.fence() != fence {
        return Err(PlannedCommitMismatch::WriterFence);
    }
    Ok(())
}

fn decode_committed(
    result: w9pt_fs_state::MutationResult,
    expected: FilesystemResultKind,
    limits: EngineLimits,
) -> Result<FilesystemResult, ResultCodecError> {
    let decoded = decode_mutation_result(&result, limits)?;
    if decoded.kind() == expected {
        Ok(decoded)
    } else {
        Err(ResultCodecError::UnsupportedResult)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt::filesystem::OpenResult;
    use w9pt_fs_state::{
        AcquireLeaseOutcome, AcquireWriterLease, ClientIncarnationId, FencingToken, LeaseDeadline,
        LeaseDuration, LeaseId, LeaseOperationId, ManualLeaseClock, MutationResultKind,
        MutationRetention, RecordRevision, ResultFormatVersion, StateChange, StateLimits,
        StateRevision, StateStoreOperation, WriterIncarnationId, WriterScopeId, WriterTopology,
        testing::{MemoryAuthority, MemoryTracePhase},
    };
    use w9pt_fs_storage::MutationId;

    fn context() -> MutationContext {
        MutationContext::new(
            MutationId::from_u128(1),
            w9pt_fs_state::RequestFingerprint::blake3(b"request"),
            ClientIncarnationId::from_u128(2),
            MutationRetention::new(3),
        )
    }

    fn fence() -> WriterFence {
        WriterFence::new(
            WriterScopeId::from_u128(5),
            WriterIncarnationId::from_u128(6),
            LeaseId::from_u128(7),
            FencingToken::new(8).unwrap(),
        )
    }

    fn planned_request(
        filesystem_id: FilesystemId,
        mutation: MutationContext,
        fence: WriterFence,
    ) -> CommitRequest {
        CommitRequest::new(
            filesystem_id,
            mutation,
            fence,
            Vec::new(),
            vec![StateChange::BumpFilesystemPolicyGeneration],
            crate::encode_mutation_result(
                &FilesystemResult::Released,
                EngineLimits::default(),
                StateLimits::default(),
            )
            .unwrap(),
            StateLimits::default(),
        )
        .unwrap()
    }

    fn record(result: FilesystemResult) -> MutationRecord {
        let context = context();
        let result =
            crate::encode_mutation_result(&result, EngineLimits::default(), StateLimits::default())
                .unwrap();
        MutationRecord::new(
            FilesystemId::from_u128(4),
            context.mutation_id,
            context.fingerprint,
            context.client_incarnation,
            WriterScopeId::from_u128(5),
            WriterIncarnationId::from_u128(6),
            FencingToken::new(7).unwrap(),
            result,
            StateRevision::new(8).unwrap(),
            context.retention,
            RecordRevision::new(8).unwrap(),
        )
    }

    #[test]
    fn exact_replay_reconstructs_values_and_mismatch_stops() {
        let result = FilesystemResult::Opened(OpenResult {
            qid: w9pt::Qid::new(w9pt::protocol::QidType::FILE, 0, 9),
            open: w9pt::filesystem::OpenHandle::new(10),
            io_unit: 11,
        });
        let record = record(result.clone());
        assert_eq!(
            classify_retained_mutation::<core::convert::Infallible>(
                context(),
                Some(&record),
                FilesystemResultKind::Opened,
                EngineLimits::default(),
            )
            .unwrap(),
            LedgerReplay::Exact(result)
        );
        let changed = MutationContext {
            fingerprint: w9pt_fs_state::RequestFingerprint::blake3(b"changed"),
            ..context()
        };
        assert!(matches!(
            classify_retained_mutation::<core::convert::Infallible>(
                changed,
                Some(&record),
                FilesystemResultKind::Opened,
                EngineLimits::default(),
            ),
            Err(LedgerProbeError::MutationMismatch(
                MutationMismatch::Fingerprint
            ))
        ));
    }

    #[test]
    fn malformed_or_wrong_kind_replay_never_invents_a_result() {
        let record = record(FilesystemResult::Written(1));
        assert!(matches!(
            classify_retained_mutation::<core::convert::Infallible>(
                context(),
                Some(&record),
                FilesystemResultKind::Opened,
                EngineLimits::default(),
            ),
            Err(LedgerProbeError::ResultKindMismatch { .. })
        ));
        let context = context();
        let malformed = MutationRecord::new(
            FilesystemId::from_u128(4),
            context.mutation_id,
            context.fingerprint,
            context.client_incarnation,
            WriterScopeId::from_u128(5),
            WriterIncarnationId::from_u128(6),
            FencingToken::new(7).unwrap(),
            w9pt_fs_state::MutationResult::new(
                MutationResultKind::new(6).unwrap(),
                ResultFormatVersion::new(1).unwrap(),
                vec![1],
                StateLimits::default(),
            )
            .unwrap(),
            StateRevision::new(8).unwrap(),
            context.retention,
            RecordRevision::new(8).unwrap(),
        );
        assert!(matches!(
            classify_retained_mutation::<core::convert::Infallible>(
                context,
                Some(&malformed),
                FilesystemResultKind::Written,
                EngineLimits::default(),
            ),
            Err(LedgerProbeError::ResultCodec(_))
        ));
    }

    #[test]
    fn planner_cannot_change_runner_owned_commit_authority() {
        let expected_filesystem = FilesystemId::from_u128(4);
        let expected_mutation = context();
        let expected_fence = fence();
        let changed_mutation = MutationContext {
            fingerprint: w9pt_fs_state::RequestFingerprint::blake3(b"other-request"),
            ..expected_mutation
        };
        let changed_fence = WriterFence::new(
            expected_fence.scope,
            expected_fence.holder,
            expected_fence.lease_id,
            FencingToken::new(9).unwrap(),
        );
        let cases = [
            (
                planned_request(
                    FilesystemId::from_u128(40),
                    expected_mutation,
                    expected_fence,
                ),
                PlannedCommitMismatch::FilesystemId,
            ),
            (
                planned_request(expected_filesystem, changed_mutation, expected_fence),
                PlannedCommitMismatch::MutationContext,
            ),
            (
                planned_request(expected_filesystem, expected_mutation, changed_fence),
                PlannedCommitMismatch::WriterFence,
            ),
        ];

        for (request, expected_error) in cases {
            let authority = MemoryAuthority::new(
                WriterTopology::SerializableMultiWriter,
                StateLimits::default(),
                ManualLeaseClock::new(LeaseDeadline::new(0)),
            );
            let client = authority.open_client();
            let outcome = w9pt_fs_storage::testing::block_on(run_mutation(
                &client,
                expected_filesystem,
                expected_mutation,
                expected_fence,
                FilesystemResultKind::Released,
                EngineLimits::default(),
                |_| core::future::ready(Ok::<_, core::convert::Infallible>(request.clone())),
            ));
            match outcome {
                Err(MutationRunnerError::PlannedCommit(actual)) => {
                    assert_eq!(actual, expected_error);
                }
                other => panic!("expected planned-commit mismatch, got {other:?}"),
            }
            assert!(
                authority
                    .trace()
                    .unwrap()
                    .iter()
                    .all(|event| event.operation != StateStoreOperation::Commit),
                "planner mismatch reached the state commit boundary"
            );
        }
    }

    #[test]
    fn definitive_conflict_stops_at_the_configured_bound() {
        let filesystem_id = FilesystemId::from_u128(4);
        let authority = MemoryAuthority::new(
            WriterTopology::SerializableMultiWriter,
            StateLimits::default(),
            ManualLeaseClock::new(LeaseDeadline::new(0)),
        );
        let client = authority.open_client();
        let acquire = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(20),
            WriterScopeId::from_u128(5),
            WriterIncarnationId::from_u128(6),
            LeaseId::from_u128(7),
            LeaseDuration::new(100).unwrap(),
            StateLimits::default(),
        )
        .unwrap();
        let AcquireLeaseOutcome::Granted(grant) =
            w9pt_fs_storage::testing::block_on(client.acquire_writer_lease(acquire)).unwrap()
        else {
            panic!("lease not granted")
        };
        let limits = EngineLimits::new(crate::EngineLimitValues {
            max_conflict_retries: 0,
            ..crate::EngineLimitValues::default()
        })
        .unwrap();
        let mutation = context();
        let request = planned_request(filesystem_id, mutation, grant.fence);
        let outcome = w9pt_fs_storage::testing::block_on(run_mutation(
            &client,
            filesystem_id,
            mutation,
            grant.fence,
            FilesystemResultKind::Released,
            limits,
            |_| core::future::ready(Ok::<_, core::convert::Infallible>(request.clone())),
        ));
        assert!(matches!(
            outcome,
            Err(MutationRunnerError::ConflictExhausted(_))
        ));
        assert_eq!(
            authority
                .trace()
                .unwrap()
                .iter()
                .filter(|event| {
                    event.operation == StateStoreOperation::Commit
                        && event.phase == MemoryTracePhase::Started
                })
                .count(),
            1
        );
    }
}
