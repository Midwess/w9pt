//! Strong protocol and correlation identifiers.

use core::fmt;

/// Stable identifier supplied by the host for one transport connection.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId(u64);

impl SessionId {
    /// Constructs a session identifier from a host-assigned value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the host-assigned integer value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Opaque monotonically allocated identifier for one externally completed operation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OperationId(u64);

impl OperationId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the session-local integer value for logging and host routing.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Globally routable pair identifying one operation emitted by one session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OperationRoute {
    /// Host-supplied connection identity.
    pub session_id: SessionId,
    /// Never-reused identifier within that session.
    pub operation_id: OperationId,
}

impl OperationRoute {
    /// Constructs a route from the identifiers carried by the session and effect.
    pub const fn new(session_id: SessionId, operation_id: OperationId) -> Self {
        Self {
            session_id,
            operation_id,
        }
    }
}

/// A 16-bit 9P request/response correlation tag.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Tag(u16);

impl Tag {
    /// The reserved tag used by version negotiation.
    pub const NOTAG: Self = Self(u16::MAX);

    /// Constructs a tag from its wire value.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the wire value.
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A session-scoped 32-bit 9P file identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Fid(u32);

impl Fid {
    /// The reserved value used when no authentication fid is supplied.
    pub const NOFID: Self = Self(u32::MAX);

    /// Constructs a fid from its wire value.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the wire value.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Fid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The single-byte QID type bitset.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct QidType(u8);

impl QidType {
    /// Directory.
    pub const DIRECTORY: Self = Self(0x80);
    /// Append-only object.
    pub const APPEND_ONLY: Self = Self(0x40);
    /// Exclusive-use object.
    pub const EXCLUSIVE: Self = Self(0x20);
    /// Mount point.
    pub const MOUNT: Self = Self(0x10);
    /// Authentication stream.
    pub const AUTH: Self = Self(0x08);
    /// Temporary/non-backed-up object.
    pub const TEMPORARY: Self = Self(0x04);
    /// Symbolic link.
    pub const SYMLINK: Self = Self(0x02);
    /// Hard-link marker.
    pub const LINK: Self = Self(0x01);
    /// Ordinary file.
    pub const FILE: Self = Self(0);

    /// Constructs a QID type from its wire bitset.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns the raw wire bitset.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Tests whether every bit in `other` is present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// Stable 9P object identity returned to clients.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Qid {
    /// Object-type flags.
    pub ty: QidType,
    /// Monotonic object version, or zero when the backend does not track one.
    pub version: u32,
    /// Export-stable object path number.
    pub path: u64,
}

impl Qid {
    /// Constructs a QID.
    pub const fn new(ty: QidType, version: u32, path: u64) -> Self {
        Self { ty, version, path }
    }
}
