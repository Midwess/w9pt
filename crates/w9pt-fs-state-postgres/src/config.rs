//! Checked PostgreSQL adapter configuration.

use core::fmt;
use std::time::Duration;

use w9pt_fs_state::{StateLimitValues, StateLimits};

const MAX_TIMEOUT_MILLIS: u128 = i32::MAX as u128;
const MAX_DEFINITIVE_ABORT_RETRIES: u32 = 64;
const MAX_AMBIGUOUS_COMMIT_RECOVERY_ATTEMPTS: u32 = 64;

/// Immutable maxima enforced by the version-1 PostgreSQL schema.
pub const SCHEMA_LIMITS: StateLimitValues = StateLimitValues {
    max_entry_name_bytes: 1_024,
    max_xattr_name_bytes: 1_024,
    max_xattr_value_bytes: 1024 * 1024,
    max_principal_bytes: 64 * 1024,
    max_group_bytes: 64 * 1024,
    max_symlink_bytes: 1024 * 1024,
    max_mutation_result_bytes: 8 * 1024 * 1024,
    max_read_queries: 4_096,
    max_directory_ancestor_depth: 65_535,
    max_scan_items: 65_536,
    max_scan_bytes: 64 * 1024 * 1024,
    max_preconditions: 16_384,
    max_changes: 16_384,
    max_change_keys: 16_385,
    max_transaction_bytes: 64 * 1024 * 1024,
    max_locks_per_request: 16_384,
    max_open_pins_per_request: 16_384,
    max_xattrs_per_request: 16_384,
    max_lease_duration_ticks: u64::MAX,
    max_lease_operation_history: 1_000_000,
    max_change_history_commits: 1_000_000,
};

pub(crate) fn schema_limits() -> StateLimits {
    StateLimits::new(SCHEMA_LIMITS).expect("version-1 schema limits are internally valid")
}

/// Durability boundary implemented by the version-1 adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PrimaryWalDurability {
    /// Acknowledge after the writable primary durably flushes local WAL.
    #[default]
    Required,
}

/// Validated behavior and resource limits for one PostgreSQL adapter instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PostgresStateConfig {
    limits: StateLimits,
    statement_timeout: Duration,
    lock_timeout: Duration,
    definitive_abort_retries: u32,
    ambiguous_commit_recovery_attempts: u32,
    durability: PrimaryWalDurability,
}

impl PostgresStateConfig {
    /// Constructs configuration after checking adapter and schema bounds.
    pub fn new(
        limits: StateLimits,
        statement_timeout: Duration,
        lock_timeout: Duration,
        definitive_abort_retries: u32,
        ambiguous_commit_recovery_attempts: u32,
        durability: PrimaryWalDurability,
    ) -> Result<Self, PostgresConfigError> {
        validate_timeout("statement_timeout", statement_timeout)?;
        validate_timeout("lock_timeout", lock_timeout)?;
        if lock_timeout > statement_timeout {
            return Err(PostgresConfigError::LockTimeoutExceedsStatementTimeout);
        }
        if definitive_abort_retries > MAX_DEFINITIVE_ABORT_RETRIES {
            return Err(PostgresConfigError::RetryBoundTooLarge {
                field: "definitive_abort_retries",
                actual: definitive_abort_retries,
                maximum: MAX_DEFINITIVE_ABORT_RETRIES,
            });
        }
        if ambiguous_commit_recovery_attempts == 0 {
            return Err(PostgresConfigError::Zero {
                field: "ambiguous_commit_recovery_attempts",
            });
        }
        if ambiguous_commit_recovery_attempts > MAX_AMBIGUOUS_COMMIT_RECOVERY_ATTEMPTS {
            return Err(PostgresConfigError::RetryBoundTooLarge {
                field: "ambiguous_commit_recovery_attempts",
                actual: ambiguous_commit_recovery_attempts,
                maximum: MAX_AMBIGUOUS_COMMIT_RECOVERY_ATTEMPTS,
            });
        }
        validate_schema_limits(limits)?;
        Ok(Self {
            limits,
            statement_timeout,
            lock_timeout,
            definitive_abort_retries,
            ambiguous_commit_recovery_attempts,
            durability,
        })
    }

    /// Returns the finalized state-store limits enforced by this instance.
    pub const fn limits(self) -> StateLimits {
        self.limits
    }

    /// Returns the per-transaction PostgreSQL statement timeout.
    pub const fn statement_timeout(self) -> Duration {
        self.statement_timeout
    }

    /// Returns the per-transaction PostgreSQL row-lock timeout.
    pub const fn lock_timeout(self) -> Duration {
        self.lock_timeout
    }

    /// Returns the maximum retry count after definitive native aborts.
    pub const fn definitive_abort_retries(self) -> u32 {
        self.definitive_abort_retries
    }

    /// Returns the maximum number of fresh-primary ambiguous-commit recovery attempts.
    pub const fn ambiguous_commit_recovery_attempts(self) -> u32 {
        self.ambiguous_commit_recovery_attempts
    }

    /// Returns the required durability boundary.
    pub const fn durability(self) -> PrimaryWalDurability {
        self.durability
    }
}

impl Default for PostgresStateConfig {
    fn default() -> Self {
        Self::new(
            StateLimits::default(),
            Duration::from_secs(30),
            Duration::from_secs(5),
            8,
            8,
            PrimaryWalDurability::Required,
        )
        .expect("default PostgreSQL state configuration is valid")
    }
}

/// Stable name of a configured state limit checked against the schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaLimit {
    /// Directory-entry name bytes.
    EntryNameBytes,
    /// Extended-attribute name bytes.
    XattrNameBytes,
    /// Extended-attribute value bytes.
    XattrValueBytes,
    /// Principal identity bytes.
    PrincipalBytes,
    /// Group identity bytes.
    GroupBytes,
    /// Symbolic-link target bytes.
    SymlinkBytes,
    /// Mutation result bytes.
    MutationResultBytes,
    /// Queries per read batch.
    ReadQueries,
    /// Directory ancestors traversed during validation.
    DirectoryAncestorDepth,
    /// Records per scan page.
    ScanItems,
    /// Bytes per scan page.
    ScanBytes,
    /// Preconditions per commit.
    Preconditions,
    /// State changes per commit.
    Changes,
    /// Changed keys per event.
    ChangeKeys,
    /// Aggregate transaction bytes.
    TransactionBytes,
    /// Locks per request.
    Locks,
    /// Open pins per request.
    OpenPins,
    /// Extended attributes per request.
    Xattrs,
    /// Lease duration ticks.
    LeaseDurationTicks,
    /// Retained lease-operation rows.
    LeaseOperationHistory,
    /// Retained change commits.
    ChangeHistoryCommits,
}

/// Invalid PostgreSQL adapter configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostgresConfigError {
    /// A required positive adapter value was zero.
    Zero {
        /// Stable configuration field name.
        field: &'static str,
    },
    /// A duration cannot be represented by transaction-local PostgreSQL settings.
    TimeoutTooLarge {
        /// Stable configuration field name.
        field: &'static str,
        /// Rejected duration in milliseconds, saturated to `u64`.
        milliseconds: u64,
    },
    /// A positive duration would truncate to PostgreSQL's timeout-disabling zero milliseconds.
    TimeoutBelowOneMillisecond {
        /// Stable configuration field name.
        field: &'static str,
    },
    /// Waiting for a lock could outlive the enclosing statement.
    LockTimeoutExceedsStatementTimeout,
    /// A bounded retry setting exceeds the adapter hard maximum.
    RetryBoundTooLarge {
        /// Stable configuration field name.
        field: &'static str,
        /// Rejected retry count.
        actual: u32,
        /// Adapter hard maximum.
        maximum: u32,
    },
    /// A finalized state limit exceeds the immutable schema maximum.
    StateLimitExceedsSchema {
        /// Limit category.
        limit: SchemaLimit,
        /// Configured value.
        actual: u64,
        /// Immutable schema maximum.
        maximum: u64,
    },
}

impl fmt::Display for PostgresConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero { field } => write!(formatter, "{field} must be nonzero"),
            Self::TimeoutTooLarge {
                field,
                milliseconds,
            } => write!(
                formatter,
                "{field} value {milliseconds}ms exceeds PostgreSQL adapter maximum"
            ),
            Self::TimeoutBelowOneMillisecond { field } => {
                write!(formatter, "{field} must be at least one millisecond")
            }
            Self::LockTimeoutExceedsStatementTimeout => {
                formatter.write_str("lock_timeout must not exceed statement_timeout")
            }
            Self::RetryBoundTooLarge {
                field,
                actual,
                maximum,
            } => write!(
                formatter,
                "{field} value {actual} exceeds maximum {maximum}"
            ),
            Self::StateLimitExceedsSchema {
                limit,
                actual,
                maximum,
            } => write!(
                formatter,
                "state limit {limit:?} value {actual} exceeds schema maximum {maximum}"
            ),
        }
    }
}

impl std::error::Error for PostgresConfigError {}

fn validate_timeout(field: &'static str, value: Duration) -> Result<(), PostgresConfigError> {
    if value.is_zero() {
        return Err(PostgresConfigError::Zero { field });
    }
    if value.as_millis() == 0 {
        return Err(PostgresConfigError::TimeoutBelowOneMillisecond { field });
    }
    if value.as_millis() > MAX_TIMEOUT_MILLIS {
        return Err(PostgresConfigError::TimeoutTooLarge {
            field,
            milliseconds: u64::try_from(value.as_millis()).unwrap_or(u64::MAX),
        });
    }
    Ok(())
}

fn validate_schema_limits(limits: StateLimits) -> Result<(), PostgresConfigError> {
    let actual = limits.values();
    let pairs = [
        (
            SchemaLimit::EntryNameBytes,
            usize_u64(actual.max_entry_name_bytes),
            usize_u64(SCHEMA_LIMITS.max_entry_name_bytes),
        ),
        (
            SchemaLimit::XattrNameBytes,
            usize_u64(actual.max_xattr_name_bytes),
            usize_u64(SCHEMA_LIMITS.max_xattr_name_bytes),
        ),
        (
            SchemaLimit::XattrValueBytes,
            usize_u64(actual.max_xattr_value_bytes),
            usize_u64(SCHEMA_LIMITS.max_xattr_value_bytes),
        ),
        (
            SchemaLimit::PrincipalBytes,
            usize_u64(actual.max_principal_bytes),
            usize_u64(SCHEMA_LIMITS.max_principal_bytes),
        ),
        (
            SchemaLimit::GroupBytes,
            usize_u64(actual.max_group_bytes),
            usize_u64(SCHEMA_LIMITS.max_group_bytes),
        ),
        (
            SchemaLimit::SymlinkBytes,
            usize_u64(actual.max_symlink_bytes),
            usize_u64(SCHEMA_LIMITS.max_symlink_bytes),
        ),
        (
            SchemaLimit::MutationResultBytes,
            usize_u64(actual.max_mutation_result_bytes),
            usize_u64(SCHEMA_LIMITS.max_mutation_result_bytes),
        ),
        (
            SchemaLimit::ReadQueries,
            u64::from(actual.max_read_queries),
            u64::from(SCHEMA_LIMITS.max_read_queries),
        ),
        (
            SchemaLimit::DirectoryAncestorDepth,
            u64::from(actual.max_directory_ancestor_depth),
            u64::from(SCHEMA_LIMITS.max_directory_ancestor_depth),
        ),
        (
            SchemaLimit::ScanItems,
            u64::from(actual.max_scan_items),
            u64::from(SCHEMA_LIMITS.max_scan_items),
        ),
        (
            SchemaLimit::ScanBytes,
            usize_u64(actual.max_scan_bytes),
            usize_u64(SCHEMA_LIMITS.max_scan_bytes),
        ),
        (
            SchemaLimit::Preconditions,
            u64::from(actual.max_preconditions),
            u64::from(SCHEMA_LIMITS.max_preconditions),
        ),
        (
            SchemaLimit::Changes,
            u64::from(actual.max_changes),
            u64::from(SCHEMA_LIMITS.max_changes),
        ),
        (
            SchemaLimit::ChangeKeys,
            u64::from(actual.max_change_keys),
            u64::from(SCHEMA_LIMITS.max_change_keys),
        ),
        (
            SchemaLimit::TransactionBytes,
            usize_u64(actual.max_transaction_bytes),
            usize_u64(SCHEMA_LIMITS.max_transaction_bytes),
        ),
        (
            SchemaLimit::Locks,
            u64::from(actual.max_locks_per_request),
            u64::from(SCHEMA_LIMITS.max_locks_per_request),
        ),
        (
            SchemaLimit::OpenPins,
            u64::from(actual.max_open_pins_per_request),
            u64::from(SCHEMA_LIMITS.max_open_pins_per_request),
        ),
        (
            SchemaLimit::Xattrs,
            u64::from(actual.max_xattrs_per_request),
            u64::from(SCHEMA_LIMITS.max_xattrs_per_request),
        ),
        (
            SchemaLimit::LeaseDurationTicks,
            actual.max_lease_duration_ticks,
            SCHEMA_LIMITS.max_lease_duration_ticks,
        ),
        (
            SchemaLimit::LeaseOperationHistory,
            u64::from(actual.max_lease_operation_history),
            u64::from(SCHEMA_LIMITS.max_lease_operation_history),
        ),
        (
            SchemaLimit::ChangeHistoryCommits,
            u64::from(actual.max_change_history_commits),
            u64::from(SCHEMA_LIMITS.max_change_history_commits),
        ),
    ];
    for (limit, actual, maximum) in pairs {
        if actual > maximum {
            return Err(PostgresConfigError::StateLimitExceedsSchema {
                limit,
                actual,
                maximum,
            });
        }
    }
    Ok(())
}

fn usize_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_checked_and_expose_primary_wal_durability() {
        let config = PostgresStateConfig::default();
        assert_eq!(config.limits(), StateLimits::default());
        assert_eq!(config.durability(), PrimaryWalDurability::Required);
        assert_eq!(config.statement_timeout().as_millis(), 30_000);
        assert_eq!(config.lock_timeout().as_millis(), 5_000);
    }

    #[test]
    fn invalid_timeouts_and_retry_bounds_are_rejected() {
        let default = PostgresStateConfig::default();
        assert!(matches!(
            PostgresStateConfig::new(
                default.limits(),
                Duration::ZERO,
                default.lock_timeout(),
                0,
                1,
                default.durability(),
            ),
            Err(PostgresConfigError::Zero {
                field: "statement_timeout"
            })
        ));
        assert!(matches!(
            PostgresStateConfig::new(
                default.limits(),
                Duration::from_nanos(1),
                Duration::from_nanos(1),
                0,
                1,
                default.durability(),
            ),
            Err(PostgresConfigError::TimeoutBelowOneMillisecond {
                field: "statement_timeout"
            })
        ));
        assert!(matches!(
            PostgresStateConfig::new(
                default.limits(),
                default.statement_timeout(),
                default.lock_timeout(),
                0,
                0,
                default.durability(),
            ),
            Err(PostgresConfigError::Zero {
                field: "ambiguous_commit_recovery_attempts"
            })
        ));
    }

    #[test]
    fn schema_maxima_are_enforced() {
        let values = StateLimitValues {
            max_entry_name_bytes: SCHEMA_LIMITS.max_entry_name_bytes + 1,
            ..StateLimitValues::default()
        };
        let limits = StateLimits::new(values).expect("state limits are internally valid");
        let default = PostgresStateConfig::default();
        assert!(matches!(
            PostgresStateConfig::new(
                limits,
                default.statement_timeout(),
                default.lock_timeout(),
                0,
                1,
                default.durability(),
            ),
            Err(PostgresConfigError::StateLimitExceedsSchema {
                limit: SchemaLimit::EntryNameBytes,
                ..
            })
        ));
    }
}
