//! Backend-neutral semantic operations emitted by the protocol session.

use crate::protocol::{
    GetattrMask, Lock, LockRequest, OpenFlags, SetAttributes, UnlinkFlags, XattrFlags,
};

use super::{ObjectHandle, OpenHandle, RequestContext, XattrHandle};

/// Filesystem effect with the attach-bound context used for atomic authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemRequest {
    /// Principal/export/session context for authorization and cross-session coordination.
    pub context: RequestContext,
    /// High-level operation with no wire or storage-engine representation.
    pub operation: FilesystemOperation,
}

impl FilesystemRequest {
    /// Constructs semantic work in one attached export.
    pub const fn new(context: RequestContext, operation: FilesystemOperation) -> Self {
        Self { context, operation }
    }

    /// Exact successful result variant required by this operation.
    pub const fn expected_result(&self) -> super::FilesystemResultKind {
        self.operation.expected_result()
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.context
            .principal
            .as_str()
            .len()
            .saturating_add(self.context.export.as_str().len())
            .saturating_add(self.operation.retained_bytes())
    }
}

/// Semantic filesystem operations. Implementations atomically authorize and execute mutations.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum FilesystemOperation {
    /// Resolve as much of a component sequence as possible from `start`.
    Walk {
        start: ObjectHandle,
        names: Vec<String>,
    },
    /// Release backend live state owned by a retiring fid.
    Release {
        object: ObjectHandle,
        open: Option<OpenHandle>,
        xattr: Option<XattrHandle>,
    },
    /// Open an existing object.
    Open {
        object: ObjectHandle,
        flags: OpenFlags,
    },
    /// Atomically authorize/create/open a regular file in `directory`.
    Create {
        directory: ObjectHandle,
        name: String,
        flags: OpenFlags,
        mode: u32,
        gid: u32,
    },
    /// Atomically create a directory.
    Mkdir {
        directory: ObjectHandle,
        name: String,
        mode: u32,
        gid: u32,
    },
    /// Atomically create a special node.
    Mknod {
        directory: ObjectHandle,
        name: String,
        mode: u32,
        major: u32,
        minor: u32,
        gid: u32,
    },
    /// Atomically create a symbolic link.
    Symlink {
        directory: ObjectHandle,
        name: String,
        target: String,
        gid: u32,
    },
    /// Positioned read from an opaque open handle.
    Read {
        open: OpenHandle,
        offset: u64,
        count: u32,
    },
    /// Positioned write to an opaque open handle.
    Write {
        open: OpenHandle,
        offset: u64,
        data: Vec<u8>,
    },
    /// Read directory entries starting after a backend cookie.
    ReadDir {
        open: OpenHandle,
        offset: u64,
        count: u32,
    },
    /// Data-only or full durability barrier for an open instance.
    Fsync { open: OpenHandle, data_only: bool },
    /// Query filesystem-wide statistics for an object/export.
    Statfs { object: ObjectHandle },
    /// Query selected object attributes.
    Getattr {
        object: ObjectHandle,
        mask: GetattrMask,
    },
    /// Atomically authorize and update selected object attributes.
    Setattr {
        object: ObjectHandle,
        attributes: SetAttributes,
    },
    /// Read a symbolic-link target.
    Readlink { object: ObjectHandle },
    /// Atomically rename `object` into another directory.
    Rename {
        object: ObjectHandle,
        directory: ObjectHandle,
        name: String,
    },
    /// Atomically rename one named entry, potentially across directories.
    RenameAt {
        old_directory: ObjectHandle,
        old_name: String,
        new_directory: ObjectHandle,
        new_name: String,
    },
    /// Atomically remove the name represented by an object fid.
    Remove {
        object: ObjectHandle,
        open: Option<OpenHandle>,
        xattr: Option<XattrHandle>,
    },
    /// Atomically unlink a named child.
    UnlinkAt {
        directory: ObjectHandle,
        name: String,
        flags: UnlinkFlags,
    },
    /// Atomically create a hard link to `target`.
    Link {
        directory: ObjectHandle,
        target: ObjectHandle,
        name: String,
    },
    /// Resolve one xattr or the xattr-name list into a readable stream.
    XattrWalk { object: ObjectHandle, name: String },
    /// Prepare a fixed-size writable xattr stream.
    XattrCreate {
        object: ObjectHandle,
        name: String,
        size: u64,
        flags: XattrFlags,
    },
    /// Positioned read from an xattr stream.
    XattrRead {
        xattr: XattrHandle,
        offset: u64,
        count: u32,
    },
    /// Positioned write into an xattr staging stream.
    XattrWrite {
        xattr: XattrHandle,
        offset: u64,
        data: Vec<u8>,
    },
    /// Atomically publish and release a staged xattr stream.
    XattrCommit {
        xattr: XattrHandle,
        expected_size: u64,
    },
    /// Acquire, release, or reclaim a byte-range lock.
    Lock { open: OpenHandle, lock: LockRequest },
    /// Query a conflicting byte-range lock.
    Getlock { open: OpenHandle, lock: Lock },
}

impl FilesystemOperation {
    /// Exact result kind that can successfully complete this operation.
    pub const fn expected_result(&self) -> super::FilesystemResultKind {
        use super::FilesystemResultKind as Kind;
        match self {
            Self::Walk { .. } => Kind::Walked,
            Self::Release { .. } => Kind::Released,
            Self::Open { .. } => Kind::Opened,
            Self::Create { .. } => Kind::Created,
            Self::Mkdir { .. } => Kind::DirectoryCreated,
            Self::Mknod { .. } => Kind::NodeCreated,
            Self::Symlink { .. } => Kind::SymlinkCreated,
            Self::Read { .. } => Kind::Read,
            Self::Write { .. } => Kind::Written,
            Self::ReadDir { .. } => Kind::DirectoryRead,
            Self::Fsync { .. } => Kind::Synced,
            Self::Statfs { .. } => Kind::Statfs,
            Self::Getattr { .. } => Kind::Attributes,
            Self::Setattr { .. } => Kind::AttributesSet,
            Self::Readlink { .. } => Kind::LinkTarget,
            Self::Rename { .. } => Kind::Renamed,
            Self::RenameAt { .. } => Kind::RenamedAt,
            Self::Remove { .. } => Kind::Removed,
            Self::UnlinkAt { .. } => Kind::Unlinked,
            Self::Link { .. } => Kind::Linked,
            Self::XattrWalk { .. } => Kind::XattrWalked,
            Self::XattrCreate { .. } => Kind::XattrCreated,
            Self::XattrRead { .. } => Kind::XattrRead,
            Self::XattrWrite { .. } => Kind::XattrWritten,
            Self::XattrCommit { .. } => Kind::XattrCommitted,
            Self::Lock { .. } => Kind::Locked,
            Self::Getlock { .. } => Kind::LockQueried,
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::Walk { names, .. } => names.iter().map(String::len).sum(),
            Self::Create { name, .. }
            | Self::Mkdir { name, .. }
            | Self::Mknod { name, .. }
            | Self::Rename { name, .. }
            | Self::UnlinkAt { name, .. }
            | Self::Link { name, .. }
            | Self::XattrWalk { name, .. }
            | Self::XattrCreate { name, .. } => name.len(),
            Self::Symlink { name, target, .. } => name.len().saturating_add(target.len()),
            Self::Write { data, .. } | Self::XattrWrite { data, .. } => data.len(),
            Self::RenameAt {
                old_name, new_name, ..
            } => old_name.len().saturating_add(new_name.len()),
            Self::Lock { lock, .. } => lock.lock.client_id.len(),
            Self::Getlock { lock, .. } => lock.client_id.len(),
            Self::Release { .. }
            | Self::Open { .. }
            | Self::Read { .. }
            | Self::ReadDir { .. }
            | Self::Fsync { .. }
            | Self::Statfs { .. }
            | Self::Getattr { .. }
            | Self::Setattr { .. }
            | Self::Readlink { .. }
            | Self::Remove { .. }
            | Self::XattrRead { .. }
            | Self::XattrCommit { .. } => 0,
        }
    }
}
