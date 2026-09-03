//! Typed request and response messages for the declared operation matrix.

use super::{
    Fid, FileAttributes, GetattrMask, LinuxWireError, Lock, LockRequest, LockStatus, MessageType,
    OpenFlags, Qid, SetAttributes, Statfs, Tag, UnlinkFlags, XattrFlags,
};

/// A decoded client request with its correlation tag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    /// Client-selected request tag.
    pub tag: Tag,
    /// Typed request payload.
    pub body: RequestBody,
}

impl Request {
    /// Returns the exact wire message number for this request.
    pub const fn message_type(&self) -> MessageType {
        self.body.message_type()
    }
}

/// Payload of any supported client request.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum RequestBody {
    /// Negotiate dialect and maximum frame size.
    Version { msize: u32, version: String },
    /// Start a host-defined authentication exchange.
    Auth {
        afid: Fid,
        uname: String,
        aname: String,
        n_uname: u32,
    },
    /// Attach a fid to an authorized export root.
    Attach {
        fid: Fid,
        afid: Fid,
        uname: String,
        aname: String,
        n_uname: u32,
    },
    /// Suppress a pending request's reply.
    Flush { old_tag: Tag },
    /// Clone a fid and optionally walk path components.
    Walk {
        fid: Fid,
        new_fid: Fid,
        names: Vec<String>,
    },
    /// Retire a fid.
    Clunk { fid: Fid },
    /// Open an existing object.
    Lopen { fid: Fid, flags: OpenFlags },
    /// Create and open a regular file relative to `fid`.
    Lcreate {
        fid: Fid,
        name: String,
        flags: OpenFlags,
        mode: u32,
        gid: u32,
    },
    /// Create a directory relative to `directory`.
    Mkdir {
        directory: Fid,
        name: String,
        mode: u32,
        gid: u32,
    },
    /// Create a device or other special node.
    Mknod {
        directory: Fid,
        name: String,
        mode: u32,
        major: u32,
        minor: u32,
        gid: u32,
    },
    /// Create a symbolic link.
    Symlink {
        directory: Fid,
        name: String,
        target: String,
        gid: u32,
    },
    /// Read from an open file or authentication/xattr stream.
    Read { fid: Fid, offset: u64, count: u32 },
    /// Write to an open file or authentication/xattr stream.
    Write {
        fid: Fid,
        offset: u64,
        data: Vec<u8>,
    },
    /// Read encoded directory entries from an opened directory.
    Readdir { fid: Fid, offset: u64, count: u32 },
    /// Request a durability barrier.
    Fsync { fid: Fid, data_only: bool },
    /// Query filesystem-wide statistics.
    Statfs { fid: Fid },
    /// Query object attributes.
    Getattr { fid: Fid, mask: GetattrMask },
    /// Update selected attributes.
    Setattr { fid: Fid, attributes: SetAttributes },
    /// Read a symbolic-link target.
    Readlink { fid: Fid },
    /// Rename the object denoted by `fid` into `directory`.
    Rename {
        fid: Fid,
        directory: Fid,
        name: String,
    },
    /// Atomically rename a directory entry.
    RenameAt {
        old_directory: Fid,
        old_name: String,
        new_directory: Fid,
        new_name: String,
    },
    /// Remove the object denoted by `fid` and retire the fid.
    Remove { fid: Fid },
    /// Remove a named entry relative to a directory.
    UnlinkAt {
        directory: Fid,
        name: String,
        flags: UnlinkFlags,
    },
    /// Create a hard link to `target` in `directory`.
    Link {
        directory: Fid,
        target: Fid,
        name: String,
    },
    /// Bind `new_fid` to one xattr value or to the xattr-name list.
    XattrWalk {
        fid: Fid,
        new_fid: Fid,
        name: String,
    },
    /// Convert `fid` into a writable xattr stream.
    XattrCreate {
        fid: Fid,
        name: String,
        size: u64,
        flags: XattrFlags,
    },
    /// Acquire, release, or reclaim a byte-range lock.
    Lock { fid: Fid, lock: LockRequest },
    /// Query the conflicting byte-range lock, if any.
    Getlock { fid: Fid, lock: Lock },
}

impl RequestBody {
    /// Returns the exact request message number.
    pub const fn message_type(&self) -> MessageType {
        match self {
            Self::Version { .. } => MessageType::Tversion,
            Self::Auth { .. } => MessageType::Tauth,
            Self::Attach { .. } => MessageType::Tattach,
            Self::Flush { .. } => MessageType::Tflush,
            Self::Walk { .. } => MessageType::Twalk,
            Self::Clunk { .. } => MessageType::Tclunk,
            Self::Lopen { .. } => MessageType::Tlopen,
            Self::Lcreate { .. } => MessageType::Tlcreate,
            Self::Mkdir { .. } => MessageType::Tmkdir,
            Self::Mknod { .. } => MessageType::Tmknod,
            Self::Symlink { .. } => MessageType::Tsymlink,
            Self::Read { .. } => MessageType::Tread,
            Self::Write { .. } => MessageType::Twrite,
            Self::Readdir { .. } => MessageType::Treaddir,
            Self::Fsync { .. } => MessageType::Tfsync,
            Self::Statfs { .. } => MessageType::Tstatfs,
            Self::Getattr { .. } => MessageType::Tgetattr,
            Self::Setattr { .. } => MessageType::Tsetattr,
            Self::Readlink { .. } => MessageType::Treadlink,
            Self::Rename { .. } => MessageType::Trename,
            Self::RenameAt { .. } => MessageType::Trenameat,
            Self::Remove { .. } => MessageType::Tremove,
            Self::UnlinkAt { .. } => MessageType::Tunlinkat,
            Self::Link { .. } => MessageType::Tlink,
            Self::XattrWalk { .. } => MessageType::Txattrwalk,
            Self::XattrCreate { .. } => MessageType::Txattrcreate,
            Self::Lock { .. } => MessageType::Tlock,
            Self::Getlock { .. } => MessageType::Tgetlock,
        }
    }
}

/// A server response ready for checked wire encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response {
    /// Tag copied from the originating request.
    pub tag: Tag,
    /// Typed response payload.
    pub body: ResponseBody,
}

impl Response {
    /// Returns the exact wire message number for this response.
    pub const fn message_type(&self) -> MessageType {
        self.body.message_type()
    }
}

/// Payload of any supported server response.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum ResponseBody {
    /// Negotiated dialect and frame size.
    Version { msize: u32, version: String },
    /// Host-defined authentication fid identity.
    Auth { qid: Qid },
    /// Attached export root identity.
    Attach { qid: Qid },
    /// Flush reached its ordering point.
    Flush,
    /// QIDs resolved by a walk, including a valid partial walk.
    Walk { qids: Vec<Qid> },
    /// Fid retired.
    Clunk,
    /// Object opened.
    Lopen { qid: Qid, io_unit: u32 },
    /// Regular file created and opened.
    Lcreate { qid: Qid, io_unit: u32 },
    /// Directory created.
    Mkdir { qid: Qid },
    /// Special node created.
    Mknod { qid: Qid },
    /// Symbolic link created.
    Symlink { qid: Qid },
    /// Positioned data bytes.
    Read { data: Vec<u8> },
    /// Number of write bytes committed.
    Write { count: u32 },
    /// Encoded complete directory-entry records.
    Readdir { data: Vec<u8> },
    /// Durability barrier completed.
    Fsync,
    /// Filesystem-wide statistics.
    Statfs(Statfs),
    /// Object attributes.
    Getattr(FileAttributes),
    /// Selected attributes updated.
    Setattr,
    /// Symbolic-link target.
    Readlink { target: String },
    /// Fid-based rename completed.
    Rename,
    /// Directory-relative rename completed.
    RenameAt,
    /// Remove-by-fid completed.
    Remove,
    /// Directory-relative unlink completed.
    UnlinkAt,
    /// Hard link created.
    Link,
    /// Xattr stream bound with its exact byte size.
    XattrWalk { size: u64 },
    /// Writable xattr stream prepared.
    XattrCreate,
    /// Lock operation status.
    Lock { status: LockStatus },
    /// Conflicting lock, or `Unlock` when none exists.
    Getlock { lock: Lock },
    /// Stable Linux errno response for any valid tagged request.
    Lerror(LinuxWireError),
}

impl ResponseBody {
    /// Returns the exact response message number.
    pub const fn message_type(&self) -> MessageType {
        match self {
            Self::Version { .. } => MessageType::Rversion,
            Self::Auth { .. } => MessageType::Rauth,
            Self::Attach { .. } => MessageType::Rattach,
            Self::Flush => MessageType::Rflush,
            Self::Walk { .. } => MessageType::Rwalk,
            Self::Clunk => MessageType::Rclunk,
            Self::Lopen { .. } => MessageType::Rlopen,
            Self::Lcreate { .. } => MessageType::Rlcreate,
            Self::Mkdir { .. } => MessageType::Rmkdir,
            Self::Mknod { .. } => MessageType::Rmknod,
            Self::Symlink { .. } => MessageType::Rsymlink,
            Self::Read { .. } => MessageType::Rread,
            Self::Write { .. } => MessageType::Rwrite,
            Self::Readdir { .. } => MessageType::Rreaddir,
            Self::Fsync => MessageType::Rfsync,
            Self::Statfs(_) => MessageType::Rstatfs,
            Self::Getattr(_) => MessageType::Rgetattr,
            Self::Setattr => MessageType::Rsetattr,
            Self::Readlink { .. } => MessageType::Rreadlink,
            Self::Rename => MessageType::Rrename,
            Self::RenameAt => MessageType::Rrenameat,
            Self::Remove => MessageType::Rremove,
            Self::UnlinkAt => MessageType::Runlinkat,
            Self::Link => MessageType::Rlink,
            Self::XattrWalk { .. } => MessageType::Rxattrwalk,
            Self::XattrCreate => MessageType::Rxattrcreate,
            Self::Lock { .. } => MessageType::Rlock,
            Self::Getlock { .. } => MessageType::Rgetlock,
            Self::Lerror(_) => MessageType::Rlerror,
        }
    }
}
