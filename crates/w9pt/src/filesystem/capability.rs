//! Enforceable attached-export operation and semantic guarantees.

/// One operation or semantic guarantee promised by an attached export.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Capability {
    /// Resolve path components with stable identity.
    Walk = 0,
    /// Open an existing object.
    Open,
    /// Create and open a regular file.
    Create,
    /// Create a directory.
    Mkdir,
    /// Create a special node.
    Mknod,
    /// Create a symbolic link.
    Symlink,
    /// Positioned reads.
    Read,
    /// Positioned writes.
    Write,
    /// Directory enumeration.
    Readdir,
    /// Durability barriers.
    Fsync,
    /// Filesystem statistics.
    Statfs,
    /// Attribute queries.
    Getattr,
    /// Attribute mutations.
    Setattr,
    /// Symbolic-link reads.
    Readlink,
    /// Fid-based rename.
    Rename,
    /// Directory-relative rename.
    RenameAt,
    /// Fid-based remove.
    Remove,
    /// Directory-relative unlink.
    UnlinkAt,
    /// Hard links.
    Link,
    /// Extended attributes.
    Xattr,
    /// POSIX byte-range locks.
    Lock,
    /// Stable object/QID identity for the lifetime of an export.
    StableIdentity,
    /// Mutations authorize and commit atomically against their namespace operands.
    AtomicAuthorization,
    /// Create/rename/link/unlink mutations are atomic to observers.
    AtomicNamespace,
    /// Selected attribute changes commit as one atomic mutation.
    AtomicSetattr,
    /// Positioned I/O preserves explicit offset semantics.
    PositionedIo,
    /// Open objects survive unlink until their open handles are released.
    OpenUnlinked,
    /// Successful data-only barriers make prior file content durable.
    DurableData,
    /// Successful full barriers make prior data and metadata durable.
    DurableMetadata,
    /// Lock ownership and conflicts are coordinated across sessions.
    CrossSessionLocks,
    /// Active operations honor best-effort cancellation requests.
    Cancellation,
}

const ALL_CAPABILITIES: [Capability; 31] = [
    Capability::Walk,
    Capability::Open,
    Capability::Create,
    Capability::Mkdir,
    Capability::Mknod,
    Capability::Symlink,
    Capability::Read,
    Capability::Write,
    Capability::Readdir,
    Capability::Fsync,
    Capability::Statfs,
    Capability::Getattr,
    Capability::Setattr,
    Capability::Readlink,
    Capability::Rename,
    Capability::RenameAt,
    Capability::Remove,
    Capability::UnlinkAt,
    Capability::Link,
    Capability::Xattr,
    Capability::Lock,
    Capability::StableIdentity,
    Capability::AtomicAuthorization,
    Capability::AtomicNamespace,
    Capability::AtomicSetattr,
    Capability::PositionedIo,
    Capability::OpenUnlinked,
    Capability::DurableData,
    Capability::DurableMetadata,
    Capability::CrossSessionLocks,
    Capability::Cancellation,
];

/// Dependency-free capability bitset.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct CapabilitySet(u128);

impl CapabilitySet {
    /// No optional operations or guarantees.
    pub const NONE: Self = Self(0);

    /// Every operation and semantic guarantee defined by this crate version.
    pub const ALL: Self = Self((1u128 << ALL_CAPABILITIES.len()) - 1);

    /// Constructs a set containing the listed capabilities.
    pub fn from_capabilities(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        let mut bits = 0u128;
        for capability in capabilities {
            bits |= 1u128 << capability as u8;
        }
        Self(bits)
    }

    /// Reports whether one promise is present.
    pub const fn contains(self, capability: Capability) -> bool {
        self.0 & (1u128 << capability as u8) != 0
    }

    /// Adds one promise.
    pub const fn with(self, capability: Capability) -> Self {
        Self(self.0 | (1u128 << capability as u8))
    }

    /// Returns raw project-defined bits for persistence or diagnostics.
    pub const fn bits(self) -> u128 {
        self.0
    }

    /// Returns the first missing capability required to execute an operation faithfully.
    ///
    /// Release cleanup is always permitted: capabilities govern new client work, never whether
    /// already-issued backend handles may be retired.
    pub fn require_for(
        self,
        operation: &super::FilesystemOperation,
    ) -> Result<(), CapabilityError> {
        use super::FilesystemOperation as Op;
        use Capability as Cap;

        let required: &[Capability] = match operation {
            Op::Walk { .. } => &[Cap::Walk, Cap::StableIdentity, Cap::AtomicAuthorization],
            Op::Release { .. } => return Ok(()),
            Op::Open { .. } => &[
                Cap::Open,
                Cap::StableIdentity,
                Cap::AtomicAuthorization,
                Cap::OpenUnlinked,
            ],
            Op::Create { .. } => &[
                Cap::Create,
                Cap::StableIdentity,
                Cap::AtomicAuthorization,
                Cap::AtomicNamespace,
                Cap::OpenUnlinked,
            ],
            Op::Mkdir { .. } => &[Cap::Mkdir, Cap::AtomicAuthorization, Cap::AtomicNamespace],
            Op::Mknod { .. } => &[Cap::Mknod, Cap::AtomicAuthorization, Cap::AtomicNamespace],
            Op::Symlink { .. } => &[Cap::Symlink, Cap::AtomicAuthorization, Cap::AtomicNamespace],
            Op::Read { .. } => &[Cap::Read, Cap::AtomicAuthorization, Cap::PositionedIo],
            Op::Write { .. } => &[Cap::Write, Cap::AtomicAuthorization, Cap::PositionedIo],
            Op::ReadDir { .. } => &[Cap::Readdir, Cap::AtomicAuthorization],
            Op::Fsync {
                data_only: true, ..
            } => &[Cap::Fsync, Cap::DurableData],
            Op::Fsync {
                data_only: false, ..
            } => &[Cap::Fsync, Cap::DurableData, Cap::DurableMetadata],
            Op::Statfs { .. } => &[Cap::Statfs, Cap::AtomicAuthorization],
            Op::Getattr { .. } => &[Cap::Getattr, Cap::AtomicAuthorization],
            Op::Setattr { .. } => &[Cap::Setattr, Cap::AtomicAuthorization, Cap::AtomicSetattr],
            Op::Readlink { .. } => &[Cap::Readlink, Cap::AtomicAuthorization],
            Op::Rename { .. } => &[Cap::Rename, Cap::AtomicAuthorization, Cap::AtomicNamespace],
            Op::RenameAt { .. } => &[
                Cap::RenameAt,
                Cap::AtomicAuthorization,
                Cap::AtomicNamespace,
            ],
            Op::Remove { .. } => &[
                Cap::Remove,
                Cap::AtomicAuthorization,
                Cap::AtomicNamespace,
                Cap::OpenUnlinked,
            ],
            Op::UnlinkAt { .. } => &[
                Cap::UnlinkAt,
                Cap::AtomicAuthorization,
                Cap::AtomicNamespace,
                Cap::OpenUnlinked,
            ],
            Op::Link { .. } => &[Cap::Link, Cap::AtomicAuthorization, Cap::AtomicNamespace],
            Op::XattrWalk { .. }
            | Op::XattrCreate { .. }
            | Op::XattrRead { .. }
            | Op::XattrWrite { .. }
            | Op::XattrCommit { .. } => &[Cap::Xattr, Cap::AtomicAuthorization],
            Op::Lock { .. } | Op::Getlock { .. } => {
                &[Cap::Lock, Cap::AtomicAuthorization, Cap::CrossSessionLocks]
            }
        };
        for capability in required {
            if !self.contains(*capability) {
                return Err(CapabilityError {
                    missing: *capability,
                });
            }
        }
        Ok(())
    }
}

/// An export cannot faithfully execute an operation because a promise is absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityError {
    /// First missing operation or semantic guarantee.
    pub missing: Capability,
}

impl core::fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "attached export lacks {:?}", self.missing)
    }
}

impl std::error::Error for CapabilityError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::{FilesystemOperation, ObjectHandle};

    #[test]
    fn operation_bit_without_semantic_guarantees_is_rejected() {
        let capabilities = CapabilitySet::NONE.with(Capability::RenameAt);
        let operation = FilesystemOperation::RenameAt {
            old_directory: ObjectHandle::new(1),
            old_name: "old".into(),
            new_directory: ObjectHandle::new(2),
            new_name: "new".into(),
        };
        assert_eq!(
            capabilities.require_for(&operation),
            Err(CapabilityError {
                missing: Capability::AtomicAuthorization
            })
        );
    }
}
