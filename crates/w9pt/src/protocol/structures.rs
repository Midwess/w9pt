//! Composite structures with exact stock `9P2000.L` wire representations.

use super::{GetattrMask, LockFlags, LockType, Qid, SetattrMask};
use crate::filesystem::LinuxErrno;

/// Validation failure for a decoded or backend-supplied composite wire value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructureError {
    /// A nanosecond fraction is at least one billion.
    InvalidNanoseconds,
    /// A field-selection mask includes undefined bits.
    UnknownMaskBits,
    /// A component name is empty or contains a slash/NUL byte.
    InvalidComponentName,
    /// An xattr name is invalid for the requested operation.
    InvalidXattrName,
    /// Mutually exclusive xattr create/replace flags were combined.
    ConflictingXattrFlags,
    /// A lock request contains undefined behavior flags.
    UnknownLockFlags,
}

/// Newtype used by the wire response model without exposing backend diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinuxWireError(pub LinuxErrno);

/// Filesystem-wide statistics returned by `Rstatfs`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Statfs {
    /// Filesystem type identifier.
    pub ty: u32,
    /// Fundamental block size.
    pub block_size: u32,
    /// Total blocks.
    pub blocks: u64,
    /// Free blocks.
    pub blocks_free: u64,
    /// Free blocks available to the attached principal.
    pub blocks_available: u64,
    /// Total file nodes.
    pub files: u64,
    /// Free file nodes.
    pub files_free: u64,
    /// Filesystem identifier.
    pub filesystem_id: u64,
    /// Maximum component name length.
    pub max_name_length: u32,
}

/// Seconds and nanoseconds since the Unix epoch, supplied by the backend/host.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Timestamp {
    /// Whole seconds.
    pub seconds: u64,
    /// Fractional nanoseconds; less than one billion when valid.
    pub nanoseconds: u64,
}

impl Timestamp {
    /// Returns whether the nanosecond fraction is valid.
    pub const fn is_valid(self) -> bool {
        self.nanoseconds < 1_000_000_000
    }
}

/// Complete fixed-size attribute payload returned by `Rgetattr`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FileAttributes {
    /// Fields whose values are meaningful.
    pub valid: GetattrMask,
    /// Stable object identity.
    pub qid: Qid,
    /// Linux file type and permission bits.
    pub mode: u32,
    /// Owner user ID.
    pub uid: u32,
    /// Owner group ID.
    pub gid: u32,
    /// Hard-link count.
    pub link_count: u64,
    /// Device number for a special node.
    pub device: u64,
    /// Logical byte length.
    pub size: u64,
    /// Preferred I/O block size.
    pub block_size: u64,
    /// Allocated 512-byte blocks.
    pub blocks: u64,
    /// Last access time.
    pub accessed: Timestamp,
    /// Last content modification time.
    pub modified: Timestamp,
    /// Last metadata change time.
    pub changed: Timestamp,
    /// Creation time.
    pub created: Timestamp,
    /// Inode generation.
    pub generation: u64,
    /// Backend-defined content version.
    pub data_version: u64,
}

impl FileAttributes {
    /// Validates nanosecond fields selected by the valid mask.
    pub const fn has_valid_timestamps(self) -> bool {
        (!self.valid.contains(GetattrMask::ATIME) || self.accessed.is_valid())
            && (!self.valid.contains(GetattrMask::MTIME) || self.modified.is_valid())
            && (!self.valid.contains(GetattrMask::CTIME) || self.changed.is_valid())
            && (!self.valid.contains(GetattrMask::BTIME) || self.created.is_valid())
    }

    /// Checks masks and selected timestamp fractions.
    pub const fn validate(self) -> Result<(), StructureError> {
        if self.valid.bits() & !GetattrMask::ALL.bits() != 0 {
            return Err(StructureError::UnknownMaskBits);
        }
        if !self.has_valid_timestamps() {
            return Err(StructureError::InvalidNanoseconds);
        }
        Ok(())
    }
}

/// Fields supplied by `Tsetattr`; `valid` says which values take effect.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SetAttributes {
    /// Selected update fields and time-source rules.
    pub valid: SetattrMask,
    /// Linux mode bits.
    pub mode: u32,
    /// Owner user ID.
    pub uid: u32,
    /// Owner group ID.
    pub gid: u32,
    /// New logical size.
    pub size: u64,
    /// New or caller-placeholder access time.
    pub accessed: Timestamp,
    /// New or caller-placeholder modification time.
    pub modified: Timestamp,
}

impl SetAttributes {
    /// Validates explicitly supplied nanosecond fractions.
    pub const fn has_valid_timestamps(self) -> bool {
        (!self.valid.contains(SetattrMask::ATIME_SET) || self.accessed.is_valid())
            && (!self.valid.contains(SetattrMask::MTIME_SET) || self.modified.is_valid())
    }

    /// Checks mask dependencies and explicitly selected timestamp fractions.
    pub const fn validate(self) -> Result<(), StructureError> {
        const KNOWN: u32 = 0x1ff;
        if self.valid.bits() & !KNOWN != 0 {
            return Err(StructureError::UnknownMaskBits);
        }
        if self.valid.contains(SetattrMask::ATIME_SET) && !self.valid.contains(SetattrMask::ATIME) {
            return Err(StructureError::UnknownMaskBits);
        }
        if self.valid.contains(SetattrMask::MTIME_SET) && !self.valid.contains(SetattrMask::MTIME) {
            return Err(StructureError::UnknownMaskBits);
        }
        if !self.has_valid_timestamps() {
            return Err(StructureError::InvalidNanoseconds);
        }
        Ok(())
    }
}

/// One variable-length directory record carried by `Rreaddir` data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    /// Stable object identity.
    pub qid: Qid,
    /// Cookie used as the next request's offset.
    pub offset: u64,
    /// Linux directory-entry type byte.
    pub ty: u8,
    /// Single component name.
    pub name: String,
}

impl DirectoryEntry {
    /// Checks that a directory record contains one valid path component.
    pub fn validate(&self) -> Result<(), StructureError> {
        validate_component(&self.name)
    }
}

/// Record-lock owner and byte range carried by `Tgetlock`/`Rgetlock`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lock {
    /// Read, write, or unlock/no-conflict.
    pub ty: LockType,
    /// Inclusive range start.
    pub start: u64,
    /// Byte length; zero conventionally means through end-of-file.
    pub length: u64,
    /// Host process identifier from the client.
    pub process_id: u32,
    /// Client-selected lock-owner namespace.
    pub client_id: String,
}

/// Record-lock mutation request with blocking/reclaim flags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockRequest {
    /// Requested lock and owner.
    pub lock: Lock,
    /// Blocking and reclaim behavior.
    pub flags: LockFlags,
}

impl LockRequest {
    /// Rejects flag bits not defined by stock `9P2000.L`.
    pub const fn validate(&self) -> Result<(), StructureError> {
        if self.flags.bits() & !0x3 != 0 {
            return Err(StructureError::UnknownLockFlags);
        }
        Ok(())
    }
}

/// User identity carried by the 9P2000.u-compatible auth and attach layouts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserIdentity {
    /// Textual user name, which may be empty when a numeric ID is supplied.
    pub name: String,
    /// Numeric user ID when the wire value is not [`super::NO_NUMERIC_USER`].
    pub numeric_id: Option<u32>,
}

impl UserIdentity {
    /// Converts exact wire fields into their typed optional form.
    pub fn from_wire(name: String, numeric_id: u32) -> Self {
        Self {
            name,
            numeric_id: (numeric_id != super::NO_NUMERIC_USER).then_some(numeric_id),
        }
    }

    /// Returns the exact sentinel-bearing numeric wire value.
    pub const fn numeric_wire_value(&self) -> u32 {
        match self.numeric_id {
            Some(value) => value,
            None => super::NO_NUMERIC_USER,
        }
    }
}

/// Validated extended-attribute name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XattrName(String);

impl XattrName {
    /// Validates a non-empty xattr name for create/update operations.
    pub fn new(name: impl Into<String>) -> Result<Self, StructureError> {
        let name = name.into();
        if name.is_empty() || name.as_bytes().contains(&0) {
            return Err(StructureError::InvalidXattrName);
        }
        Ok(Self(name))
    }

    /// Borrows the validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validates the two stock xattr creation flags.
    pub const fn validate_flags(flags: super::XattrFlags) -> Result<(), StructureError> {
        if flags.bits() & !0x3 != 0 {
            return Err(StructureError::UnknownMaskBits);
        }
        if flags.contains(super::XattrFlags::CREATE) && flags.contains(super::XattrFlags::REPLACE) {
            return Err(StructureError::ConflictingXattrFlags);
        }
        Ok(())
    }
}

fn validate_component(name: &str) -> Result<(), StructureError> {
    if name.is_empty() || name.as_bytes().contains(&b'/') || name.as_bytes().contains(&0) {
        return Err(StructureError::InvalidComponentName);
    }
    Ok(())
}
