//! Project-owned `9P2000.L` flags and masks.

macro_rules! bitset {
    ($name:ident, $repr:ty, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
        pub struct $name($repr);

        impl $name {
            /// No bits set.
            pub const EMPTY: Self = Self(0);

            /// Creates a value without interpreting or discarding unknown bits.
            pub const fn from_bits(bits: $repr) -> Self {
                Self(bits)
            }

            /// Returns the exact wire bits.
            pub const fn bits(self) -> $repr {
                self.0
            }

            /// Tests whether every bit in `other` is present.
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }
        }

        impl core::ops::BitOr for $name {
            type Output = Self;

            fn bitor(self, rhs: Self) -> Self::Output {
                Self(self.0 | rhs.0)
            }
        }
    };
}

bitset!(
    OpenFlags,
    u32,
    "Linux open flags carried by `Tlopen` and `Tlcreate`."
);

impl OpenFlags {
    /// Read-only access.
    pub const RDONLY: Self = Self(0);
    /// Write-only access.
    pub const WRONLY: Self = Self(0o1);
    /// Read/write access.
    pub const RDWR: Self = Self(0o2);
    /// Access-mode mask.
    pub const ACCESS_MASK: Self = Self(0o3);
    /// Create if absent.
    pub const CREATE: Self = Self(0o100);
    /// Require exclusive creation.
    pub const EXCL: Self = Self(0o200);
    /// Do not acquire a controlling terminal.
    pub const NOCTTY: Self = Self(0o400);
    /// Truncate on open.
    pub const TRUNC: Self = Self(0o1000);
    /// Append writes.
    pub const APPEND: Self = Self(0o2000);
    /// Nonblocking operation.
    pub const NONBLOCK: Self = Self(0o4000);
    /// Data-only synchronization.
    pub const DSYNC: Self = Self(0o10000);
    /// Asynchronous notification request.
    pub const FASYNC: Self = Self(0o20000);
    /// Direct I/O request.
    pub const DIRECT: Self = Self(0o40000);
    /// Large-file support marker.
    pub const LARGEFILE: Self = Self(0o100000);
    /// Require a directory.
    pub const DIRECTORY: Self = Self(0o200000);
    /// Do not follow the final symlink.
    pub const NOFOLLOW: Self = Self(0o400000);
    /// Do not update access time.
    pub const NOATIME: Self = Self(0o1000000);
    /// Close-on-exec marker.
    pub const CLOEXEC: Self = Self(0o2000000);
    /// Full synchronization.
    pub const SYNC: Self = Self(0o4000000);
}

bitset!(
    GetattrMask,
    u64,
    "Fields requested by `Tgetattr` and valid in `Rgetattr`."
);

impl GetattrMask {
    /// File mode.
    pub const MODE: Self = Self(1 << 0);
    /// Hard-link count.
    pub const NLINK: Self = Self(1 << 1);
    /// Owner user ID.
    pub const UID: Self = Self(1 << 2);
    /// Owner group ID.
    pub const GID: Self = Self(1 << 3);
    /// Device number.
    pub const RDEV: Self = Self(1 << 4);
    /// Access time.
    pub const ATIME: Self = Self(1 << 5);
    /// Modification time.
    pub const MTIME: Self = Self(1 << 6);
    /// Status-change time.
    pub const CTIME: Self = Self(1 << 7);
    /// Inode identity, represented by the QID path.
    pub const INO: Self = Self(1 << 8);
    /// Byte length.
    pub const SIZE: Self = Self(1 << 9);
    /// Allocated block count.
    pub const BLOCKS: Self = Self(1 << 10);
    /// Creation time.
    pub const BTIME: Self = Self(1 << 11);
    /// Inode generation.
    pub const GEN: Self = Self(1 << 12);
    /// Data version.
    pub const DATA_VERSION: Self = Self(1 << 13);
    /// All base stat fields through blocks.
    pub const BASIC: Self = Self(0x07ff);
    /// Every defined field.
    pub const ALL: Self = Self(0x3fff);
}

bitset!(SetattrMask, u32, "Fields selected by `Tsetattr`.");

impl SetattrMask {
    /// File mode.
    pub const MODE: Self = Self(1 << 0);
    /// Owner user ID.
    pub const UID: Self = Self(1 << 1);
    /// Owner group ID.
    pub const GID: Self = Self(1 << 2);
    /// Byte length.
    pub const SIZE: Self = Self(1 << 3);
    /// Access time.
    pub const ATIME: Self = Self(1 << 4);
    /// Modification time.
    pub const MTIME: Self = Self(1 << 5);
    /// Status-change time must be updated.
    pub const CTIME: Self = Self(1 << 6);
    /// Use the supplied access time instead of a host-supplied current time.
    pub const ATIME_SET: Self = Self(1 << 7);
    /// Use the supplied modification time instead of a host-supplied current time.
    pub const MTIME_SET: Self = Self(1 << 8);
}

bitset!(LockFlags, u32, "Record-lock behavior flags.");

impl LockFlags {
    /// Client requests blocking semantics.
    pub const BLOCK: Self = Self(1);
    /// Client is reclaiming a lock after recovery.
    pub const RECLAIM: Self = Self(2);
}

bitset!(UnlinkFlags, u32, "Directory-relative unlink flags.");

impl UnlinkFlags {
    /// Remove a directory instead of a non-directory entry.
    pub const REMOVE_DIR: Self = Self(0x200);
}

bitset!(XattrFlags, u32, "Extended-attribute creation flags.");

impl XattrFlags {
    /// Fail when the attribute already exists.
    pub const CREATE: Self = Self(1);
    /// Fail when the attribute does not exist.
    pub const REPLACE: Self = Self(2);
}

/// POSIX record-lock type encoded by `Tlock` and `Tgetlock`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum LockType {
    /// Shared/read lock.
    Read = 0,
    /// Exclusive/write lock.
    Write = 1,
    /// Unlock request or no conflicting lock.
    Unlock = 2,
}

impl TryFrom<u8> for LockType {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Read),
            1 => Ok(Self::Write),
            2 => Ok(Self::Unlock),
            other => Err(other),
        }
    }
}

/// Result status returned by `Rlock`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum LockStatus {
    /// Lock acquired or released.
    Success = 0,
    /// Conflicting lock currently blocks the request.
    Blocked = 1,
    /// Backend lock error.
    Error = 2,
    /// Backend is in a lock-recovery grace period.
    Grace = 3,
}
