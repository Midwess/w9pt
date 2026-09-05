//! Owned execution identity, authority, request, and outcome values.

use w9pt::{FilesystemError, OperationId, filesystem::FilesystemRequest};
use w9pt_fs_state::{ClientIncarnationId, MutationRetention, UnixTimestamp, WriterFence};
use w9pt_fs_storage::MutationId;

use crate::ExecutionFailure;

/// Caller-supplied stable identity, time, retention, and writer authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionContext {
    /// Globally stable client/session incarnation.
    pub client_incarnation: ClientIncarnationId,
    /// Globally stable mutation identity, required for state-changing work.
    pub mutation_id: Option<MutationId>,
    /// Caller-owned retention horizon for an idempotent mutation result.
    pub retention: MutationRetention,
    /// Exact current writer authority, required for state-changing work.
    pub fence: Option<WriterFence>,
    /// Frozen logical-operation timestamp, required for state-changing work.
    pub timestamp: Option<UnixTimestamp>,
}

impl ExecutionContext {
    /// Constructs an explicit execution context without consulting globals.
    pub const fn new(
        client_incarnation: ClientIncarnationId,
        mutation_id: Option<MutationId>,
        retention: MutationRetention,
        fence: Option<WriterFence>,
        timestamp: Option<UnixTimestamp>,
    ) -> Self {
        Self {
            client_incarnation,
            mutation_id,
            retention,
            fence,
            timestamp,
        }
    }

    /// Constructs a read-only context with no mutation authority.
    pub const fn read_only(
        client_incarnation: ClientIncarnationId,
        retention: MutationRetention,
    ) -> Self {
        Self::new(client_incarnation, None, retention, None, None)
    }
}

/// One owned filesystem effect payload plus durable execution context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineRequest {
    /// Session-local operation identity retained only for completion routing.
    pub operation_id: OperationId,
    /// Owned attach-bound filesystem request emitted by `w9pt`.
    pub request: FilesystemRequest,
    /// Caller-supplied durable execution identity and authority.
    pub execution: ExecutionContext,
}

impl EngineRequest {
    /// Constructs an engine request from one emitted filesystem effect.
    pub const fn new(
        operation_id: OperationId,
        request: FilesystemRequest,
        execution: ExecutionContext,
    ) -> Self {
        Self {
            operation_id,
            request,
            execution,
        }
    }
}

/// Definitive client-safe result that may complete the originating session operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineTerminal {
    /// Session-local routing identity from the original effect.
    pub operation_id: OperationId,
    /// Exact filesystem result kind or stable client-visible error.
    pub result: Result<w9pt::filesystem::FilesystemResult, FilesystemError>,
}

impl EngineTerminal {
    /// Converts this terminal value into the exact `w9pt` completion envelope.
    pub fn into_completion(self) -> w9pt::Completion {
        w9pt::Completion::Filesystem {
            operation_id: self.operation_id,
            result: self.result,
        }
    }
}

/// Unresolved infrastructure or authority failure that must not complete the session yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedEngineFailure<S, T, P> {
    /// Session-local routing identity retained across exact retry.
    pub operation_id: OperationId,
    /// Typed source and required recovery class.
    pub failure: ExecutionFailure<S, T, P>,
}

/// Outcome of one semantic-engine execution attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineOutcome<S, T, P> {
    /// A definitive value safe to submit exactly once to `Session::complete`.
    Terminal(EngineTerminal),
    /// Work whose authoritative outcome remains unresolved.
    Unresolved(UnresolvedEngineFailure<S, T, P>),
}

impl<S, T, P> EngineOutcome<S, T, P> {
    /// Returns the originating session-local operation identity.
    pub const fn operation_id(&self) -> OperationId {
        match self {
            Self::Terminal(terminal) => terminal.operation_id,
            Self::Unresolved(unresolved) => unresolved.operation_id,
        }
    }
}
