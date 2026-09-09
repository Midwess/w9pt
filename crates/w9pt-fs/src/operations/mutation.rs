//! Shared mutation-planning and runner error boundary.

use w9pt::{FilesystemError, LinuxErrno};
use w9pt_fs_state::{MalformedCommit, StateLimitError};
use w9pt_fs_storage::StorageError;

use crate::{
    AuthorityFailure, ExecutionContextError, FingerprintError, LedgerProbeError,
    MutationRunnerError, ResultCodecError,
};

/// Failure while constructing one fresh semantic mutation plan.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum MutationPlanError<S, T, P, I> {
    Client(FilesystemError),
    State(S),
    Target(StorageError<T>),
    Policy(P),
    Identity(I),
    MalformedRead(StateLimitError),
    MalformedCommit(MalformedCommit),
    ResultCodec(ResultCodecError),
    MalformedState,
}

/// Classified operation failure used by the public engine outcome boundary.
#[derive(Debug)]
pub(crate) enum MutationOperationError<S, T, P, I> {
    Client(FilesystemError),
    State(S),
    Target(StorageError<T>),
    Policy(P),
    Identity(I),
    Context(ExecutionContextError),
    Authority(AuthorityFailure),
    Internal,
}

impl<S, T, P, I> From<MutationPlanError<S, T, P, I>> for MutationOperationError<S, T, P, I> {
    fn from(error: MutationPlanError<S, T, P, I>) -> Self {
        match error {
            MutationPlanError::Client(error) => Self::Client(error),
            MutationPlanError::State(error) => Self::State(error),
            MutationPlanError::Target(error) => Self::Target(error),
            MutationPlanError::Policy(error) => Self::Policy(error),
            MutationPlanError::Identity(error) => Self::Identity(error),
            MutationPlanError::MalformedRead(error) => {
                let _ = error;
                Self::Internal
            }
            MutationPlanError::MalformedCommit(error) => {
                let _ = error;
                Self::Internal
            }
            MutationPlanError::ResultCodec(error) => {
                let _ = error;
                Self::Internal
            }
            MutationPlanError::MalformedState => Self::Internal,
        }
    }
}

pub(crate) fn fingerprint_error<S, T, P, I>(
    error: FingerprintError,
) -> MutationOperationError<S, T, P, I> {
    match error {
        FingerprintError::Context(error) => MutationOperationError::Context(error),
        FingerprintError::Limit(_) | FingerprintError::Arithmetic => {
            MutationOperationError::Client(FilesystemError::new(LinuxErrno::ENOMEM))
        }
        FingerprintError::NotFirstSliceMutation => MutationOperationError::Internal,
    }
}

pub(crate) fn runner_error<S, T, P, I>(
    error: MutationRunnerError<S, MutationPlanError<S, T, P, I>>,
) -> MutationOperationError<S, T, P, I> {
    match error {
        MutationRunnerError::Plan(error) => error.into(),
        MutationRunnerError::State(error) => MutationOperationError::State(error),
        MutationRunnerError::Authority(error) => MutationOperationError::Authority(error),
        MutationRunnerError::ConflictExhausted(_) => {
            MutationOperationError::Client(FilesystemError::new(LinuxErrno::EAGAIN))
        }
        MutationRunnerError::Ledger(LedgerProbeError::State(error)) => {
            MutationOperationError::State(error)
        }
        MutationRunnerError::Ledger(_)
        | MutationRunnerError::PlannedCommit(_)
        | MutationRunnerError::MutationMismatch(_)
        | MutationRunnerError::MalformedCommit(_)
        | MutationRunnerError::ResultCodec(_) => MutationOperationError::Internal,
    }
}

pub(crate) const fn client<S, T, P, I>(errno: LinuxErrno) -> MutationPlanError<S, T, P, I> {
    MutationPlanError::Client(FilesystemError::new(errno))
}
