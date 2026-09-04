//! Bounded byte-exact names, identities, targets, and terminal results.

use core::fmt;

use crate::{StateLimitError, StateLimitKind, StateLimits};

macro_rules! bounded_bytes {
    ($name:ident, $description:literal, $maximum:ident, $kind:ident) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Box<[u8]>);

        impl $name {
            /// Creates a nonempty bounded value without NUL bytes.
            pub fn new(
                value: impl Into<Vec<u8>>,
                limits: StateLimits,
            ) -> Result<Self, BoundedValueError> {
                let value = value.into();
                validate_nonempty(&value, stringify!($name))?;
                validate_no_nul(&value, stringify!($name))?;
                limits
                    .require_bytes(StateLimitKind::$kind, value.len(), limits.$maximum())
                    .map_err(BoundedValueError::Limit)?;
                Ok(Self(value.into_boxed_slice()))
            }

            /// Returns the canonical bytes.
            pub fn as_bytes(&self) -> &[u8] {
                &self.0
            }
        }
    };
}

bounded_bytes!(
    XattrName,
    "Bounded byte-exact extended-attribute name.",
    max_xattr_name_bytes,
    XattrName
);
bounded_bytes!(
    PrincipalId,
    "Bounded portable principal identity.",
    max_principal_bytes,
    Principal
);
bounded_bytes!(
    GroupId,
    "Bounded portable group identity.",
    max_group_bytes,
    Group
);
bounded_bytes!(
    SymlinkTarget,
    "Bounded byte-exact symbolic-link target.",
    max_symlink_bytes,
    Symlink
);

/// Bounded canonical directory entry component.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryName(Box<[u8]>);

impl EntryName {
    /// Creates a valid nonempty component other than `.` or `..`.
    pub fn new(value: impl Into<Vec<u8>>, limits: StateLimits) -> Result<Self, BoundedValueError> {
        let value = value.into();
        validate_nonempty(&value, "EntryName")?;
        validate_no_nul(&value, "EntryName")?;
        if value.contains(&b'/') {
            return Err(BoundedValueError::ForbiddenByte {
                field: "EntryName",
                byte: b'/',
            });
        }
        if value.as_slice() == b"." || value.as_slice() == b".." {
            return Err(BoundedValueError::ReservedComponent);
        }
        limits
            .require_bytes(
                StateLimitKind::EntryName,
                value.len(),
                limits.max_entry_name_bytes(),
            )
            .map_err(BoundedValueError::Limit)?;
        Ok(Self(value.into_boxed_slice()))
    }

    /// Returns the canonical component bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Bounded byte-exact inline extended-attribute or staging value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XattrValue(Box<[u8]>);

impl XattrValue {
    /// Creates a value within the configured metadata bound; empty values are valid.
    pub fn new(value: impl Into<Vec<u8>>, limits: StateLimits) -> Result<Self, BoundedValueError> {
        let value = value.into();
        limits
            .require_bytes(
                StateLimitKind::XattrValue,
                value.len(),
                limits.max_xattr_value_bytes(),
            )
            .map_err(BoundedValueError::Limit)?;
        Ok(Self(value.into_boxed_slice()))
    }

    /// Returns the exact value bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Stable semantic kind of an opaque retained mutation result.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MutationResultKind(u16);

impl MutationResultKind {
    /// Creates a nonzero caller-defined result kind.
    pub const fn new(value: u16) -> Result<Self, BoundedValueError> {
        if value == 0 {
            Err(BoundedValueError::ZeroTag {
                field: "MutationResultKind",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the stable numeric tag.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Codec version of opaque retained mutation-result bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResultFormatVersion(u16);

impl ResultFormatVersion {
    /// Creates a nonzero result codec version.
    pub const fn new(value: u16) -> Result<Self, BoundedValueError> {
        if value == 0 {
            Err(BoundedValueError::ZeroTag {
                field: "ResultFormatVersion",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the stable numeric version.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Exact bounded terminal result retained for mutation replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationResult {
    kind: MutationResultKind,
    format: ResultFormatVersion,
    bytes: Box<[u8]>,
}

impl MutationResult {
    /// Creates a versioned opaque result within the configured byte bound.
    pub fn new(
        kind: MutationResultKind,
        format: ResultFormatVersion,
        bytes: impl Into<Vec<u8>>,
        limits: StateLimits,
    ) -> Result<Self, BoundedValueError> {
        let bytes = bytes.into();
        limits
            .require_bytes(
                StateLimitKind::MutationResult,
                bytes.len(),
                limits.max_mutation_result_bytes(),
            )
            .map_err(BoundedValueError::Limit)?;
        Ok(Self {
            kind,
            format,
            bytes: bytes.into_boxed_slice(),
        })
    }

    /// Returns the semantic result kind.
    pub const fn kind(&self) -> MutationResultKind {
        self.kind
    }

    /// Returns the result codec version.
    pub const fn format(&self) -> ResultFormatVersion {
        self.format
    }

    /// Returns the exact retained bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes.len()
    }
}

/// Failure constructing a bounded state value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundedValueError {
    /// A value that must identify something was empty.
    Empty {
        /// Stable value/type name.
        field: &'static str,
    },
    /// A value contained a forbidden byte.
    ForbiddenByte {
        /// Stable value/type name.
        field: &'static str,
        /// Rejected byte.
        byte: u8,
    },
    /// A directory component was `.` or `..`.
    ReservedComponent,
    /// A stable kind or format tag was zero.
    ZeroTag {
        /// Stable tag/type name.
        field: &'static str,
    },
    /// Configured bound was exceeded.
    Limit(StateLimitError),
}

impl fmt::Display for BoundedValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => write!(formatter, "{field} is empty"),
            Self::ForbiddenByte { field, byte } => {
                write!(formatter, "{field} contains forbidden byte {byte:#04x}")
            }
            Self::ReservedComponent => formatter.write_str("directory component is reserved"),
            Self::ZeroTag { field } => write!(formatter, "{field} must be nonzero"),
            Self::Limit(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for BoundedValueError {}

fn validate_nonempty(value: &[u8], field: &'static str) -> Result<(), BoundedValueError> {
    if value.is_empty() {
        Err(BoundedValueError::Empty { field })
    } else {
        Ok(())
    }
}

fn validate_no_nul(value: &[u8], field: &'static str) -> Result<(), BoundedValueError> {
    if value.contains(&0) {
        Err(BoundedValueError::ForbiddenByte { field, byte: 0 })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_components_are_byte_exact_and_checked() {
        let limits = StateLimits::default();
        assert_eq!(
            EntryName::new(b"name".to_vec(), limits).unwrap().as_bytes(),
            b"name"
        );
        assert!(EntryName::new(b"".to_vec(), limits).is_err());
        assert!(EntryName::new(b".".to_vec(), limits).is_err());
        assert!(EntryName::new(b"a/b".to_vec(), limits).is_err());
    }

    #[test]
    fn mutation_results_preserve_exact_versioned_bytes() {
        let result = MutationResult::new(
            MutationResultKind::new(1).unwrap(),
            ResultFormatVersion::new(1).unwrap(),
            b"exact".to_vec(),
            StateLimits::default(),
        )
        .unwrap();
        assert_eq!(result.bytes(), b"exact");
        assert_eq!(result.kind().get(), 1);
        assert_eq!(result.format().get(), 1);
    }
}
