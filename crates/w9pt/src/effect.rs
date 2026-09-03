//! Owned effects emitted by a session and completions supplied by its host.
//!
//! Filesystem and policy effects transfer ownership of their payload to the host and may be
//! scheduled in any order. Each requires exactly one matching terminal completion. A [`Effect::Cancel`]
//! is advisory and does not replace that obligation or imply mutation rollback. Complete response
//! frames retain the polling order the host must preserve on one transport connection.

use crate::{
    error::CloseReason,
    filesystem::{
        AttachResult, FilesystemError, FilesystemRequest, FilesystemResult, FilesystemResultKind,
        LinuxErrno,
    },
    protocol::{Fid, OperationId, Qid, UserIdentity},
};

/// Opaque host-owned authentication exchange handle.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AuthHandle(u128);

impl AuthHandle {
    /// Constructs a handle from a host-assigned value.
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    /// Returns the opaque value for routing to its originating policy engine.
    pub const fn get(self) -> u128 {
        self.0
    }
}

/// Typed external work emitted by a session.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum Effect {
    /// Complete protocol response frame. Hosts preserve polling order for one session.
    SendFrame { bytes: Vec<u8> },
    /// One backend-neutral filesystem operation.
    Filesystem {
        operation_id: OperationId,
        request: FilesystemRequest,
    },
    /// One authentication/export-policy operation.
    Policy {
        operation_id: OperationId,
        request: PolicyRequest,
    },
    /// Best-effort cancellation of active work; this never promises transaction rollback.
    Cancel {
        operation_id: OperationId,
        kind: CancelKind,
    },
    /// Terminal request to close the associated transport.
    CloseSession { reason: CloseReason },
}

impl Effect {
    /// Returns payload bytes retained by this effect for queue accounting.
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::SendFrame { bytes } => bytes.len(),
            Self::Filesystem { request, .. } => request.retained_bytes(),
            Self::Policy { request, .. } => request.retained_bytes(),
            Self::Cancel { .. } | Self::CloseSession { .. } => 0,
        }
    }
}

/// Boundary on which an operation is currently executing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelKind {
    /// Backend-neutral filesystem operation.
    Filesystem,
    /// Host authentication or export-policy operation.
    Policy,
}

/// Host-supplied terminal event for one previously emitted operation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum Completion {
    /// Filesystem result or stable semantic error.
    Filesystem {
        operation_id: OperationId,
        result: Result<FilesystemResult, FilesystemError>,
    },
    /// Policy/authentication result or selected client-visible error.
    Policy {
        operation_id: OperationId,
        result: Result<PolicyResult, PolicyError>,
    },
    /// Work stopped without a protocol result. This is terminal for the original operation.
    Cancelled {
        operation_id: OperationId,
        kind: CancelKind,
    },
}

impl Completion {
    /// Returns the originating session-local operation identifier.
    pub const fn operation_id(&self) -> OperationId {
        match self {
            Self::Filesystem { operation_id, .. }
            | Self::Policy { operation_id, .. }
            | Self::Cancelled { operation_id, .. } => *operation_id,
        }
    }

    /// Returns the exact terminal result kind presented by this completion.
    pub const fn kind(&self) -> CompletionKind {
        match self {
            Self::Filesystem {
                result: Ok(result), ..
            } => CompletionKind::Filesystem(result.kind()),
            Self::Filesystem { result: Err(_), .. } => CompletionKind::FilesystemError,
            Self::Policy {
                result: Ok(result), ..
            } => CompletionKind::Policy(result.kind()),
            Self::Policy { result: Err(_), .. } => CompletionKind::PolicyError,
            Self::Cancelled { kind, .. } => CompletionKind::Cancelled(*kind),
        }
    }
}

/// Exact kind of terminal value supplied for host-completion validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionKind {
    /// Successful filesystem result variant.
    Filesystem(FilesystemResultKind),
    /// Filesystem semantic error, valid for any filesystem request.
    FilesystemError,
    /// Successful policy result variant.
    Policy(PolicyResultKind),
    /// Policy error, valid for any policy request.
    PolicyError,
    /// Terminal cancellation for work on the named boundary.
    Cancelled(CancelKind),
}

/// Host-owned authentication and export-policy work.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum PolicyRequest {
    /// Start a protocol authentication exchange and bind `afid` on success.
    StartAuth {
        afid: Fid,
        identity: UserIdentity,
        export_name: String,
        transport_principal: Option<String>,
    },
    /// Read opaque authentication exchange bytes.
    ReadAuth {
        handle: AuthHandle,
        offset: u64,
        count: u32,
    },
    /// Write opaque authentication exchange bytes.
    WriteAuth {
        handle: AuthHandle,
        offset: u64,
        data: Vec<u8>,
    },
    /// Retire an authentication handle.
    ClunkAuth { handle: AuthHandle },
    /// Authorize attachment and select a principal/export/root/capability contract.
    Attach {
        fid: Fid,
        auth: Option<AuthHandle>,
        identity: UserIdentity,
        export_name: String,
        transport_principal: Option<String>,
    },
}

impl PolicyRequest {
    /// Exact successful completion variant required by this request.
    pub const fn expected_result(&self) -> PolicyResultKind {
        match self {
            Self::StartAuth { .. } => PolicyResultKind::AuthStarted,
            Self::ReadAuth { .. } => PolicyResultKind::AuthRead,
            Self::WriteAuth { .. } => PolicyResultKind::AuthWritten,
            Self::ClunkAuth { .. } => PolicyResultKind::AuthClunked,
            Self::Attach { .. } => PolicyResultKind::Attached,
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::StartAuth {
                identity,
                export_name,
                transport_principal,
                ..
            }
            | Self::Attach {
                identity,
                export_name,
                transport_principal,
                ..
            } => identity
                .name
                .len()
                .saturating_add(export_name.len())
                .saturating_add(transport_principal.as_ref().map_or(0, String::len)),
            Self::WriteAuth { data, .. } => data.len(),
            Self::ReadAuth { .. } | Self::ClunkAuth { .. } => 0,
        }
    }
}

/// Exact successful result kinds for [`PolicyRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum PolicyResult {
    /// Authentication exchange created.
    AuthStarted { qid: Qid, handle: AuthHandle },
    /// Authentication bytes read.
    AuthRead { data: Vec<u8> },
    /// Authentication bytes accepted.
    AuthWritten { count: u32 },
    /// Authentication handle retired.
    AuthClunked,
    /// Attach authorized and export contract installed.
    Attached(AttachResult),
}

impl PolicyResult {
    /// Returns the exact variant kind without inspecting its owned payload.
    pub const fn kind(&self) -> PolicyResultKind {
        match self {
            Self::AuthStarted { .. } => PolicyResultKind::AuthStarted,
            Self::AuthRead { .. } => PolicyResultKind::AuthRead,
            Self::AuthWritten { .. } => PolicyResultKind::AuthWritten,
            Self::AuthClunked => PolicyResultKind::AuthClunked,
            Self::Attached(_) => PolicyResultKind::Attached,
        }
    }
}

/// Discriminant used to validate policy completions exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyResultKind {
    /// Result of `StartAuth`.
    AuthStarted,
    /// Result of `ReadAuth`.
    AuthRead,
    /// Result of `WriteAuth`.
    AuthWritten,
    /// Result of `ClunkAuth`.
    AuthClunked,
    /// Result of `Attach`.
    Attached,
}

/// Policy-selected failure safe to expose as an `Rlerror` errno.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyError {
    /// Stable target-independent Linux errno.
    pub errno: LinuxErrno,
}

impl PolicyError {
    /// Constructs a policy error.
    pub const fn new(errno: LinuxErrno) -> Self {
        Self { errno }
    }
}

/// Checked monotonic allocator that never reuses an ID within a session.
#[derive(Clone, Debug)]
pub(crate) struct OperationIdAllocator {
    next: Option<u64>,
}

impl OperationIdAllocator {
    pub const fn new() -> Self {
        Self { next: Some(1) }
    }

    pub fn allocate(&mut self) -> Result<OperationId, crate::error::SessionError> {
        let value = self
            .next
            .ok_or(crate::error::SessionError::OperationIdExhausted)?;
        self.next = value.checked_add(1);
        Ok(OperationId::new(value))
    }

    #[cfg(test)]
    const fn starting_at(value: u64) -> Self {
        Self { next: Some(value) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_are_monotonic_and_never_wrap() {
        let mut allocator = OperationIdAllocator::new();
        assert_eq!(allocator.allocate().unwrap().get(), 1);
        assert_eq!(allocator.allocate().unwrap().get(), 2);

        let mut allocator = OperationIdAllocator::starting_at(u64::MAX);
        assert_eq!(allocator.allocate().unwrap().get(), u64::MAX);
        assert_eq!(
            allocator.allocate(),
            Err(crate::error::SessionError::OperationIdExhausted)
        );
    }
}
