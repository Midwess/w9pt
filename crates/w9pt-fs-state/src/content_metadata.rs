//! Generic bounded opaque file-content metadata.

use core::fmt;

use crate::{InodeId, RecordRevision, StateLimitError, StateLimitKind, StateLimits};

/// Authoritative opaque storage policy and wrapped per-file key metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentMetadataRecord {
    owner_inode_id: InodeId,
    content_file_id: w9pt_fs_storage::FileId,
    context_id: w9pt_fs_storage::ContentContextId,
    policy_format: u16,
    policy_bytes: Box<[u8]>,
    key_commitment: Option<[u8; 32]>,
    wrapped_key_bytes: Option<Box<[u8]>>,
    revision: RecordRevision,
}

impl ContentMetadataRecord {
    /// Constructs bounded opaque metadata without interpreting algorithms.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner_inode_id: InodeId,
        content_file_id: w9pt_fs_storage::FileId,
        context_id: w9pt_fs_storage::ContentContextId,
        policy_format: u16,
        policy_bytes: Vec<u8>,
        key_commitment: Option<[u8; 32]>,
        wrapped_key_bytes: Option<Vec<u8>>,
        revision: RecordRevision,
        limits: StateLimits,
    ) -> Result<Self, ContentMetadataError> {
        if policy_format == 0 || policy_bytes.is_empty() {
            return Err(ContentMetadataError::InvalidPolicyShape);
        }
        if key_commitment.is_some() != wrapped_key_bytes.is_some() {
            return Err(ContentMetadataError::InvalidKeyShape);
        }
        limits.require_bytes(
            StateLimitKind::ContentPolicy,
            policy_bytes.len(),
            limits.max_content_policy_bytes(),
        )?;
        let wrapped_len = wrapped_key_bytes.as_ref().map_or(0, Vec::len);
        limits.require_bytes(
            StateLimitKind::WrappedContentKey,
            wrapped_len,
            limits.max_wrapped_content_key_bytes(),
        )?;
        let retained = 96usize
            .checked_add(policy_bytes.len())
            .and_then(|size| size.checked_add(wrapped_len))
            .ok_or(ContentMetadataError::RetainedSizeOverflow)?;
        limits.require_bytes(
            StateLimitKind::ContentMetadata,
            retained,
            limits.max_content_metadata_bytes(),
        )?;
        Ok(Self {
            owner_inode_id,
            content_file_id,
            context_id,
            policy_format,
            policy_bytes: policy_bytes.into_boxed_slice(),
            key_commitment,
            wrapped_key_bytes: wrapped_key_bytes.map(Vec::into_boxed_slice),
            revision,
        })
    }

    /// Returns the original owning inode.
    pub const fn owner_inode_id(&self) -> InodeId {
        self.owner_inode_id
    }
    /// Returns the storage content file identity.
    pub const fn content_file_id(&self) -> w9pt_fs_storage::FileId {
        self.content_file_id
    }
    /// Returns the stable context identity.
    pub const fn context_id(&self) -> w9pt_fs_storage::ContentContextId {
        self.context_id
    }
    /// Returns the opaque policy format.
    pub const fn policy_format(&self) -> u16 {
        self.policy_format
    }
    /// Returns exact opaque policy bytes.
    pub fn policy_bytes(&self) -> &[u8] {
        &self.policy_bytes
    }
    /// Returns the immutable optional key commitment.
    pub const fn key_commitment(&self) -> Option<&[u8; 32]> {
        self.key_commitment.as_ref()
    }
    /// Returns the optional opaque wrapped key envelope.
    pub fn wrapped_key_bytes(&self) -> Option<&[u8]> {
        self.wrapped_key_bytes.as_deref()
    }
    /// Returns the authoritative record revision.
    pub const fn revision(&self) -> RecordRevision {
        self.revision
    }

    pub(crate) fn with_revision(mut self, revision: RecordRevision) -> Self {
        self.revision = revision;
        self
    }

    pub(crate) fn rewrap(
        &mut self,
        expected_context: w9pt_fs_storage::ContentContextId,
        wrapped_key_bytes: Vec<u8>,
        limits: StateLimits,
    ) -> Result<(), ContentMetadataError> {
        self.validate_rewrap(expected_context, &wrapped_key_bytes, limits)?;
        self.wrapped_key_bytes = Some(wrapped_key_bytes.into_boxed_slice());
        Ok(())
    }

    /// Validates a replacement envelope against immutable shape and aggregate limits.
    pub fn validate_rewrap(
        &self,
        expected_context: w9pt_fs_storage::ContentContextId,
        wrapped_key_bytes: &[u8],
        limits: StateLimits,
    ) -> Result<(), ContentMetadataError> {
        if self.context_id != expected_context
            || self.key_commitment.is_none()
            || self.wrapped_key_bytes.is_none()
        {
            return Err(ContentMetadataError::InvalidRewrap);
        }
        if wrapped_key_bytes.is_empty() {
            return Err(ContentMetadataError::InvalidRewrap);
        }
        limits.require_bytes(
            StateLimitKind::WrappedContentKey,
            wrapped_key_bytes.len(),
            limits.max_wrapped_content_key_bytes(),
        )?;
        let retained = 96usize
            .checked_add(self.policy_bytes.len())
            .and_then(|size| size.checked_add(wrapped_key_bytes.len()))
            .ok_or(ContentMetadataError::RetainedSizeOverflow)?;
        limits.require_bytes(
            StateLimitKind::ContentMetadata,
            retained,
            limits.max_content_metadata_bytes(),
        )?;
        Ok(())
    }

    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        96usize.checked_add(self.policy_bytes.len())?.checked_add(
            self.wrapped_key_bytes
                .as_ref()
                .map_or(0, |bytes| bytes.len()),
        )
    }
}

/// Invalid opaque content metadata shape or bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContentMetadataError {
    /// Policy format/bytes are absent.
    InvalidPolicyShape,
    /// Commitment and wrapped envelope presence disagree.
    InvalidKeyShape,
    /// Retained-size arithmetic overflowed.
    RetainedSizeOverflow,
    /// Dedicated rewrap was attempted on a wrong/plain context.
    InvalidRewrap,
    /// A configured state bound was exceeded.
    Limit(StateLimitError),
}

impl fmt::Display for ContentMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicyShape => formatter.write_str("invalid content policy shape"),
            Self::InvalidKeyShape => formatter.write_str("invalid wrapped-key optional shape"),
            Self::RetainedSizeOverflow => {
                formatter.write_str("content metadata retained size overflow")
            }
            Self::InvalidRewrap => formatter.write_str("invalid content metadata rewrap"),
            Self::Limit(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ContentMetadataError {}

impl From<StateLimitError> for ContentMetadataError {
    fn from(error: StateLimitError) -> Self {
        Self::Limit(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_shapes_and_bounds_are_checked_without_policy_parsing() {
        let limits = StateLimits::default();
        let record = ContentMetadataRecord::new(
            InodeId::from_u128(1),
            w9pt_fs_storage::FileId::from_u128(2),
            w9pt_fs_storage::ContentContextId::from_u128(3),
            77,
            vec![0xaa; 512],
            Some([4; 32]),
            Some(vec![0xbb; 512]),
            RecordRevision::new(1).unwrap(),
            limits,
        )
        .unwrap();
        assert_eq!(record.policy_format(), 77);
        assert_eq!(record.policy_bytes().len(), 512);
        assert!(matches!(
            ContentMetadataRecord::new(
                InodeId::from_u128(1),
                w9pt_fs_storage::FileId::from_u128(2),
                w9pt_fs_storage::ContentContextId::from_u128(3),
                1,
                vec![1],
                Some([0; 32]),
                None,
                RecordRevision::new(1).unwrap(),
                limits,
            ),
            Err(ContentMetadataError::InvalidKeyShape)
        ));
        assert!(matches!(
            ContentMetadataRecord::new(
                InodeId::from_u128(1),
                w9pt_fs_storage::FileId::from_u128(2),
                w9pt_fs_storage::ContentContextId::from_u128(3),
                1,
                vec![0; 513],
                None,
                None,
                RecordRevision::new(1).unwrap(),
                limits,
            ),
            Err(ContentMetadataError::Limit(_))
        ));
    }

    #[test]
    fn receiver_and_rewrap_enforce_the_complete_metadata_bound() {
        let defaults = StateLimits::default();
        let mut values = defaults.values();
        values.max_content_metadata_bytes = 700;
        let tight = StateLimits::new(values).unwrap();
        let oversized = ContentMetadataRecord::new(
            InodeId::from_u128(1),
            w9pt_fs_storage::FileId::from_u128(2),
            w9pt_fs_storage::ContentContextId::from_u128(3),
            1,
            vec![0xaa; 512],
            Some([4; 32]),
            Some(vec![0xbb; 100]),
            RecordRevision::new(1).unwrap(),
            defaults,
        )
        .unwrap();
        assert!(matches!(
            crate::StateRecord::ContentMetadata(oversized).validate_against_limits(tight),
            Err(StateLimitError {
                kind: StateLimitKind::ContentMetadata,
                ..
            })
        ));

        let mut rewrapped = ContentMetadataRecord::new(
            InodeId::from_u128(1),
            w9pt_fs_storage::FileId::from_u128(2),
            w9pt_fs_storage::ContentContextId::from_u128(3),
            1,
            vec![0xaa; 512],
            Some([4; 32]),
            Some(vec![0xbb; 1]),
            RecordRevision::new(1).unwrap(),
            tight,
        )
        .unwrap();
        assert!(matches!(
            rewrapped.rewrap(
                w9pt_fs_storage::ContentContextId::from_u128(3),
                vec![0xbb; 100],
                tight,
            ),
            Err(ContentMetadataError::Limit(StateLimitError {
                kind: StateLimitKind::ContentMetadata,
                ..
            }))
        ));
        assert_eq!(rewrapped.wrapped_key_bytes().unwrap().len(), 1);
    }
}
