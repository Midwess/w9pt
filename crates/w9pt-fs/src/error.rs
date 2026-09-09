//! Stable semantic and typed unresolved-execution failures.

use w9pt_fs_state::MutationContext;

use w9pt::{FilesystemError, LinuxErrno};

use crate::{AuthorizationError, HandleResolutionError, LedgerProbeError, MutationRunnerError};

/// Caller-owned policy or deterministic identity provider failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineProviderFailure<P, I> {
    /// Export or numeric-identity policy failed.
    Policy(P),
    /// Deterministic identity allocation failed.
    Identity(I),
}

impl<P: core::fmt::Display, I: core::fmt::Display> core::fmt::Display
    for EngineProviderFailure<P, I>
{
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Policy(error) => write!(formatter, "policy provider failed: {error}"),
            Self::Identity(error) => write!(formatter, "identity provider failed: {error}"),
        }
    }
}

impl<P, I> std::error::Error for EngineProviderFailure<P, I>
where
    P: std::error::Error + 'static,
    I: std::error::Error + 'static,
{
}

/// Missing or contradictory caller-owned execution context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionContextError {
    /// A state-changing operation omitted its globally stable mutation identity.
    MissingMutationId,
    /// A state-changing operation omitted its exact current writer fence.
    MissingWriterFence,
    /// A state-changing operation omitted its frozen logical timestamp.
    MissingTimestamp,
    /// Read-only work was routed into a state-publication path.
    ReadOnlyPublication,
}

/// Authoritative status that cannot safely be guessed or mapped to a client response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityFailure {
    /// The presented writer fence no longer owns mutation authority.
    StaleFence,
    /// The presented writer lease expired under adapter-authoritative time.
    ExpiredLease,
    /// The exact commit may have applied and must be replayed unchanged.
    AmbiguousCommit {
        /// Durable mutation identity and fingerprint required for exact replay.
        mutation: MutationContext,
    },
    /// Exact ambiguity resolution exhausted its configured bound.
    AmbiguityResolutionExhausted {
        /// Durable mutation identity and fingerprint that remains unresolved.
        mutation: MutationContext,
    },
}

/// Typed failure that remains outside the client-safe terminal boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionFailure<S, T, P> {
    /// Authoritative state adapter failed without a client-safe semantic result.
    State(S),
    /// Immutable content target failed without a client-safe semantic result.
    Target(T),
    /// Caller-owned export or identity policy failed to resolve.
    Policy(P),
    /// Required durable identity, fence, or timestamp was absent.
    Context(ExecutionContextError),
    /// Commit authority remains unresolved and requires host action or exact retry.
    Authority(AuthorityFailure),
}

/// Maps a definitive authorization denial to its stable client-visible errno.
pub const fn authorization_client_error(error: AuthorizationError) -> FilesystemError {
    let errno = match error {
        AuthorizationError::PermissionDenied => LinuxErrno::EACCES,
        AuthorizationError::OperationNotPermitted => LinuxErrno::EPERM,
        AuthorizationError::NotDirectory => LinuxErrno::ENOTDIR,
        AuthorizationError::ReadOnlyExport => LinuxErrno::EROFS,
        AuthorizationError::InvalidMode => LinuxErrno::EINVAL,
    };
    FilesystemError::new(errno)
}

/// Maps a definitive portable-handle failure without exposing adapter diagnostics.
pub const fn handle_client_error(error: HandleResolutionError) -> FilesystemError {
    let errno = match error {
        HandleResolutionError::UnknownObject
        | HandleResolutionError::UnknownOpen
        | HandleResolutionError::IdentityMismatch
        | HandleResolutionError::ClientMismatch => LinuxErrno::EBADF,
        HandleResolutionError::ExportMismatch => LinuxErrno::EXDEV,
        HandleResolutionError::MalformedRecordKind => LinuxErrno::EIO,
    };
    FilesystemError::new(errno)
}

/// Returns a client-safe error only when the runner proved a definitive terminal failure.
pub const fn runner_client_error<S, P>(
    error: &MutationRunnerError<S, P>,
) -> Option<FilesystemError> {
    let errno = match error {
        MutationRunnerError::ConflictExhausted(_) => LinuxErrno::EAGAIN,
        MutationRunnerError::MutationMismatch(_)
        | MutationRunnerError::PlannedCommit(_)
        | MutationRunnerError::MalformedCommit(_)
        | MutationRunnerError::ResultCodec(_)
        | MutationRunnerError::Ledger(LedgerProbeError::MutationMismatch(_))
        | MutationRunnerError::Ledger(LedgerProbeError::ResultCodec(_))
        | MutationRunnerError::Ledger(LedgerProbeError::ResultKindMismatch { .. }) => {
            LinuxErrno::EIO
        }
        MutationRunnerError::Ledger(_)
        | MutationRunnerError::Plan(_)
        | MutationRunnerError::State(_)
        | MutationRunnerError::Authority(_) => return None,
    };
    Some(FilesystemError::new(errno))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitive_semantic_failures_have_stable_errno_mappings() {
        assert_eq!(
            authorization_client_error(AuthorizationError::ReadOnlyExport).errno,
            LinuxErrno::EROFS
        );
        assert_eq!(
            handle_client_error(HandleResolutionError::UnknownOpen).errno,
            LinuxErrno::EBADF
        );
    }
}
