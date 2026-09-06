//! Typed failures for repository, format, and publication operations.

use core::fmt;

use crate::{ConfigurationError, LimitError, ObjectKey, RepresentationError};

/// Invalid relationship between a logical mutation, its base, and prepared content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationError {
    /// Publisher mutation ID differs from the preparation identity.
    MutationMismatch,
    /// Preparation was produced against a different base content version.
    BaseMismatch,
    /// Immutable manifest key does not encode the preparation identity.
    KeyMismatch,
    /// A rebase closure changed the logical operation fingerprint.
    FingerprintMismatch,
    /// A publication operation received an unchanged preparation.
    UnchangedPublication,
}

impl fmt::Display for PreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MutationMismatch => formatter.write_str("prepared mutation identity mismatch"),
            Self::BaseMismatch => formatter.write_str("prepared base content identity mismatch"),
            Self::KeyMismatch => formatter.write_str("prepared manifest key identity mismatch"),
            Self::FingerprintMismatch => {
                formatter.write_str("logical operation fingerprint changed during rebase")
            }
            Self::UnchangedPublication => {
                formatter.write_str("unchanged content must not publish a replacement head")
            }
        }
    }
}

impl std::error::Error for PreparationError {}

/// Invalid or overflowing caller-supplied logical range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeError {
    /// `offset + length` cannot be represented as a `u64`.
    EndOverflow {
        /// Starting logical offset.
        offset: u64,
        /// Requested byte length.
        length: u64,
    },
    /// A platform-sized length cannot be represented as a logical length.
    LengthConversion,
    /// A half-open range ends before it starts.
    InvalidOrder {
        /// Inclusive range start.
        start: u64,
        /// Exclusive range end.
        end: u64,
    },
}

impl fmt::Display for RangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndOverflow { offset, length } => {
                write!(formatter, "logical range {offset}+{length} overflows u64")
            }
            Self::LengthConversion => formatter.write_str("range length conversion overflow"),
            Self::InvalidOrder { start, end } => {
                write!(formatter, "range end {end} is before start {start}")
            }
        }
    }
}

impl std::error::Error for RangeError {}

/// Invalid, non-canonical, or unsupported persisted bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormatError {
    /// The object is shorter than a required field.
    Truncated,
    /// The object contains bytes after its canonical end.
    TrailingData,
    /// The object magic does not identify this format.
    InvalidMagic,
    /// The object kind does not match the requested decoder.
    UnexpectedKind {
        /// Kind required by the selected decoder.
        expected: u8,
        /// Kind found in the envelope.
        actual: u8,
    },
    /// The major format version is unsupported.
    UnsupportedVersion {
        /// Unsupported major version.
        major: u16,
        /// Encoded minor version.
        minor: u16,
    },
    /// An enum or representation tag is unknown.
    UnknownTag {
        /// Field containing the tag.
        field: &'static str,
        /// Unsupported numeric tag.
        tag: u64,
    },
    /// A decoded collection or relationship is non-canonical.
    NonCanonical {
        /// Non-canonical field or relationship.
        field: &'static str,
    },
    /// Length fields are inconsistent with encoded bytes.
    InconsistentLength {
        /// Field whose declared and actual lengths differ.
        field: &'static str,
    },
    /// A decoded integer relationship overflowed.
    ArithmeticOverflow {
        /// Field whose bounds cannot be represented.
        field: &'static str,
    },
}

impl fmt::Display for FormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("persisted object is truncated"),
            Self::TrailingData => formatter.write_str("persisted object has trailing data"),
            Self::InvalidMagic => formatter.write_str("persisted object has invalid magic"),
            Self::UnexpectedKind { expected, actual } => {
                write!(
                    formatter,
                    "unexpected object kind {actual}; expected {expected}"
                )
            }
            Self::UnsupportedVersion { major, minor } => {
                write!(formatter, "unsupported format version {major}.{minor}")
            }
            Self::UnknownTag { field, tag } => write!(formatter, "unknown {field} tag {tag}"),
            Self::NonCanonical { field } => write!(formatter, "non-canonical {field}"),
            Self::InconsistentLength { field } => write!(formatter, "inconsistent {field} length"),
            Self::ArithmeticOverflow { field } => write!(formatter, "{field} arithmetic overflow"),
        }
    }
}

impl std::error::Error for FormatError {}

/// Persisted bytes are structurally valid but fail integrity or identity checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CorruptionError {
    /// The envelope checksum does not match its payload.
    ChecksumMismatch,
    /// Canonical plaintext does not match its recorded digest.
    DigestMismatch,
    /// A decoded object belongs to a different file or generation.
    IdentityMismatch {
        /// Identity field that did not match its context.
        field: &'static str,
    },
    /// Decoded plaintext or stored bytes have an impossible size.
    InvalidLength {
        /// Persisted length field being checked.
        field: &'static str,
        /// Canonical required length.
        expected: u64,
        /// Decoded actual length.
        actual: u64,
    },
    /// An immutable key already contains different bytes.
    ImmutableCollision,
    /// A materialized final block contains nonzero bytes beyond logical EOF.
    NonZeroPadding,
    /// A persisted key points outside the configured repository prefix.
    ForeignKey,
    /// A persisted key does not use the canonical current repository schema.
    InvalidKeySchema,
}

impl fmt::Display for CorruptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChecksumMismatch => formatter.write_str("persisted payload checksum mismatch"),
            Self::DigestMismatch => formatter.write_str("canonical plaintext digest mismatch"),
            Self::IdentityMismatch { field } => {
                write!(formatter, "persisted {field} identity mismatch")
            }
            Self::InvalidLength {
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "invalid {field} length {actual}; expected {expected}"
            ),
            Self::ImmutableCollision => {
                formatter.write_str("immutable key contains different object bytes")
            }
            Self::NonZeroPadding => {
                formatter.write_str("final block contains nonzero bytes beyond logical EOF")
            }
            Self::ForeignKey => formatter.write_str("object key is outside the repository prefix"),
            Self::InvalidKeySchema => {
                formatter.write_str("object key does not match the canonical repository schema")
            }
        }
    }
}

impl std::error::Error for CorruptionError {}

/// Target operation being attempted when an adapter failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetOperation {
    /// Exact object read.
    Get,
    /// Exact half-open range read.
    GetRange,
    /// Atomic immutable creation.
    PutIfAbsent,
    /// Conditional mutable-head publication.
    CompareExchange,
}

/// Adapter failure annotated with its operation and key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetError<E> {
    /// Operation that failed.
    pub operation: TargetOperation,
    /// Target key involved in the operation.
    pub key: ObjectKey,
    /// Adapter-specific source error.
    pub source: E,
}

impl<E: fmt::Display> fmt::Display for TargetError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "target {:?} failed for {}: {}",
            self.operation, self.key, self.source
        )
    }
}

impl<E: std::error::Error + 'static> std::error::Error for TargetError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Conditional publication exhausted its configured rebase bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConflictError {
    /// Number of conflicts observed.
    pub conflicts: u32,
}

impl fmt::Display for ConflictError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "publication conflicted {} times", self.conflicts)
    }
}

impl std::error::Error for ConflictError {}

/// Operation whose commit state could not be resolved safely by readback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmbiguousOperation {
    /// Conditional creation of an immutable payload or manifest.
    ImmutableCreation,
    /// Conditional publication of a mutable standalone file head.
    HeadPublication,
}

impl fmt::Display for AmbiguousOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImmutableCreation => formatter.write_str("immutable creation"),
            Self::HeadPublication => formatter.write_str("head publication"),
        }
    }
}

/// An ambiguous target mutation could not be resolved safely by readback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmbiguityError {
    /// Kind of target mutation whose outcome remains unknown.
    pub operation: AmbiguousOperation,
    /// Target key whose outcome remains unknown.
    pub key: ObjectKey,
}

impl fmt::Display for AmbiguityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} outcome for {} is ambiguous",
            self.operation, self.key
        )
    }
}

impl std::error::Error for AmbiguityError {}

/// Type of required object that was absent from the target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissingObjectKind {
    /// Mutable file head.
    Head,
    /// Immutable file manifest.
    Manifest,
    /// Immutable block-mapping page.
    MappingPage,
    /// Immutable raw or block payload.
    Payload,
}

/// Unified repository error preserving each correctness-relevant category.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageError<E> {
    /// Invalid repository configuration.
    Configuration(ConfigurationError),
    /// Invalid caller-supplied logical range.
    Range(RangeError),
    /// Invalid or unsupported persistent encoding.
    Format(FormatError),
    /// Integrity or object-identity failure.
    Corruption(CorruptionError),
    /// Adapter-specific target failure.
    Target(TargetError<E>),
    /// Bounded publication conflict.
    Conflict(ConflictError),
    /// Unresolved ambiguous target mutation result.
    Ambiguous(AmbiguityError),
    /// Configured bound exceeded.
    Limit(LimitError),
    /// Prepared content does not match its mutation or publication base.
    Preparation(PreparationError),
    /// Compression, encryption, file-key, or protected-context failure.
    Representation(RepresentationError),
    /// Required target object was absent.
    Missing {
        /// Expected object category.
        kind: MissingObjectKind,
        /// Missing target key.
        key: ObjectKey,
    },
}

impl<E: fmt::Display> fmt::Display for StorageError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => error.fmt(formatter),
            Self::Range(error) => error.fmt(formatter),
            Self::Format(error) => error.fmt(formatter),
            Self::Corruption(error) => error.fmt(formatter),
            Self::Target(error) => error.fmt(formatter),
            Self::Conflict(error) => error.fmt(formatter),
            Self::Ambiguous(error) => error.fmt(formatter),
            Self::Limit(error) => error.fmt(formatter),
            Self::Preparation(error) => error.fmt(formatter),
            Self::Representation(error) => error.fmt(formatter),
            Self::Missing { kind, key } => write!(formatter, "missing {kind:?} object {key}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for StorageError<E> {}

impl<E> From<ConfigurationError> for StorageError<E> {
    fn from(error: ConfigurationError) -> Self {
        Self::Configuration(error)
    }
}

impl<E> From<RangeError> for StorageError<E> {
    fn from(error: RangeError) -> Self {
        Self::Range(error)
    }
}

impl<E> From<FormatError> for StorageError<E> {
    fn from(error: FormatError) -> Self {
        Self::Format(error)
    }
}

impl<E> From<CorruptionError> for StorageError<E> {
    fn from(error: CorruptionError) -> Self {
        Self::Corruption(error)
    }
}

impl<E> From<LimitError> for StorageError<E> {
    fn from(error: LimitError) -> Self {
        Self::Limit(error)
    }
}

impl<E> From<PreparationError> for StorageError<E> {
    fn from(error: PreparationError) -> Self {
        Self::Preparation(error)
    }
}

impl<E> From<RepresentationError> for StorageError<E> {
    fn from(error: RepresentationError) -> Self {
        Self::Representation(error)
    }
}

impl<E> From<crate::format::PersistentFormatError> for StorageError<E> {
    fn from(error: crate::format::PersistentFormatError) -> Self {
        match error {
            crate::format::PersistentFormatError::Format(error) => Self::Format(error),
            crate::format::PersistentFormatError::Corruption(error) => Self::Corruption(error),
            crate::format::PersistentFormatError::Limit(error) => Self::Limit(error),
        }
    }
}
