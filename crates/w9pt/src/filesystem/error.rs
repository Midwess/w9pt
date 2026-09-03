//! Stable filesystem semantic failures and target-independent Linux errno values.

use core::fmt;

/// A project-owned Linux errno number used by `9P2000.L` `Rlerror` replies.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LinuxErrno(u32);

impl LinuxErrno {
    /// Operation not permitted.
    pub const EPERM: Self = Self(1);
    /// No such file or directory.
    pub const ENOENT: Self = Self(2);
    /// Interrupted operation.
    pub const EINTR: Self = Self(4);
    /// I/O error.
    pub const EIO: Self = Self(5);
    /// Bad fid/open handle.
    pub const EBADF: Self = Self(9);
    /// Try again.
    pub const EAGAIN: Self = Self(11);
    /// Out of memory or configured capacity.
    pub const ENOMEM: Self = Self(12);
    /// Permission denied.
    pub const EACCES: Self = Self(13);
    /// Resource busy.
    pub const EBUSY: Self = Self(16);
    /// Entry already exists.
    pub const EEXIST: Self = Self(17);
    /// Cross-export link or rename.
    pub const EXDEV: Self = Self(18);
    /// Object is not a directory.
    pub const ENOTDIR: Self = Self(20);
    /// Object is a directory.
    pub const EISDIR: Self = Self(21);
    /// Invalid argument or protocol state.
    pub const EINVAL: Self = Self(22);
    /// No storage space.
    pub const ENOSPC: Self = Self(28);
    /// Read-only export.
    pub const EROFS: Self = Self(30);
    /// Result does not fit supplied buffer.
    pub const ERANGE: Self = Self(34);
    /// Name is too long.
    pub const ENAMETOOLONG: Self = Self(36);
    /// No locks available.
    pub const ENOLCK: Self = Self(37);
    /// Operation is not implemented.
    pub const ENOSYS: Self = Self(38);
    /// Directory is not empty.
    pub const ENOTEMPTY: Self = Self(39);
    /// Too many symbolic links.
    pub const ELOOP: Self = Self(40);
    /// Extended attribute has no data.
    pub const ENODATA: Self = Self(61);
    /// Numeric value overflowed the protocol contract.
    pub const EOVERFLOW: Self = Self(75);
    /// Protocol state or message sequencing error.
    pub const EPROTO: Self = Self(71);
    /// Operation is unsupported by this attached export.
    pub const EOPNOTSUPP: Self = Self(95);
    /// Operation was cancelled.
    pub const ECANCELED: Self = Self(125);

    /// Constructs an errno from an explicitly chosen Linux wire value.
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    /// Returns the target-independent `u32` carried on the wire.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for LinuxErrno {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Linux errno {}", self.0)
    }
}

/// Backend semantic failure safe to map to a client-visible errno.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemError {
    /// Stable error code sent to the client.
    pub errno: LinuxErrno,
}

impl FilesystemError {
    /// Creates a filesystem error without backend-private diagnostic text.
    pub const fn new(errno: LinuxErrno) -> Self {
        Self { errno }
    }
}

impl fmt::Display for FilesystemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.errno.fmt(formatter)
    }
}

impl std::error::Error for FilesystemError {}
