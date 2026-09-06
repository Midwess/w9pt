//! Runtime-neutral authoritative filesystem state for `w9pt`.
//!
//! This crate defines portable filesystem metadata records and the semantic
//! transaction contract implemented by local or distributed state adapters.
//!
//! This crate is under active unreleased development. Its API and record model
//! may change without compatibility aliases or readers for earlier builds.

#![forbid(unsafe_code)]

mod bounded;
mod change;
mod commit;
mod content_metadata;
mod contract;
mod error;
mod ids;
mod lease;
mod limits;
mod read;
mod records;
mod store;
mod values;

pub mod testing;

pub use change::{
    ChangeBatch, ChangeCursor, ChangeEvent, ChangeOrigin, ChangePoll, ChangePollOutcome,
    InvalidChangeRequest,
};
pub use commit::{
    AmbiguousCommit, COMMIT_PROTOCOL_ORDER, CommitConflict, CommitConflictKind, CommitOutcome,
    CommitProtocolPhase, CommitRequest, CommittedMutation, CounterAdjustment, InodeAttributeUpdate,
    MalformedCommit, MutationContext, MutationMismatch, MutationReplay, Precondition,
    PublishContent, PublishContentError, PublishXattrStaging, RewrapContentMetadata, StateChange,
    validate_publish_content, validate_publish_content_with_metadata,
};

pub use bounded::{
    BoundedValueError, EntryName, GroupId, MutationResult, MutationResultKind, PrincipalId,
    ResultFormatVersion, SymlinkTarget, XattrName, XattrValue,
};
pub use content_metadata::{ContentMetadataError, ContentMetadataRecord};
pub use contract::{
    AuthorityClass, InvalidStateStoreContract, RequiredGuarantee, StateStoreContract,
    StateStoreGuarantees, WriterTopology,
};
pub use error::{AdapterFailureKind, StateStoreAdapterError, StateStoreOperation};
pub use ids::{
    ClientIncarnationId, FilesystemId, InodeId, LeaseId, LeaseOperationId, LockId, OpenId,
    WriterIncarnationId, WriterScopeId, XattrStagingId,
};
pub use lease::{
    AcquireLeaseOutcome, AcquireWriterLease, FenceValidation, InvalidLeaseRequest, LeaseRejection,
    LeaseTimeAuthority, ManualClockError, ManualLeaseClock, ReleaseLeaseOutcome,
    ReleaseWriterLease, RenewLeaseOutcome, RenewWriterLease, WriterFence, WriterLeaseGrant,
    grant_writer_lease, next_fencing_token, renew_current_lease, validate_lease_release,
    validate_writer_fence,
};
pub use limits::{
    InvalidStateLimits, StateLimitError, StateLimitKind, StateLimitValues, StateLimits,
};
pub use read::{
    DirectoryPage, DirectoryPageEntry, InvalidScanBounds, InvalidStateSnapshot, LockCursor,
    OpenPinCursor, ReadBatch, ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordScan,
    ScanBounds, ScanPage, ScanResume, StateSnapshot, XattrCursor,
};
pub use records::{
    DeviceNumbers, DirectoryEntryRecord, FilesystemRecord, InodeData, InodeKind, InodeRecord,
    InodeTimes, LockKind, LockOwner, LockRange, LockRangeEnd, LockRecord, MutationRecord,
    OpenAccess, OpenPinRecord, OpenRecord, OrphanRecord, RecordFamily, RecordKey,
    RecordValidationError, StateRecord, WriterLeaseRecord, XattrRecord, XattrStagingRecord,
    validate_record_set, validate_record_set_with_limits,
};
pub use store::FilesystemStateStore;
pub use values::{
    CounterOverflow, DataGeneration, DirectoryCookie, DirectoryGeneration, FencingToken,
    InodeGeneration, InvalidValue, LeaseDeadline, LeaseDuration, LockGeneration, MutationRetention,
    QidPath, RecordRevision, RequestFingerprint, StateRevision, UnixTimestamp,
};
