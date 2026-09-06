//! Checked versioned persistent-object encoding.

mod block_map;
mod envelope;
mod head;
mod manifest;
mod primitives;

use core::fmt;

pub use envelope::{Envelope, ObjectKind, decode_envelope, encode_envelope};
pub use head::{FileHead, decode_head, encode_head};
pub use manifest::{BlobRef, FileManifest, ManifestLayout, decode_manifest, encode_manifest};
pub(crate) use primitives::{Reader, Writer};

use crate::{CorruptionError, FormatError, LimitError};

/// Failure while decoding one checked persistent object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistentFormatError {
    /// Structurally malformed or unsupported bytes.
    Format(FormatError),
    /// Checksum, digest, identity, or canonical-content failure.
    Corruption(CorruptionError),
    /// Object exceeds a configured decode bound.
    Limit(LimitError),
}

impl fmt::Display for PersistentFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(error) => error.fmt(formatter),
            Self::Corruption(error) => error.fmt(formatter),
            Self::Limit(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PersistentFormatError {}

impl From<FormatError> for PersistentFormatError {
    fn from(error: FormatError) -> Self {
        Self::Format(error)
    }
}

impl From<CorruptionError> for PersistentFormatError {
    fn from(error: CorruptionError) -> Self {
        Self::Corruption(error)
    }
}

impl From<LimitError> for PersistentFormatError {
    fn from(error: LimitError) -> Self {
        Self::Limit(error)
    }
}
pub use block_map::{
    BlockMapPage, BranchEntry, LeafEntry, PageRef, aligned_page_start, decode_block_map_page,
    encode_block_map_page, minimum_root_level, page_coverage, page_slot, validate_page_context,
};
