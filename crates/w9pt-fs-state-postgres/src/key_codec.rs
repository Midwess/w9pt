//! Stable SQL encoding for semantic record keys.

use core::fmt;

use w9pt_fs_state::{
    BoundedValueError, EntryName, FilesystemId, InodeId, LockId, MalformedCommit, OpenId,
    RecordKey, StateChange, StateLimits, WriterScopeId, XattrName, XattrStagingId,
};
use w9pt_fs_storage::MutationId;

const FILESYSTEM_TAG: i16 = 1;
const INODE_TAG: i16 = 2;
const DIRECTORY_ENTRY_TAG: i16 = 3;
const OPEN_TAG: i16 = 4;
const OPEN_PIN_TAG: i16 = 5;
const ORPHAN_TAG: i16 = 6;
const LOCK_TAG: i16 = 7;
const XATTR_TAG: i16 = 8;
const XATTR_STAGING_TAG: i16 = 9;
const MUTATION_TAG: i16 = 10;
const WRITER_LEASE_TAG: i16 = 11;

/// Canonical sortable components persisted in `change_keys`.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SqlRecordKey {
    pub(crate) family_tag: i16,
    pub(crate) filesystem_id: [u8; 16],
    pub(crate) component_a: Vec<u8>,
    pub(crate) component_b: Vec<u8>,
}

impl SqlRecordKey {
    pub(crate) fn encode(key: &RecordKey) -> Self {
        let filesystem_id = *key.filesystem_id().as_bytes();
        let (family_tag, component_a, component_b) = match key {
            RecordKey::Filesystem(_) => (FILESYSTEM_TAG, Vec::new(), Vec::new()),
            RecordKey::Inode(_, inode_id) => (INODE_TAG, inode_id.as_bytes().to_vec(), Vec::new()),
            RecordKey::DirectoryEntry(_, parent_id, name) => (
                DIRECTORY_ENTRY_TAG,
                parent_id.as_bytes().to_vec(),
                name.as_bytes().to_vec(),
            ),
            RecordKey::Open(_, open_id) => (OPEN_TAG, open_id.as_bytes().to_vec(), Vec::new()),
            RecordKey::OpenPin(_, inode_id, open_id) => (
                OPEN_PIN_TAG,
                inode_id.as_bytes().to_vec(),
                open_id.as_bytes().to_vec(),
            ),
            RecordKey::Orphan(_, inode_id) => {
                (ORPHAN_TAG, inode_id.as_bytes().to_vec(), Vec::new())
            }
            RecordKey::Lock(_, inode_id, lock_id) => (
                LOCK_TAG,
                inode_id.as_bytes().to_vec(),
                lock_id.as_bytes().to_vec(),
            ),
            RecordKey::Xattr(_, inode_id, name) => (
                XATTR_TAG,
                inode_id.as_bytes().to_vec(),
                name.as_bytes().to_vec(),
            ),
            RecordKey::XattrStaging(_, staging_id) => (
                XATTR_STAGING_TAG,
                staging_id.as_bytes().to_vec(),
                Vec::new(),
            ),
            RecordKey::Mutation(_, mutation_id) => {
                (MUTATION_TAG, mutation_id.as_bytes().to_vec(), Vec::new())
            }
            RecordKey::WriterLease(_, scope_id) => {
                (WRITER_LEASE_TAG, scope_id.as_bytes().to_vec(), Vec::new())
            }
        };
        Self {
            family_tag,
            filesystem_id,
            component_a,
            component_b,
        }
    }

    pub(crate) fn decode(self, limits: StateLimits) -> Result<RecordKey, KeyCodecError> {
        let filesystem_id = FilesystemId::new(self.filesystem_id);
        match self.family_tag {
            FILESYSTEM_TAG if self.component_a.is_empty() && self.component_b.is_empty() => {
                Ok(RecordKey::Filesystem(filesystem_id))
            }
            INODE_TAG if self.component_b.is_empty() => Ok(RecordKey::Inode(
                filesystem_id,
                InodeId::new(fixed_component("inode_id", self.component_a)?),
            )),
            DIRECTORY_ENTRY_TAG => Ok(RecordKey::DirectoryEntry(
                filesystem_id,
                InodeId::new(fixed_component("parent_inode_id", self.component_a)?),
                EntryName::new(self.component_b, limits).map_err(KeyCodecError::Bounded)?,
            )),
            OPEN_TAG if self.component_b.is_empty() => Ok(RecordKey::Open(
                filesystem_id,
                OpenId::new(fixed_component("open_id", self.component_a)?),
            )),
            OPEN_PIN_TAG => Ok(RecordKey::OpenPin(
                filesystem_id,
                InodeId::new(fixed_component("inode_id", self.component_a)?),
                OpenId::new(fixed_component("open_id", self.component_b)?),
            )),
            ORPHAN_TAG if self.component_b.is_empty() => Ok(RecordKey::Orphan(
                filesystem_id,
                InodeId::new(fixed_component("inode_id", self.component_a)?),
            )),
            LOCK_TAG => Ok(RecordKey::Lock(
                filesystem_id,
                InodeId::new(fixed_component("inode_id", self.component_a)?),
                LockId::new(fixed_component("lock_id", self.component_b)?),
            )),
            XATTR_TAG => Ok(RecordKey::Xattr(
                filesystem_id,
                InodeId::new(fixed_component("inode_id", self.component_a)?),
                XattrName::new(self.component_b, limits).map_err(KeyCodecError::Bounded)?,
            )),
            XATTR_STAGING_TAG if self.component_b.is_empty() => Ok(RecordKey::XattrStaging(
                filesystem_id,
                XattrStagingId::new(fixed_component("staging_id", self.component_a)?),
            )),
            MUTATION_TAG if self.component_b.is_empty() => Ok(RecordKey::Mutation(
                filesystem_id,
                MutationId::new(fixed_component("mutation_id", self.component_a)?),
            )),
            WRITER_LEASE_TAG if self.component_b.is_empty() => Ok(RecordKey::WriterLease(
                filesystem_id,
                WriterScopeId::new(fixed_component("writer_scope_id", self.component_a)?),
            )),
            tag if !(FILESYSTEM_TAG..=WRITER_LEASE_TAG).contains(&tag) => {
                Err(KeyCodecError::UnknownFamilyTag(tag))
            }
            _ => Err(KeyCodecError::InvalidComponentShape {
                family_tag: self.family_tag,
            }),
        }
    }

    pub(crate) fn sql_component_a(&self) -> Option<&[u8]> {
        (!self.component_a.is_empty()).then_some(self.component_a.as_slice())
    }

    pub(crate) fn sql_component_b(&self) -> Option<&[u8]> {
        (!self.component_b.is_empty()).then_some(self.component_b.as_slice())
    }
}

pub(crate) fn canonical_affected_keys(
    filesystem_id: FilesystemId,
    changes: &[StateChange],
) -> Result<Vec<RecordKey>, MalformedCommit> {
    let mut keys = std::collections::BTreeSet::new();
    let mut filesystem_target_seen = false;
    let mut filesystem_target_only_allocations = true;
    let mut directory_allocation_seen = false;
    let mut qid_allocation_seen = false;
    for change in changes {
        let is_filesystem_allocation = match change {
            StateChange::AdvanceDirectoryCookie { .. } => {
                if directory_allocation_seen {
                    return Err(MalformedCommit::DuplicateChangeTarget(
                        RecordKey::Filesystem(filesystem_id),
                    ));
                }
                directory_allocation_seen = true;
                true
            }
            StateChange::AdvanceQidPath { .. } => {
                if qid_allocation_seen {
                    return Err(MalformedCommit::DuplicateChangeTarget(
                        RecordKey::Filesystem(filesystem_id),
                    ));
                }
                qid_allocation_seen = true;
                true
            }
            _ => false,
        };
        for key in change.affected_keys(filesystem_id) {
            let inserted = keys.insert(key.clone());
            if matches!(key, RecordKey::Filesystem(_)) {
                if filesystem_target_seen
                    && !(is_filesystem_allocation && filesystem_target_only_allocations)
                {
                    return Err(MalformedCommit::DuplicateChangeTarget(key));
                }
                filesystem_target_seen = true;
                filesystem_target_only_allocations &= is_filesystem_allocation;
            } else if !inserted {
                return Err(MalformedCommit::DuplicateChangeTarget(key));
            }
        }
    }
    Ok(keys.into_iter().collect())
}

/// Persisted semantic-key tag or component was malformed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyCodecError {
    /// A fixed-width semantic identity had the wrong byte count.
    FixedWidth {
        /// Stable component field name.
        field: &'static str,
        /// Observed byte count.
        actual: usize,
    },
    /// A persisted family tag is unknown to version 1.
    UnknownFamilyTag(i16),
    /// Components do not match the persisted family tag.
    InvalidComponentShape {
        /// Persisted family tag.
        family_tag: i16,
    },
    /// A byte-exact name violated its finalized bounds or syntax.
    Bounded(BoundedValueError),
}

impl fmt::Display for KeyCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FixedWidth { field, actual } => {
                write!(formatter, "{field} has {actual} bytes, expected 16")
            }
            Self::UnknownFamilyTag(tag) => write!(formatter, "unknown record family tag {tag}"),
            Self::InvalidComponentShape { family_tag } => {
                write!(
                    formatter,
                    "invalid component shape for family tag {family_tag}"
                )
            }
            Self::Bounded(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for KeyCodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bounded(error) => Some(error),
            _ => None,
        }
    }
}

fn fixed_component(field: &'static str, bytes: Vec<u8>) -> Result<[u8; 16], KeyCodecError> {
    let actual = bytes.len();
    bytes
        .try_into()
        .map_err(|_| KeyCodecError::FixedWidth { field, actual })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(limits: StateLimits) -> Vec<RecordKey> {
        let fs = FilesystemId::from_u128(1);
        let inode = InodeId::from_u128(2);
        vec![
            RecordKey::Filesystem(fs),
            RecordKey::Inode(fs, inode),
            RecordKey::DirectoryEntry(
                fs,
                inode,
                EntryName::new(b"entry".to_vec(), limits).unwrap(),
            ),
            RecordKey::Open(fs, OpenId::from_u128(3)),
            RecordKey::OpenPin(fs, inode, OpenId::from_u128(3)),
            RecordKey::Orphan(fs, inode),
            RecordKey::Lock(fs, inode, LockId::from_u128(4)),
            RecordKey::Xattr(
                fs,
                inode,
                XattrName::new(b"user.key".to_vec(), limits).unwrap(),
            ),
            RecordKey::XattrStaging(fs, XattrStagingId::from_u128(5)),
            RecordKey::Mutation(fs, MutationId::from_u128(6)),
            RecordKey::WriterLease(fs, WriterScopeId::from_u128(7)),
        ]
    }

    #[test]
    fn sql_tags_round_trip_every_family_and_preserve_order() {
        let limits = StateLimits::default();
        let keys = keys(limits);
        let encoded: Vec<_> = keys.iter().map(SqlRecordKey::encode).collect();
        let mut sorted = encoded.clone();
        sorted.sort();
        assert_eq!(encoded, sorted);
        for (key, sql_key) in keys.into_iter().zip(encoded) {
            assert_eq!(sql_key.decode(limits), Ok(key));
        }
    }

    #[test]
    fn duplicate_affected_keys_are_rejected_canonically() {
        let filesystem_id = FilesystemId::from_u128(1);
        let inode_id = InodeId::from_u128(2);
        let changes = [
            StateChange::BumpInodeGeneration(inode_id),
            StateChange::BumpInodeGeneration(inode_id),
        ];
        assert!(matches!(
            canonical_affected_keys(filesystem_id, &changes),
            Err(MalformedCommit::DuplicateChangeTarget(RecordKey::Inode(
                _,
                _
            )))
        ));
    }
}
