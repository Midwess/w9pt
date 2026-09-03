//! Validated resource limits for storage amplification.

use core::fmt;

/// Configurable values used to construct [`StorageLimits`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageLimitValues {
    /// Maximum logical bytes materialized by the raw layout.
    pub max_raw_file_bytes: u64,
    /// Maximum encoded manifest object bytes.
    pub max_manifest_bytes: usize,
    /// Maximum bytes accepted for any one stored object.
    pub max_object_bytes: usize,
    /// Maximum entries in one flat block-split manifest.
    pub max_blocks: u32,
    /// Maximum bytes returned by one repository read.
    pub max_read_bytes: usize,
    /// Maximum bytes accepted by one repository write.
    pub max_write_bytes: usize,
    /// Maximum CAS conflicts that may be rebased after the initial attempt.
    pub max_publish_retries: u32,
    /// Maximum UTF-8 bytes in a constructed target key.
    pub max_key_bytes: usize,
}

impl Default for StorageLimitValues {
    fn default() -> Self {
        Self {
            max_raw_file_bytes: 16 * 1024 * 1024,
            max_manifest_bytes: 8 * 1024 * 1024,
            max_object_bytes: 64 * 1024 * 1024,
            max_blocks: 131_072,
            max_read_bytes: 8 * 1024 * 1024,
            max_write_bytes: 8 * 1024 * 1024,
            max_publish_retries: 8,
            max_key_bytes: 1_024,
        }
    }
}

/// Validated, bounded repository configuration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StorageLimits(StorageLimitValues);

impl StorageLimits {
    /// Validates caller-provided limit values.
    pub fn new(values: StorageLimitValues) -> Result<Self, ConfigurationError> {
        require_nonzero("max_raw_file_bytes", values.max_raw_file_bytes)?;
        require_nonzero("max_manifest_bytes", values.max_manifest_bytes)?;
        require_nonzero("max_object_bytes", values.max_object_bytes)?;
        require_nonzero("max_blocks", values.max_blocks)?;
        require_nonzero("max_read_bytes", values.max_read_bytes)?;
        require_nonzero("max_write_bytes", values.max_write_bytes)?;
        require_nonzero("max_key_bytes", values.max_key_bytes)?;

        if values.max_manifest_bytes > values.max_object_bytes {
            return Err(ConfigurationError::Inconsistent {
                field: "max_manifest_bytes",
                other: "max_object_bytes",
            });
        }
        const ENVELOPE_BYTES: u64 = 53;
        let max_object_bytes = u64::try_from(values.max_object_bytes).unwrap_or(u64::MAX);
        if values
            .max_raw_file_bytes
            .checked_add(ENVELOPE_BYTES)
            .is_none_or(|required| required > max_object_bytes)
        {
            return Err(ConfigurationError::Inconsistent {
                field: "max_raw_file_bytes",
                other: "max_object_bytes minus envelope",
            });
        }
        if u64::from(crate::BLOCK_SIZE_V1) + ENVELOPE_BYTES > max_object_bytes {
            return Err(ConfigurationError::Inconsistent {
                field: "version-1 block object",
                other: "max_object_bytes",
            });
        }
        if values.max_publish_retries > 1_024 {
            return Err(ConfigurationError::TooLarge {
                field: "max_publish_retries",
                maximum: 1_024,
            });
        }

        Ok(Self(values))
    }

    /// Returns the maximum raw logical size.
    pub const fn max_raw_file_bytes(self) -> u64 {
        self.0.max_raw_file_bytes
    }

    /// Returns the maximum encoded manifest size.
    pub const fn max_manifest_bytes(self) -> usize {
        self.0.max_manifest_bytes
    }

    /// Returns the maximum encoded object size.
    pub const fn max_object_bytes(self) -> usize {
        self.0.max_object_bytes
    }

    /// Returns the maximum flat block-entry count.
    pub const fn max_blocks(self) -> u32 {
        self.0.max_blocks
    }

    /// Returns the maximum read result size.
    pub const fn max_read_bytes(self) -> usize {
        self.0.max_read_bytes
    }

    /// Returns the maximum write input size.
    pub const fn max_write_bytes(self) -> usize {
        self.0.max_write_bytes
    }

    /// Returns the number of CAS conflicts that may be rebased.
    pub const fn max_publish_retries(self) -> u32 {
        self.0.max_publish_retries
    }

    /// Returns the maximum repository key length.
    pub const fn max_key_bytes(self) -> usize {
        self.0.max_key_bytes
    }
}

trait NonZeroValue {
    fn equals_zero(self) -> bool;
}

impl NonZeroValue for u64 {
    fn equals_zero(self) -> bool {
        self == 0
    }
}

impl NonZeroValue for u32 {
    fn equals_zero(self) -> bool {
        self == 0
    }
}

impl NonZeroValue for usize {
    fn equals_zero(self) -> bool {
        self == 0
    }
}

fn require_nonzero<T: NonZeroValue>(
    field: &'static str,
    value: T,
) -> Result<(), ConfigurationError> {
    if value.equals_zero() {
        Err(ConfigurationError::Zero { field })
    } else {
        Ok(())
    }
}

/// Invalid resource-limit configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationError {
    /// A bound that must be positive was zero.
    Zero {
        /// Name of the rejected field.
        field: &'static str,
    },
    /// One configured bound exceeds another bound that contains it.
    Inconsistent {
        /// Name of the larger child bound.
        field: &'static str,
        /// Name of the containing bound.
        other: &'static str,
    },
    /// The configured private prefix is not canonical.
    InvalidPrefix {
        /// Human-readable validation reason.
        reason: &'static str,
    },
    /// The target does not provide a required semantic guarantee.
    MissingTargetGuarantee {
        /// Stable name of the required guarantee.
        guarantee: &'static str,
    },
    /// A configuration value exceeds a hard safety ceiling.
    TooLarge {
        /// Name of the rejected field.
        field: &'static str,
        /// Largest accepted value.
        maximum: u64,
    },
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero { field } => {
                write!(formatter, "configuration field {field} must be nonzero")
            }
            Self::Inconsistent { field, other } => {
                write!(formatter, "configuration field {field} exceeds {other}")
            }
            Self::InvalidPrefix { reason } => write!(formatter, "invalid private prefix: {reason}"),
            Self::MissingTargetGuarantee { guarantee } => {
                write!(formatter, "target lacks required guarantee: {guarantee}")
            }
            Self::TooLarge { field, maximum } => {
                write!(formatter, "configuration field {field} exceeds {maximum}")
            }
        }
    }
}

impl std::error::Error for ConfigurationError {}

/// Resource category whose configured bound was exceeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    /// Raw whole-file materialization.
    RawFile,
    /// Encoded immutable manifest.
    Manifest,
    /// One encoded target object.
    Object,
    /// Flat block manifest entry count.
    BlockCount,
    /// One read result.
    Read,
    /// One write input.
    Write,
    /// Constructed target key bytes.
    Key,
}

/// A checked operation exceeds a configured bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LimitError {
    /// Resource category that was bounded.
    pub kind: LimitKind,
    /// Requested or decoded amount.
    pub actual: u64,
    /// Configured maximum.
    pub limit: u64,
}

impl LimitError {
    /// Constructs a limit failure.
    pub const fn new(kind: LimitKind, actual: u64, limit: u64) -> Self {
        Self {
            kind,
            actual,
            limit,
        }
    }
}

impl fmt::Display for LimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} amount {} exceeds configured limit {}",
            self.kind, self.actual, self.limit
        )
    }
}

impl std::error::Error for LimitError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        assert_eq!(
            StorageLimits::new(StorageLimitValues::default()),
            Ok(StorageLimits::default())
        );
    }

    #[test]
    fn zero_and_inconsistent_bounds_are_rejected() {
        let values = StorageLimitValues {
            max_read_bytes: 0,
            ..StorageLimitValues::default()
        };
        assert_eq!(
            StorageLimits::new(values),
            Err(ConfigurationError::Zero {
                field: "max_read_bytes"
            })
        );

        let defaults = StorageLimitValues::default();
        let values = StorageLimitValues {
            max_manifest_bytes: defaults.max_object_bytes + 1,
            ..defaults
        };
        assert!(matches!(
            StorageLimits::new(values),
            Err(ConfigurationError::Inconsistent { .. })
        ));
    }
}
