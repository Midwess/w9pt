//! Typed filesystem completion results.

use crate::protocol::{DirectoryEntry, FileAttributes, Lock, LockStatus, Qid, Statfs};

use super::{CapabilitySet, ExportId, ObjectHandle, OpenHandle, PrincipalId, XattrHandle};

/// Export root and enforceable guarantees selected by successful attach policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachResult {
    /// Principal bound to all operations derived from the root fid.
    pub principal: PrincipalId,
    /// Export bound to all fids derived from the root fid.
    pub export: ExportId,
    /// Opaque root object handle.
    pub root: ObjectHandle,
    /// Stable root identity returned on the wire.
    pub qid: Qid,
    /// Operations and semantic guarantees the backend promises for this export.
    pub capabilities: CapabilitySet,
}

/// One successfully resolved walk prefix element.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WalkElement {
    /// Resolved object for subsequent operations.
    pub object: ObjectHandle,
    /// Stable identity returned to the client.
    pub qid: Qid,
}

/// Successful full or non-empty partial walk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WalkResult {
    /// Resolved prefix, in input component order.
    pub elements: Vec<WalkElement>,
}

/// Existing object opened for I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenResult {
    /// Current stable object identity.
    pub qid: Qid,
    /// Backend open-instance handle.
    pub open: OpenHandle,
    /// Backend-selected preferred I/O unit, or zero.
    pub io_unit: u32,
}

/// Regular file created and opened, replacing the parent fid's object state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreateResult {
    /// Newly created object handle.
    pub object: ObjectHandle,
    /// Stable object identity.
    pub qid: Qid,
    /// Backend open-instance handle.
    pub open: OpenHandle,
    /// Backend-selected preferred I/O unit, or zero.
    pub io_unit: u32,
}

/// Newly created directory, special node, or symbolic link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeResult {
    /// Newly created object handle.
    pub object: ObjectHandle,
    /// Stable object identity returned to the client.
    pub qid: Qid,
}

/// Readable xattr stream resolved by `XattrWalk`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XattrWalkResult {
    /// Backend xattr stream handle.
    pub xattr: XattrHandle,
    /// Exact stream size returned by `Rxattrwalk`.
    pub size: u64,
}

/// Writable xattr staging stream prepared by `XattrCreate`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XattrCreateResult {
    /// Backend staging handle committed on fid clunk.
    pub xattr: XattrHandle,
}

/// Discriminant used to validate filesystem completions exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemResultKind {
    /// Walk result.
    Walked,
    /// Fid resources released.
    Released,
    /// Existing object opened.
    Opened,
    /// Regular file created/opened.
    Created,
    /// Directory created.
    DirectoryCreated,
    /// Special node created.
    NodeCreated,
    /// Symbolic link created.
    SymlinkCreated,
    /// File bytes read.
    Read,
    /// File bytes written.
    Written,
    /// Directory records read.
    DirectoryRead,
    /// Durability barrier completed.
    Synced,
    /// Filesystem statistics returned.
    Statfs,
    /// Attributes returned.
    Attributes,
    /// Attributes updated.
    AttributesSet,
    /// Symbolic-link target returned.
    LinkTarget,
    /// Fid-based rename completed.
    Renamed,
    /// Directory-relative rename completed.
    RenamedAt,
    /// Remove-by-object completed.
    Removed,
    /// Named unlink completed.
    Unlinked,
    /// Hard link created.
    Linked,
    /// Xattr stream resolved.
    XattrWalked,
    /// Xattr staging stream created.
    XattrCreated,
    /// Xattr bytes read.
    XattrRead,
    /// Xattr bytes staged.
    XattrWritten,
    /// Xattr staged value committed.
    XattrCommitted,
    /// Lock operation completed.
    Locked,
    /// Conflicting lock queried.
    LockQueried,
}

/// Successful filesystem completion payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilesystemResult {
    /// Walked a full path or non-empty prefix.
    Walked(WalkResult),
    /// Released all supplied fid resources.
    Released,
    /// Existing object opened.
    Opened(OpenResult),
    /// Regular file created/opened.
    Created(CreateResult),
    /// Directory created.
    DirectoryCreated(NodeResult),
    /// Special node created.
    NodeCreated(NodeResult),
    /// Symbolic link created.
    SymlinkCreated(NodeResult),
    /// Positioned bytes read.
    Read(Vec<u8>),
    /// Positioned bytes committed.
    Written(u32),
    /// Directory entries returned in semantic form.
    DirectoryRead(Vec<DirectoryEntry>),
    /// Requested durability barrier completed.
    Synced,
    /// Filesystem-wide statistics.
    Statfs(Statfs),
    /// Object attributes.
    Attributes(FileAttributes),
    /// Selected attributes updated.
    AttributesSet,
    /// Symbolic-link target.
    LinkTarget(String),
    /// Fid-based rename completed.
    Renamed,
    /// Directory-relative rename completed.
    RenamedAt,
    /// Remove-by-object completed.
    Removed,
    /// Named unlink completed.
    Unlinked,
    /// Hard link created.
    Linked,
    /// Readable xattr stream resolved.
    XattrWalked(XattrWalkResult),
    /// Writable xattr staging stream created.
    XattrCreated(XattrCreateResult),
    /// Xattr bytes read.
    XattrRead(Vec<u8>),
    /// Xattr bytes staged.
    XattrWritten(u32),
    /// Xattr value atomically committed.
    XattrCommitted,
    /// Lock mutation result.
    Locked(LockStatus),
    /// Conflicting lock result.
    LockQueried(Lock),
}

impl FilesystemResult {
    /// Returns the exact result discriminant.
    pub const fn kind(&self) -> FilesystemResultKind {
        match self {
            Self::Walked(_) => FilesystemResultKind::Walked,
            Self::Released => FilesystemResultKind::Released,
            Self::Opened(_) => FilesystemResultKind::Opened,
            Self::Created(_) => FilesystemResultKind::Created,
            Self::DirectoryCreated(_) => FilesystemResultKind::DirectoryCreated,
            Self::NodeCreated(_) => FilesystemResultKind::NodeCreated,
            Self::SymlinkCreated(_) => FilesystemResultKind::SymlinkCreated,
            Self::Read(_) => FilesystemResultKind::Read,
            Self::Written(_) => FilesystemResultKind::Written,
            Self::DirectoryRead(_) => FilesystemResultKind::DirectoryRead,
            Self::Synced => FilesystemResultKind::Synced,
            Self::Statfs(_) => FilesystemResultKind::Statfs,
            Self::Attributes(_) => FilesystemResultKind::Attributes,
            Self::AttributesSet => FilesystemResultKind::AttributesSet,
            Self::LinkTarget(_) => FilesystemResultKind::LinkTarget,
            Self::Renamed => FilesystemResultKind::Renamed,
            Self::RenamedAt => FilesystemResultKind::RenamedAt,
            Self::Removed => FilesystemResultKind::Removed,
            Self::Unlinked => FilesystemResultKind::Unlinked,
            Self::Linked => FilesystemResultKind::Linked,
            Self::XattrWalked(_) => FilesystemResultKind::XattrWalked,
            Self::XattrCreated(_) => FilesystemResultKind::XattrCreated,
            Self::XattrRead(_) => FilesystemResultKind::XattrRead,
            Self::XattrWritten(_) => FilesystemResultKind::XattrWritten,
            Self::XattrCommitted => FilesystemResultKind::XattrCommitted,
            Self::Locked(_) => FilesystemResultKind::Locked,
            Self::LockQueried(_) => FilesystemResultKind::LockQueried,
        }
    }
}
