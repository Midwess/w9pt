//! Deterministic caller-owned identity allocation.

use w9pt_fs_state::{FilesystemId, InodeId, OpenId};
use w9pt_fs_storage::{FileId, MutationId};

/// Domain-separated deterministic allocation scope for one logical mutation slot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IdentityScope {
    /// Filesystem receiving the allocated identity.
    pub filesystem_id: FilesystemId,
    /// Stable logical mutation identity.
    pub mutation_id: MutationId,
    /// Stable domain-local allocation slot within that mutation.
    pub slot: u32,
}

impl IdentityScope {
    /// Constructs an explicit deterministic allocation scope.
    pub const fn new(filesystem_id: FilesystemId, mutation_id: MutationId, slot: u32) -> Self {
        Self {
            filesystem_id,
            mutation_id,
            slot,
        }
    }
}

/// Caller-owned deterministic source for portable first-slice identities.
pub trait IdentitySource: Send + Sync {
    /// Identity-provider failure retained for host diagnostics.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Returns the stable inode identity for this exact scope.
    fn inode_id(&self, scope: IdentityScope) -> Result<InodeId, Self::Error>;

    /// Returns the stable portable open identity for this exact scope.
    fn open_id(&self, scope: IdentityScope) -> Result<OpenId, Self::Error>;

    /// Returns the stable immutable-content file identity for this exact scope.
    fn content_file_id(&self, scope: IdentityScope) -> Result<FileId, Self::Error>;
}
