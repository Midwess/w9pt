//! Runtime-neutral immutable file-content storage methods for `w9pt`.
//!
//! This crate owns content layout and persistence. Filesystem namespace and inode
//! publication remain the responsibility of a higher-level metadata layer.
//!
//! Version 1 provides a bounded whole-file [`StorageMethod::Raw`] layout and a
//! sparse, fixed 32 KiB [`StorageMethod::BlockSplit`] layout. Layout selection is
//! persisted separately from the identity/no-encryption [`Representation`].
//! Immutable payloads are acknowledged before immutable manifests, and
//! [`PreparedContent`] is returned before any authoritative inode transaction.
//!
//! [`ObjectHeadPublisher`] offers standalone compare-and-swap publication. A
//! clustered filesystem should instead publish [`ContentRef`] with inode metadata
//! in its authoritative database. Successful version-1 writes are write-through;
//! [`ContentRepository::sync_content`] does not imply namespace metadata durability.
//!
//! This crate is under active unreleased development. Its API, private key
//! layout, fingerprints, and persisted formats may change without legacy readers
//! or migration support; recreate development object prefixes after a breaking
//! change.

#![forbid(unsafe_code)]

mod config;
mod error;
pub mod format;
mod ids;
mod keys;
pub mod layout;
mod limits;
mod object_store;
mod publisher;
mod repository;
pub mod testing;

pub use config::{
    BLOCK_SIZE_V1, BlockSplitParameters, CreationDefaults, HashAlgorithm, PayloadCipher,
    PayloadCodec, Representation, StorageMethod, UnsupportedBlockSize,
};
pub use error::{
    AmbiguityError, AmbiguousOperation, ConflictError, CorruptionError, FormatError,
    MissingObjectKind, PreparationError, RangeError, StorageError, TargetError, TargetOperation,
};
pub use ids::{
    BaseContentIdentity, ContentRef, Digest, FileId, InvalidContentRef, InvalidObjectKey,
    MutationId, ObjectKey, ObjectVersion, OperationFingerprint, PreparationIdentity,
    PreparedContent,
};
pub use keys::KeySpace;
pub use limits::{ConfigurationError, LimitError, LimitKind, StorageLimitValues, StorageLimits};
pub use object_store::{
    CompareExchange, ObjectRange, PutIfAbsent, TargetGuarantees, TargetObject, TargetStore,
};
pub use publisher::{ObjectHeadPublisher, Publication, PublishedContent};
pub use repository::ContentRepository;
