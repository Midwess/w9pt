//! Stable portable filesystem-state identities.

use core::fmt;

macro_rules! fixed_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; 16]);

        impl $name {
            /// Creates an identity from its canonical 16 bytes.
            pub const fn new(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            /// Creates an identity from an integer encoded in big-endian order.
            pub const fn from_u128(value: u128) -> Self {
                Self(value.to_be_bytes())
            }

            /// Returns the canonical bytes.
            pub const fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}({})", stringify!($name), Hex(&self.0))
            }
        }
    };
}

fixed_id!(FilesystemId, "Stable identity of one filesystem authority.");
fixed_id!(InodeId, "Stable, non-reused identity of one inode.");
fixed_id!(OpenId, "Cluster-resolvable identity of one open instance.");
fixed_id!(LockId, "Stable identity of one byte-range lock record.");
fixed_id!(LeaseId, "Stable identity of one granted writer lease.");
fixed_id!(
    LeaseOperationId,
    "Stable replay identity of one acquire, renew, or release operation."
);
fixed_id!(
    WriterScopeId,
    "Portable identity of a scope protected by one writer fence."
);

impl WriterScopeId {
    /// Canonical filesystem-wide scope required by single-writer adapters.
    pub const FILESYSTEM: Self = Self([0; 16]);
}
fixed_id!(
    WriterIncarnationId,
    "Globally unique incarnation of one state-writing process or actor."
);
fixed_id!(
    ClientIncarnationId,
    "Globally unique incarnation of one filesystem client/session lineage."
);
fixed_id!(
    XattrStagingId,
    "Stable identity of one portable extended-attribute staging record."
);

struct Hex<'a>(&'a [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_have_fixed_canonical_bytes_and_debug_text() {
        let filesystem = FilesystemId::from_u128(1);
        let inode = InodeId::new(*filesystem.as_bytes());
        assert_eq!(filesystem.as_bytes().len(), 16);
        assert_eq!(
            format!("{filesystem:?}"),
            "FilesystemId(00000000000000000000000000000001)"
        );
        assert_eq!(
            format!("{inode:?}"),
            "InodeId(00000000000000000000000000000001)"
        );
    }
}
