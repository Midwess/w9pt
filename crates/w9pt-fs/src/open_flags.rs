//! Checked conversion of Linux open flags into portable semantic state.

use core::fmt;

use w9pt::protocol::OpenFlags;
use w9pt_fs_state::OpenAccess;

const COMMON_FLAGS: u32 = OpenFlags::ACCESS_MASK.bits()
    | OpenFlags::TRUNC.bits()
    | OpenFlags::APPEND.bits()
    | OpenFlags::LARGEFILE.bits()
    | OpenFlags::DIRECTORY.bits()
    | OpenFlags::NOATIME.bits()
    | OpenFlags::CLOEXEC.bits();
const CREATE_FLAGS: u32 = OpenFlags::CREATE.bits() | OpenFlags::EXCL.bits();

/// Operation family in which open flags are interpreted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenPurpose {
    /// Open an already resolved object.
    Existing,
    /// Create and open a new regular file.
    Create,
}

/// Portable access and behavior retained by the semantic engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenOptions {
    access: OpenAccess,
    append: bool,
    truncate: bool,
    directory: bool,
}

impl OpenOptions {
    /// Returns read/write access for a regular file.
    pub const fn access(self) -> OpenAccess {
        self.access
    }

    /// Reports whether writes choose authoritative EOF.
    pub const fn append(self) -> bool {
        self.append
    }

    /// Reports whether opening must atomically truncate an existing regular file.
    pub const fn truncate(self) -> bool {
        self.truncate
    }

    /// Reports whether the caller requires a directory.
    pub const fn directory(self) -> bool {
        self.directory
    }

    /// Converts a checked read-only directory open into its retained state value.
    pub const fn directory_access(self) -> OpenAccess {
        OpenAccess::DirectoryRead
    }
}

/// Invalid or unsupported open flags rejected before semantic side effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenFlagError {
    /// Undefined access mode or contradictory flag combination.
    InvalidCombination,
    /// A known or unknown flag has no complete first-slice semantics.
    UnsupportedFlags {
        /// Exact unsupported bits.
        bits: u32,
    },
}

impl fmt::Display for OpenFlagError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCombination => formatter.write_str("invalid open flag combination"),
            Self::UnsupportedFlags { bits } => {
                write!(formatter, "unsupported open flag bits {bits:#x}")
            }
        }
    }
}

impl std::error::Error for OpenFlagError {}

/// Validates flags without discarding unknown bits or treating zero-valued RDONLY specially.
pub fn validate_open_flags(
    flags: OpenFlags,
    purpose: OpenPurpose,
) -> Result<OpenOptions, OpenFlagError> {
    let allowed = COMMON_FLAGS
        | if purpose == OpenPurpose::Create {
            CREATE_FLAGS
        } else {
            0
        };
    let unsupported = flags.bits() & !allowed;
    if unsupported != 0 {
        return Err(OpenFlagError::UnsupportedFlags { bits: unsupported });
    }
    let access = match flags.bits() & OpenFlags::ACCESS_MASK.bits() {
        0 => OpenAccess::ReadOnly,
        1 => OpenAccess::WriteOnly,
        2 => OpenAccess::ReadWrite,
        _ => return Err(OpenFlagError::InvalidCombination),
    };
    let writable = matches!(access, OpenAccess::WriteOnly | OpenAccess::ReadWrite);
    let append = flags.contains(OpenFlags::APPEND);
    let truncate = flags.contains(OpenFlags::TRUNC);
    let directory = flags.contains(OpenFlags::DIRECTORY);
    if (append || truncate) && !writable {
        return Err(OpenFlagError::InvalidCombination);
    }
    if directory && (purpose == OpenPurpose::Create || writable || append || truncate) {
        return Err(OpenFlagError::InvalidCombination);
    }
    Ok(OpenOptions {
        access,
        append,
        truncate,
        directory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_access_is_read_only_and_write_behaviors_require_write_access() {
        assert_eq!(
            validate_open_flags(OpenFlags::RDONLY, OpenPurpose::Existing)
                .unwrap()
                .access(),
            OpenAccess::ReadOnly
        );
        assert_eq!(
            validate_open_flags(OpenFlags::WRONLY | OpenFlags::APPEND, OpenPurpose::Existing)
                .unwrap()
                .access(),
            OpenAccess::WriteOnly
        );
        assert_eq!(
            validate_open_flags(OpenFlags::RDONLY | OpenFlags::TRUNC, OpenPurpose::Existing),
            Err(OpenFlagError::InvalidCombination)
        );
    }

    #[test]
    fn unknown_and_purpose_specific_flags_are_not_discarded() {
        assert!(matches!(
            validate_open_flags(OpenFlags::from_bits(1 << 31), OpenPurpose::Existing),
            Err(OpenFlagError::UnsupportedFlags { bits }) if bits == 1 << 31
        ));
        assert!(matches!(
            validate_open_flags(OpenFlags::CREATE, OpenPurpose::Existing),
            Err(OpenFlagError::UnsupportedFlags { .. })
        ));
        for flag in [OpenFlags::DSYNC, OpenFlags::SYNC] {
            assert!(matches!(
                validate_open_flags(OpenFlags::WRONLY | flag, OpenPurpose::Existing),
                Err(OpenFlagError::UnsupportedFlags { bits }) if bits == flag.bits()
            ));
        }
        assert!(
            validate_open_flags(
                OpenFlags::CREATE | OpenFlags::EXCL | OpenFlags::WRONLY,
                OpenPurpose::Create,
            )
            .is_ok()
        );
    }
}
