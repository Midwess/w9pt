//! Runtime-neutral immutable file-content storage methods for `w9pt`.
//!
//! This crate owns content layout and persistence. Filesystem namespace and inode
//! publication remain the responsibility of a higher-level metadata layer.
//!
//! The current version provides a bounded whole-file [`StorageMethod::Raw`] layout
//! and a paged sparse, fixed 32 KiB [`StorageMethod::BlockSplit`] layout. Layout
//! selection is persisted separately from the identity/no-encryption
//! [`Representation`]. Immutable payloads and mapping pages are acknowledged in
//! dependency order before the compact manifest, and [`PreparedContent`] is
//! returned before any authoritative inode transaction.
//!
//! [`ObjectHeadPublisher`] offers standalone compare-and-swap publication. A
//! clustered filesystem should instead publish [`ContentRef`] with inode metadata
//! in its authoritative database. Successful writes are write-through;
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
pub mod representation;
pub mod testing;

pub use config::{
    BLOCK_MAP_FANOUT, BLOCK_MAP_INDEX_BITS, BLOCK_MAP_MAX_LEVEL, BLOCK_SIZE, BlockSplitParameters,
    CreationDefaults, HashAlgorithm, PayloadCipher, PayloadCodec, Representation, StorageMethod,
    UnsupportedBlockProfile,
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
pub use representation::{
    ActualCodec, CandidateKeyError, CompressionPolicy, ContentCipher, ContentContextBinding,
    ContentContextId, ContentMetadataCandidate, FileContextScope, FileCryptoContext,
    FileStoragePolicy, MasterKey, MasterKeyId, ObjectProvenance, RepresentationError,
    SecureEntropy, encode_object, generate_content_metadata, open_committed_context,
    rewrap_content_metadata,
};
