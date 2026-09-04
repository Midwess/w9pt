//! Runtime-neutral filesystem-state store interface.

use crate::{
    AcquireLeaseOutcome, AcquireWriterLease, ChangePoll, ChangePollOutcome, CommitOutcome,
    CommitRequest, ReadBatch, ReadOutcome, ReleaseLeaseOutcome, ReleaseWriterLease,
    RenewLeaseOutcome, RenewWriterLease, StateStoreAdapterError, StateStoreContract,
};

/// Authoritative filesystem metadata and coordination store.
///
/// Requests own their data and returned futures do not borrow temporary request
/// values, allowing callers to schedule them on any executor.
pub trait FilesystemStateStore: Send + Sync {
    /// Adapter-specific definitive infrastructure failure.
    type Error: StateStoreAdapterError;

    /// Returns the validated guarantees and limits of this adapter instance.
    fn contract(&self) -> StateStoreContract;

    /// Executes all point reads and scans against one authoritative revision.
    fn read(
        &self,
        request: ReadBatch,
    ) -> impl Future<Output = Result<ReadOutcome, Self::Error>> + Send;

    /// Applies one declarative serializable mutation or returns a semantic outcome.
    fn commit(
        &self,
        request: CommitRequest,
    ) -> impl Future<Output = Result<CommitOutcome, Self::Error>> + Send;

    /// Idempotently acquires an absent or expired writer lease.
    fn acquire_writer_lease(
        &self,
        request: AcquireWriterLease,
    ) -> impl Future<Output = Result<AcquireLeaseOutcome, Self::Error>> + Send;

    /// Idempotently renews the exact current writer lease.
    fn renew_writer_lease(
        &self,
        request: RenewWriterLease,
    ) -> impl Future<Output = Result<RenewLeaseOutcome, Self::Error>> + Send;

    /// Idempotently releases the exact current writer lease.
    fn release_writer_lease(
        &self,
        request: ReleaseWriterLease,
    ) -> impl Future<Output = Result<ReleaseLeaseOutcome, Self::Error>> + Send;

    /// Nonblockingly polls bounded whole-commit invalidation events.
    fn poll_changes(
        &self,
        request: ChangePoll,
    ) -> impl Future<Output = Result<ChangePollOutcome, Self::Error>> + Send;
}
