//! Writer-fence and lease protocol values.

use core::fmt;
use std::sync::{Arc, Mutex};

use crate::{
    FencingToken, FilesystemId, LeaseDeadline, LeaseDuration, LeaseId, LeaseOperationId,
    RecordRevision, RequestFingerprint, StateLimitError, StateLimitKind, StateLimits,
    WriterIncarnationId, WriterLeaseRecord, WriterScopeId, WriterTopology,
};

/// Exact portable authority presented by every non-replayed writer commit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WriterFence {
    /// Protected semantic scope.
    pub scope: WriterScopeId,
    /// Current writer incarnation.
    pub holder: WriterIncarnationId,
    /// Current grant identity.
    pub lease_id: LeaseId,
    /// Monotonically allocated token.
    pub fencing_token: FencingToken,
}

impl WriterFence {
    /// Creates the exact portable authority returned by a lease grant.
    pub const fn new(
        scope: WriterScopeId,
        holder: WriterIncarnationId,
        lease_id: LeaseId,
        fencing_token: FencingToken,
    ) -> Self {
        Self {
            scope,
            holder,
            lease_id,
            fencing_token,
        }
    }
}

/// Result of comparing a presented writer fence with current authoritative lease state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FenceValidation {
    /// Every identity matches and the exact lease remains unexpired.
    Current,
    /// Lease is absent or any scope, holder, identity, or token differs.
    Stale,
    /// Exact current lease reached its adapter-authoritative deadline.
    Expired,
}

/// Validates an exact writer authority using only adapter-supplied authoritative time.
pub fn validate_writer_fence(
    filesystem_id: FilesystemId,
    presented: WriterFence,
    current: Option<&WriterLeaseRecord>,
    now: LeaseDeadline,
) -> FenceValidation {
    let Some(current) = current else {
        return FenceValidation::Stale;
    };
    if current.filesystem_id() != filesystem_id
        || current.scope() != presented.scope
        || current.holder() != presented.holder
        || current.lease_id() != presented.lease_id
        || current.fencing_token() != presented.fencing_token
    {
        return FenceValidation::Stale;
    }
    if now >= current.deadline() {
        FenceValidation::Expired
    } else {
        FenceValidation::Current
    }
}

/// Allocates a token strictly greater than every prior grant in one scope.
pub fn next_fencing_token(
    last_granted: Option<FencingToken>,
) -> Result<FencingToken, crate::CounterOverflow> {
    match last_granted {
        Some(token) => token.checked_next(),
        None => Ok(FencingToken::new(1).expect("one is a valid fencing token")),
    }
}

/// Applies a new-grant transition after operation-id replay has been checked.
pub fn grant_writer_lease(
    request: AcquireWriterLease,
    current: Option<&WriterLeaseRecord>,
    last_granted: Option<FencingToken>,
    now: LeaseDeadline,
    topology: WriterTopology,
    revision: RecordRevision,
) -> Result<WriterLeaseRecord, LeaseRejection> {
    validate_writer_scope(topology, request.scope())?;
    if let Some(current) = current
        && now < current.deadline()
    {
        return Err(LeaseRejection::Busy(grant_from_record(current)));
    }
    let previous = current
        .map(WriterLeaseRecord::fencing_token)
        .into_iter()
        .chain(last_granted)
        .max();
    let fencing_token =
        next_fencing_token(previous).map_err(|_| LeaseRejection::FencingTokenExhausted)?;
    let deadline = now
        .checked_add(request.duration())
        .map_err(|_| LeaseRejection::InvalidDuration)?;
    Ok(WriterLeaseRecord::new(
        request.filesystem_id(),
        request.scope(),
        request.holder(),
        request.lease_id(),
        deadline,
        fencing_token,
        revision,
    ))
}

/// Applies a renewal that preserves the exact current fencing token.
pub fn renew_current_lease(
    request: RenewWriterLease,
    current: Option<&WriterLeaseRecord>,
    now: LeaseDeadline,
    topology: WriterTopology,
    revision: RecordRevision,
) -> Result<WriterLeaseRecord, LeaseRejection> {
    validate_writer_scope(topology, request.fence().scope)?;
    match validate_writer_fence(request.filesystem_id(), request.fence(), current, now) {
        FenceValidation::Current => {}
        FenceValidation::Stale => return Err(LeaseRejection::StaleFence),
        FenceValidation::Expired => return Err(LeaseRejection::Expired),
    }
    let requested_deadline = now
        .checked_add(request.duration())
        .map_err(|_| LeaseRejection::InvalidDuration)?;
    let current_deadline = current
        .expect("current fence validation succeeded")
        .deadline();
    let deadline = requested_deadline.max(current_deadline);
    let fence = request.fence();
    Ok(WriterLeaseRecord::new(
        request.filesystem_id(),
        fence.scope,
        fence.holder,
        fence.lease_id,
        deadline,
        fence.fencing_token,
        revision,
    ))
}

/// Validates release of the exact current unexpired lease.
pub fn validate_lease_release(
    request: ReleaseWriterLease,
    current: Option<&WriterLeaseRecord>,
    now: LeaseDeadline,
    topology: WriterTopology,
) -> Result<(), LeaseRejection> {
    validate_writer_scope(topology, request.fence().scope)?;
    match validate_writer_fence(request.filesystem_id(), request.fence(), current, now) {
        FenceValidation::Current => Ok(()),
        FenceValidation::Stale => Err(LeaseRejection::StaleFence),
        FenceValidation::Expired => Err(LeaseRejection::Expired),
    }
}

fn validate_writer_scope(
    topology: WriterTopology,
    scope: WriterScopeId,
) -> Result<(), LeaseRejection> {
    if matches!(topology, WriterTopology::SingleFencedWriter) && scope != WriterScopeId::FILESYSTEM
    {
        Err(LeaseRejection::InvalidWriterScope)
    } else {
        Ok(())
    }
}

fn grant_from_record(record: &WriterLeaseRecord) -> WriterLeaseGrant {
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

/// Caller-owned source of adapter-authoritative lease time.
pub trait LeaseTimeAuthority: Send + Sync {
    /// Time-source failure type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Reads the current authoritative deadline tick without using a global clock.
    fn now(&self) -> Result<LeaseDeadline, Self::Error>;
}

/// Deterministic monotonic lease clock shared by independently opened test clients.
#[derive(Clone, Debug)]
pub struct ManualLeaseClock {
    ticks: Arc<Mutex<u64>>,
}

impl ManualLeaseClock {
    /// Creates a manual clock at an explicit starting tick.
    pub fn new(now: LeaseDeadline) -> Self {
        Self {
            ticks: Arc::new(Mutex::new(now.ticks())),
        }
    }

    /// Advances authoritative time with checked arithmetic.
    pub fn advance(&self, ticks: u64) -> Result<LeaseDeadline, ManualClockError> {
        let mut now = self.lock()?;
        *now = now.checked_add(ticks).ok_or(ManualClockError::Overflow)?;
        Ok(LeaseDeadline::new(*now))
    }

    /// Moves time to an explicit later or equal tick, never backward.
    pub fn advance_to(&self, deadline: LeaseDeadline) -> Result<(), ManualClockError> {
        let mut now = self.lock()?;
        if deadline.ticks() < *now {
            return Err(ManualClockError::WouldMoveBackward {
                current: *now,
                requested: deadline.ticks(),
            });
        }
        *now = deadline.ticks();
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, u64>, ManualClockError> {
        self.ticks.lock().map_err(|_| ManualClockError::Poisoned)
    }
}

impl LeaseTimeAuthority for ManualLeaseClock {
    type Error = ManualClockError;

    fn now(&self) -> Result<LeaseDeadline, Self::Error> {
        Ok(LeaseDeadline::new(*self.lock()?))
    }
}

/// Deterministic manual-clock failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualClockError {
    /// Advancing ticks overflowed the persistent clock domain.
    Overflow,
    /// Monotonic clock was asked to move backward.
    WouldMoveBackward {
        /// Current authoritative tick.
        current: u64,
        /// Rejected requested tick.
        requested: u64,
    },
    /// Another thread panicked while holding the clock lock.
    Poisoned,
}

impl fmt::Display for ManualClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow => formatter.write_str("manual lease clock overflowed"),
            Self::WouldMoveBackward { current, requested } => write!(
                formatter,
                "manual lease clock cannot move backward from {current} to {requested}"
            ),
            Self::Poisoned => formatter.write_str("manual lease clock lock is poisoned"),
        }
    }
}

impl std::error::Error for ManualClockError {}

/// Idempotent request to acquire an absent or expired writer lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquireWriterLease {
    /// Filesystem authority.
    filesystem_id: FilesystemId,
    /// Stable lease-operation replay identity.
    operation_id: LeaseOperationId,
    /// Requested protected scope.
    scope: WriterScopeId,
    /// Requesting writer incarnation.
    holder: WriterIncarnationId,
    /// Caller-selected identity for the new grant.
    lease_id: LeaseId,
    /// Requested positive duration.
    duration: LeaseDuration,
}

impl AcquireWriterLease {
    /// Creates a bounded lease-acquisition request.
    pub fn new(
        filesystem_id: FilesystemId,
        operation_id: LeaseOperationId,
        scope: WriterScopeId,
        holder: WriterIncarnationId,
        lease_id: LeaseId,
        duration: LeaseDuration,
        limits: StateLimits,
    ) -> Result<Self, InvalidLeaseRequest> {
        validate_duration(duration, limits)?;
        Ok(Self {
            filesystem_id,
            operation_id,
            scope,
            holder,
            lease_id,
            duration,
        })
    }

    /// Returns the filesystem authority.
    pub const fn filesystem_id(self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns the stable replay identity.
    pub const fn operation_id(self) -> LeaseOperationId {
        self.operation_id
    }

    /// Returns the requested writer scope.
    pub const fn scope(self) -> WriterScopeId {
        self.scope
    }

    /// Returns the requesting writer incarnation.
    pub const fn holder(self) -> WriterIncarnationId {
        self.holder
    }

    /// Returns the caller-selected grant identity.
    pub const fn lease_id(self) -> LeaseId {
        self.lease_id
    }

    /// Returns the requested bounded duration.
    pub const fn duration(self) -> LeaseDuration {
        self.duration
    }

    /// Revalidates duration against the receiving adapter's contract.
    pub fn validate(self, limits: StateLimits) -> Result<(), InvalidLeaseRequest> {
        validate_duration(self.duration, limits)
    }

    /// Fingerprints all operands protected by this operation identity.
    pub fn operation_fingerprint(self) -> RequestFingerprint {
        lease_fingerprint(
            b"acquire",
            &[
                self.filesystem_id.as_bytes(),
                self.operation_id.as_bytes(),
                self.scope.as_bytes(),
                self.holder.as_bytes(),
                self.lease_id.as_bytes(),
                &self.duration.ticks().to_be_bytes(),
            ],
        )
    }
}

/// Idempotent request to extend the exact current lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenewWriterLease {
    /// Filesystem authority.
    filesystem_id: FilesystemId,
    /// Stable lease-operation replay identity.
    operation_id: LeaseOperationId,
    /// Exact current writer authority.
    fence: WriterFence,
    /// Requested positive extension from adapter-authoritative now.
    duration: LeaseDuration,
}

impl RenewWriterLease {
    /// Creates a bounded lease-renewal request.
    pub fn new(
        filesystem_id: FilesystemId,
        operation_id: LeaseOperationId,
        fence: WriterFence,
        duration: LeaseDuration,
        limits: StateLimits,
    ) -> Result<Self, InvalidLeaseRequest> {
        validate_duration(duration, limits)?;
        Ok(Self {
            filesystem_id,
            operation_id,
            fence,
            duration,
        })
    }

    /// Returns the filesystem authority.
    pub const fn filesystem_id(self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns the stable replay identity.
    pub const fn operation_id(self) -> LeaseOperationId {
        self.operation_id
    }

    /// Returns the exact current writer authority.
    pub const fn fence(self) -> WriterFence {
        self.fence
    }

    /// Returns the requested bounded duration.
    pub const fn duration(self) -> LeaseDuration {
        self.duration
    }

    /// Revalidates duration against the receiving adapter's contract.
    pub fn validate(self, limits: StateLimits) -> Result<(), InvalidLeaseRequest> {
        validate_duration(self.duration, limits)
    }

    /// Fingerprints all operands protected by this operation identity.
    pub fn operation_fingerprint(self) -> RequestFingerprint {
        fence_fingerprint(
            b"renew",
            self.filesystem_id,
            self.operation_id,
            self.fence,
            Some(self.duration),
        )
    }
}

/// Idempotent request to release the exact current lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseWriterLease {
    /// Filesystem authority.
    filesystem_id: FilesystemId,
    /// Stable lease-operation replay identity.
    operation_id: LeaseOperationId,
    /// Exact current writer authority.
    fence: WriterFence,
}

impl ReleaseWriterLease {
    /// Creates an exact idempotent lease-release request.
    pub const fn new(
        filesystem_id: FilesystemId,
        operation_id: LeaseOperationId,
        fence: WriterFence,
    ) -> Self {
        Self {
            filesystem_id,
            operation_id,
            fence,
        }
    }

    /// Returns the filesystem authority.
    pub const fn filesystem_id(self) -> FilesystemId {
        self.filesystem_id
    }

    /// Returns the stable replay identity.
    pub const fn operation_id(self) -> LeaseOperationId {
        self.operation_id
    }

    /// Returns the exact current writer authority.
    pub const fn fence(self) -> WriterFence {
        self.fence
    }

    /// Fingerprints all operands protected by this operation identity.
    pub fn operation_fingerprint(self) -> RequestFingerprint {
        fence_fingerprint(
            b"release",
            self.filesystem_id,
            self.operation_id,
            self.fence,
            None,
        )
    }
}

/// Invalid lease request rejected before the authority is accessed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidLeaseRequest {
    /// Requested duration exceeds the configured maximum.
    Limit(StateLimitError),
}

impl fmt::Display for InvalidLeaseRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for InvalidLeaseRequest {}

fn validate_duration(
    duration: LeaseDuration,
    limits: StateLimits,
) -> Result<(), InvalidLeaseRequest> {
    if duration.ticks() > limits.max_lease_duration_ticks() {
        Err(InvalidLeaseRequest::Limit(StateLimitError::new(
            StateLimitKind::LeaseDuration,
            duration.ticks(),
            limits.max_lease_duration_ticks(),
        )))
    } else {
        Ok(())
    }
}

fn fence_fingerprint(
    domain: &[u8],
    filesystem_id: FilesystemId,
    operation_id: LeaseOperationId,
    fence: WriterFence,
    duration: Option<LeaseDuration>,
) -> RequestFingerprint {
    let duration_bytes = duration.map_or(0, LeaseDuration::ticks).to_be_bytes();
    let token_bytes = fence.fencing_token.get().to_be_bytes();
    lease_fingerprint(
        domain,
        &[
            filesystem_id.as_bytes(),
            operation_id.as_bytes(),
            fence.scope.as_bytes(),
            fence.holder.as_bytes(),
            fence.lease_id.as_bytes(),
            &token_bytes,
            &duration_bytes,
        ],
    )
}

fn lease_fingerprint(domain: &[u8], parts: &[&[u8]]) -> RequestFingerprint {
    let mut bytes = Vec::with_capacity(128);
    bytes.extend_from_slice(b"w9pt-fs-state-lease-v1\0");
    bytes.extend_from_slice(domain);
    for part in parts {
        bytes.extend_from_slice(part);
    }
    RequestFingerprint::blake3(&bytes)
}

/// Successful writer-lease grant or renewal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriterLeaseGrant {
    /// Exact authority required by commits and later lease operations.
    pub fence: WriterFence,
    /// Adapter-authoritative expiry deadline.
    pub deadline: LeaseDeadline,
}

/// Semantic reason a lease operation was not applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseRejection {
    /// Another unexpired lease currently owns the scope.
    Busy(WriterLeaseGrant),
    /// The presented lease identity, holder, scope, or token is stale.
    StaleFence,
    /// The exact lease has expired.
    Expired,
    /// A single-writer adapter received a non-filesystem-wide scope.
    InvalidWriterScope,
    /// Duration exceeds the configured maximum or arithmetic overflowed.
    InvalidDuration,
    /// One operation identity was reused for different operands.
    OperationMismatch,
    /// Bounded retained lease-operation history is full.
    OperationHistoryFull,
    /// The scope's monotonic fencing-token domain is exhausted.
    FencingTokenExhausted,
}

/// Result of acquiring a writer lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcquireLeaseOutcome {
    /// A new token was allocated and the lease was granted.
    Granted(WriterLeaseGrant),
    /// Exact operation replay returned its retained grant.
    AlreadyApplied(WriterLeaseGrant),
    /// Request was rejected without changing authority.
    Rejected(LeaseRejection),
}

/// Result of renewing a writer lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenewLeaseOutcome {
    /// Exact current lease was extended without changing its token.
    Renewed(WriterLeaseGrant),
    /// Exact operation replay returned its retained renewal.
    AlreadyApplied(WriterLeaseGrant),
    /// Request was rejected without changing authority.
    Rejected(LeaseRejection),
}

/// Result of releasing a writer lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseLeaseOutcome {
    /// Exact current lease was invalidated.
    Released,
    /// Exact operation replay returned its retained release result.
    AlreadyApplied,
    /// Request was rejected without changing authority.
    Rejected(LeaseRejection),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_duration_and_operation_identity_are_bounded_and_complete() {
        let limits = StateLimits::default();
        let request = AcquireWriterLease::new(
            FilesystemId::from_u128(1),
            LeaseOperationId::from_u128(2),
            WriterScopeId::from_u128(3),
            WriterIncarnationId::from_u128(4),
            LeaseId::from_u128(5),
            LeaseDuration::new(6).unwrap(),
            limits,
        )
        .unwrap();
        let changed = AcquireWriterLease::new(
            request.filesystem_id(),
            request.operation_id(),
            request.scope(),
            request.holder(),
            request.lease_id(),
            LeaseDuration::new(7).unwrap(),
            limits,
        )
        .unwrap();
        assert_ne!(
            request.operation_fingerprint(),
            changed.operation_fingerprint()
        );
        assert!(
            AcquireWriterLease::new(
                request.filesystem_id(),
                request.operation_id(),
                request.scope(),
                request.holder(),
                request.lease_id(),
                LeaseDuration::new(limits.max_lease_duration_ticks() + 1).unwrap(),
                limits,
            )
            .is_err()
        );
    }

    #[test]
    fn fence_validation_requires_every_identity_and_unexpired_deadline() {
        let filesystem_id = FilesystemId::from_u128(1);
        let fence = WriterFence::new(
            WriterScopeId::from_u128(2),
            WriterIncarnationId::from_u128(3),
            LeaseId::from_u128(4),
            FencingToken::new(5).unwrap(),
        );
        let record = WriterLeaseRecord::new(
            filesystem_id,
            fence.scope,
            fence.holder,
            fence.lease_id,
            LeaseDeadline::new(10),
            fence.fencing_token,
            crate::RecordRevision::new(1).unwrap(),
        );
        assert_eq!(
            validate_writer_fence(filesystem_id, fence, Some(&record), LeaseDeadline::new(9)),
            FenceValidation::Current
        );
        assert_eq!(
            validate_writer_fence(filesystem_id, fence, Some(&record), LeaseDeadline::new(10)),
            FenceValidation::Expired
        );
        assert_eq!(
            validate_writer_fence(
                filesystem_id,
                WriterFence {
                    fencing_token: FencingToken::new(6).unwrap(),
                    ..fence
                },
                Some(&record),
                LeaseDeadline::new(9),
            ),
            FenceValidation::Stale
        );
    }

    #[test]
    fn manual_clock_is_shared_checked_and_monotonic() {
        let clock = ManualLeaseClock::new(LeaseDeadline::new(5));
        let other_client = clock.clone();
        assert_eq!(clock.advance(2).unwrap(), LeaseDeadline::new(7));
        assert_eq!(other_client.now().unwrap(), LeaseDeadline::new(7));
        assert!(matches!(
            clock.advance_to(LeaseDeadline::new(6)),
            Err(ManualClockError::WouldMoveBackward { .. })
        ));
        let maximum = ManualLeaseClock::new(LeaseDeadline::new(u64::MAX));
        assert_eq!(maximum.advance(1), Err(ManualClockError::Overflow));
    }

    #[test]
    fn expiry_and_release_never_reset_fencing_tokens() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let request = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(2),
            WriterScopeId::from_u128(3),
            WriterIncarnationId::from_u128(4),
            LeaseId::from_u128(5),
            LeaseDuration::new(5).unwrap(),
            limits,
        )
        .unwrap();
        let first = grant_writer_lease(
            request,
            None,
            None,
            LeaseDeadline::new(0),
            WriterTopology::SerializableMultiWriter,
            RecordRevision::new(1).unwrap(),
        )
        .unwrap();
        let takeover = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(6),
            request.scope(),
            WriterIncarnationId::from_u128(7),
            LeaseId::from_u128(8),
            LeaseDuration::new(5).unwrap(),
            limits,
        )
        .unwrap();
        assert!(matches!(
            grant_writer_lease(
                takeover,
                Some(&first),
                Some(first.fencing_token()),
                LeaseDeadline::new(4),
                WriterTopology::SerializableMultiWriter,
                RecordRevision::new(2).unwrap(),
            ),
            Err(LeaseRejection::Busy(_))
        ));
        let second = grant_writer_lease(
            takeover,
            Some(&first),
            Some(first.fencing_token()),
            LeaseDeadline::new(5),
            WriterTopology::SerializableMultiWriter,
            RecordRevision::new(2).unwrap(),
        )
        .unwrap();
        assert!(second.fencing_token() > first.fencing_token());
        assert!(
            grant_writer_lease(
                request,
                None,
                Some(second.fencing_token()),
                LeaseDeadline::new(10),
                WriterTopology::SerializableMultiWriter,
                RecordRevision::new(3).unwrap(),
            )
            .unwrap()
            .fencing_token()
                > second.fencing_token()
        );
    }

    #[test]
    fn single_writer_topology_accepts_only_filesystem_scope() {
        let request = AcquireWriterLease::new(
            FilesystemId::from_u128(1),
            LeaseOperationId::from_u128(2),
            WriterScopeId::from_u128(3),
            WriterIncarnationId::from_u128(4),
            LeaseId::from_u128(5),
            LeaseDuration::new(1).unwrap(),
            StateLimits::default(),
        )
        .unwrap();
        assert_eq!(
            grant_writer_lease(
                request,
                None,
                None,
                LeaseDeadline::new(0),
                WriterTopology::SingleFencedWriter,
                RecordRevision::new(1).unwrap(),
            ),
            Err(LeaseRejection::InvalidWriterScope)
        );
    }

    #[test]
    fn renewal_never_shortens_and_fence_exhaustion_is_distinct() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let fence = WriterFence::new(
            WriterScopeId::from_u128(2),
            WriterIncarnationId::from_u128(3),
            LeaseId::from_u128(4),
            FencingToken::new(5).unwrap(),
        );
        let current = WriterLeaseRecord::new(
            filesystem_id,
            fence.scope,
            fence.holder,
            fence.lease_id,
            LeaseDeadline::new(100),
            fence.fencing_token,
            RecordRevision::new(1).unwrap(),
        );
        let renew = RenewWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(6),
            fence,
            LeaseDuration::new(5).unwrap(),
            limits,
        )
        .unwrap();
        let renewed = renew_current_lease(
            renew,
            Some(&current),
            LeaseDeadline::new(10),
            WriterTopology::SerializableMultiWriter,
            RecordRevision::new(2).unwrap(),
        )
        .unwrap();
        assert_eq!(renewed.deadline(), LeaseDeadline::new(100));

        let acquire = AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(7),
            fence.scope,
            fence.holder,
            LeaseId::from_u128(8),
            LeaseDuration::new(1).unwrap(),
            limits,
        )
        .unwrap();
        assert_eq!(
            grant_writer_lease(
                acquire,
                None,
                Some(FencingToken::new(u64::MAX).unwrap()),
                LeaseDeadline::new(100),
                WriterTopology::SerializableMultiWriter,
                RecordRevision::new(3).unwrap(),
            ),
            Err(LeaseRejection::FencingTokenExhausted)
        );
    }
}
