//! Validated bounds for state requests, records, and retained history.

use core::fmt;

/// Caller-configurable values used to construct [`StateLimits`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateLimitValues {
    /// Maximum bytes in one directory entry name.
    pub max_entry_name_bytes: usize,
    /// Maximum bytes in one xattr name.
    pub max_xattr_name_bytes: usize,
    /// Maximum bytes in an inline xattr value or staging descriptor.
    pub max_xattr_value_bytes: usize,
    /// Maximum bytes in one principal identity.
    pub max_principal_bytes: usize,
    /// Maximum bytes in one group identity.
    pub max_group_bytes: usize,
    /// Maximum bytes in one symbolic-link target.
    pub max_symlink_bytes: usize,
    /// Maximum opaque storage policy bytes per content metadata record.
    pub max_content_policy_bytes: usize,
    /// Maximum opaque wrapped-key bytes per content metadata record.
    pub max_wrapped_content_key_bytes: usize,
    /// Maximum total retained bytes per content metadata record.
    pub max_content_metadata_bytes: usize,
    /// Maximum bytes in one retained terminal mutation result.
    pub max_mutation_result_bytes: usize,
    /// Maximum queries in one consistent read batch.
    pub max_read_queries: u32,
    /// Maximum authoritative directory-parent edges traversed during validation.
    pub max_directory_ancestor_depth: u32,
    /// Maximum records returned by one ordered scan.
    pub max_scan_items: u32,
    /// Maximum aggregate bytes returned by one scan.
    pub max_scan_bytes: usize,
    /// Maximum typed preconditions in one commit request.
    pub max_preconditions: u32,
    /// Maximum typed state changes in one commit request.
    pub max_changes: u32,
    /// Maximum changed-record keys in one whole-commit event.
    pub max_change_keys: u32,
    /// Maximum aggregate retained bytes in one transaction request.
    pub max_transaction_bytes: usize,
    /// Maximum locks affected by one request.
    pub max_locks_per_request: u32,
    /// Maximum open pins affected by one request.
    pub max_open_pins_per_request: u32,
    /// Maximum xattrs affected by one request.
    pub max_xattrs_per_request: u32,
    /// Maximum requested lease duration in adapter clock ticks.
    pub max_lease_duration_ticks: u64,
    /// Maximum retained idempotent lease-operation results.
    pub max_lease_operation_history: u32,
    /// Maximum commit events retained by the deterministic reference store.
    pub max_change_history_commits: u32,
}

impl Default for StateLimitValues {
    fn default() -> Self {
        Self {
            max_entry_name_bytes: 255,
            max_xattr_name_bytes: 255,
            max_xattr_value_bytes: 64 * 1024,
            max_principal_bytes: 1_024,
            max_group_bytes: 1_024,
            max_symlink_bytes: 16 * 1024,
            max_content_policy_bytes: 512,
            max_wrapped_content_key_bytes: 512,
            max_content_metadata_bytes: 2 * 1024,
            max_mutation_result_bytes: 1024 * 1024,
            max_read_queries: 256,
            max_directory_ancestor_depth: 1_024,
            max_scan_items: 4_096,
            max_scan_bytes: 8 * 1024 * 1024,
            max_preconditions: 1_024,
            max_changes: 1_024,
            max_change_keys: 1_025,
            max_transaction_bytes: 8 * 1024 * 1024,
            max_locks_per_request: 1_024,
            max_open_pins_per_request: 1_024,
            max_xattrs_per_request: 1_024,
            max_lease_duration_ticks: 86_400_000_000_000,
            max_lease_operation_history: 100_000,
            max_change_history_commits: 100_000,
        }
    }
}

/// Validated immutable state-store limits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StateLimits(StateLimitValues);

impl StateLimits {
    /// Validates caller-provided state bounds.
    pub fn new(values: StateLimitValues) -> Result<Self, InvalidStateLimits> {
        for (field, value) in [
            ("max_entry_name_bytes", to_u64(values.max_entry_name_bytes)),
            ("max_xattr_name_bytes", to_u64(values.max_xattr_name_bytes)),
            (
                "max_xattr_value_bytes",
                to_u64(values.max_xattr_value_bytes),
            ),
            ("max_principal_bytes", to_u64(values.max_principal_bytes)),
            ("max_group_bytes", to_u64(values.max_group_bytes)),
            ("max_symlink_bytes", to_u64(values.max_symlink_bytes)),
            (
                "max_content_policy_bytes",
                to_u64(values.max_content_policy_bytes),
            ),
            (
                "max_wrapped_content_key_bytes",
                to_u64(values.max_wrapped_content_key_bytes),
            ),
            (
                "max_content_metadata_bytes",
                to_u64(values.max_content_metadata_bytes),
            ),
            (
                "max_mutation_result_bytes",
                to_u64(values.max_mutation_result_bytes),
            ),
            ("max_read_queries", u64::from(values.max_read_queries)),
            (
                "max_directory_ancestor_depth",
                u64::from(values.max_directory_ancestor_depth),
            ),
            ("max_scan_items", u64::from(values.max_scan_items)),
            ("max_scan_bytes", to_u64(values.max_scan_bytes)),
            ("max_preconditions", u64::from(values.max_preconditions)),
            ("max_changes", u64::from(values.max_changes)),
            ("max_change_keys", u64::from(values.max_change_keys)),
            (
                "max_transaction_bytes",
                to_u64(values.max_transaction_bytes),
            ),
            (
                "max_locks_per_request",
                u64::from(values.max_locks_per_request),
            ),
            (
                "max_open_pins_per_request",
                u64::from(values.max_open_pins_per_request),
            ),
            (
                "max_xattrs_per_request",
                u64::from(values.max_xattrs_per_request),
            ),
            ("max_lease_duration_ticks", values.max_lease_duration_ticks),
            (
                "max_lease_operation_history",
                u64::from(values.max_lease_operation_history),
            ),
            (
                "max_change_history_commits",
                u64::from(values.max_change_history_commits),
            ),
        ] {
            if value == 0 {
                return Err(InvalidStateLimits::Zero { field });
            }
        }

        if values.max_mutation_result_bytes > values.max_transaction_bytes {
            return Err(InvalidStateLimits::Inconsistent {
                field: "max_mutation_result_bytes",
                container: "max_transaction_bytes",
            });
        }
        if values.max_content_policy_bytes > values.max_content_metadata_bytes
            || values.max_wrapped_content_key_bytes > values.max_content_metadata_bytes
        {
            return Err(InvalidStateLimits::Inconsistent {
                field: "content metadata component",
                container: "max_content_metadata_bytes",
            });
        }
        Ok(Self(values))
    }

    /// Returns the validated raw values for adapter conformance derivation.
    pub const fn values(self) -> StateLimitValues {
        self.0
    }

    /// Returns the maximum directory-entry name length.
    pub const fn max_entry_name_bytes(self) -> usize {
        self.0.max_entry_name_bytes
    }

    /// Returns the maximum xattr-name length.
    pub const fn max_xattr_name_bytes(self) -> usize {
        self.0.max_xattr_name_bytes
    }

    /// Returns the maximum inline xattr/staging value length.
    pub const fn max_xattr_value_bytes(self) -> usize {
        self.0.max_xattr_value_bytes
    }

    /// Returns the maximum principal identity length.
    pub const fn max_principal_bytes(self) -> usize {
        self.0.max_principal_bytes
    }

    /// Returns the maximum group identity length.
    pub const fn max_group_bytes(self) -> usize {
        self.0.max_group_bytes
    }

    /// Returns the maximum symlink target length.
    pub const fn max_symlink_bytes(self) -> usize {
        self.0.max_symlink_bytes
    }

    /// Returns the maximum opaque file policy size.
    pub const fn max_content_policy_bytes(self) -> usize {
        self.0.max_content_policy_bytes
    }

    /// Returns the maximum opaque wrapped file-key size.
    pub const fn max_wrapped_content_key_bytes(self) -> usize {
        self.0.max_wrapped_content_key_bytes
    }

    /// Returns the maximum retained content metadata size.
    pub const fn max_content_metadata_bytes(self) -> usize {
        self.0.max_content_metadata_bytes
    }

    /// Returns the maximum retained result length.
    pub const fn max_mutation_result_bytes(self) -> usize {
        self.0.max_mutation_result_bytes
    }

    /// Returns the maximum read-query count.
    pub const fn max_read_queries(self) -> u32 {
        self.0.max_read_queries
    }

    /// Returns the maximum authoritative directory-parent edge depth.
    pub const fn max_directory_ancestor_depth(self) -> u32 {
        self.0.max_directory_ancestor_depth
    }

    /// Returns the maximum scan item count.
    pub const fn max_scan_items(self) -> u32 {
        self.0.max_scan_items
    }

    /// Returns the maximum scan byte count.
    pub const fn max_scan_bytes(self) -> usize {
        self.0.max_scan_bytes
    }

    /// Returns the maximum commit precondition count.
    pub const fn max_preconditions(self) -> u32 {
        self.0.max_preconditions
    }

    /// Returns the maximum state-change count.
    pub const fn max_changes(self) -> u32 {
        self.0.max_changes
    }

    /// Returns the maximum changed-key count.
    pub const fn max_change_keys(self) -> u32 {
        self.0.max_change_keys
    }

    /// Returns the maximum aggregate transaction byte count.
    pub const fn max_transaction_bytes(self) -> usize {
        self.0.max_transaction_bytes
    }

    /// Returns the maximum lock count affected by one request.
    pub const fn max_locks_per_request(self) -> u32 {
        self.0.max_locks_per_request
    }

    /// Returns the maximum open-pin count affected by one request.
    pub const fn max_open_pins_per_request(self) -> u32 {
        self.0.max_open_pins_per_request
    }

    /// Returns the maximum xattr count affected by one request.
    pub const fn max_xattrs_per_request(self) -> u32 {
        self.0.max_xattrs_per_request
    }

    /// Returns the maximum lease duration.
    pub const fn max_lease_duration_ticks(self) -> u64 {
        self.0.max_lease_duration_ticks
    }

    /// Returns the maximum retained idempotent lease-operation results.
    pub const fn max_lease_operation_history(self) -> u32 {
        self.0.max_lease_operation_history
    }

    /// Returns the maximum retained change-event count.
    pub const fn max_change_history_commits(self) -> u32 {
        self.0.max_change_history_commits
    }

    pub(crate) fn require_count(
        self,
        kind: StateLimitKind,
        actual: usize,
        maximum: u32,
    ) -> Result<(), StateLimitError> {
        if to_u64(actual) > u64::from(maximum) {
            Err(StateLimitError::new(
                kind,
                to_u64(actual),
                u64::from(maximum),
            ))
        } else {
            Ok(())
        }
    }

    pub(crate) fn require_bytes(
        self,
        kind: StateLimitKind,
        actual: usize,
        maximum: usize,
    ) -> Result<(), StateLimitError> {
        if actual > maximum {
            Err(StateLimitError::new(kind, to_u64(actual), to_u64(maximum)))
        } else {
            Ok(())
        }
    }
}

/// Invalid relationship between configured state bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidStateLimits {
    /// A required positive limit is zero.
    Zero {
        /// Stable configuration field name.
        field: &'static str,
    },
    /// One value exceeds a bound that must contain it.
    Inconsistent {
        /// Stable child field name.
        field: &'static str,
        /// Stable containing field name.
        container: &'static str,
    },
}

impl fmt::Display for InvalidStateLimits {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero { field } => write!(formatter, "state limit {field} must be nonzero"),
            Self::Inconsistent { field, container } => {
                write!(formatter, "state limit {field} exceeds {container}")
            }
        }
    }
}

impl std::error::Error for InvalidStateLimits {}

/// Bounded state resource category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateLimitKind {
    /// Directory entry name bytes.
    EntryName,
    /// Extended-attribute name bytes.
    XattrName,
    /// Extended-attribute value or staging bytes.
    XattrValue,
    /// Principal identity bytes.
    Principal,
    /// Group identity bytes.
    Group,
    /// Symbolic-link target bytes.
    Symlink,
    /// Opaque file storage policy bytes.
    ContentPolicy,
    /// Opaque wrapped per-file key bytes.
    WrappedContentKey,
    /// Total retained content metadata bytes.
    ContentMetadata,
    /// Retained mutation result bytes.
    MutationResult,
    /// Queries in one read batch.
    ReadQueries,
    /// Directory ancestors traversed during validation.
    DirectoryAncestors,
    /// Items in one scan page.
    ScanItems,
    /// Bytes in one scan page.
    ScanBytes,
    /// Preconditions in one commit.
    Preconditions,
    /// Changes in one commit.
    Changes,
    /// Keys in one change event.
    ChangeKeys,
    /// Aggregate transaction request bytes.
    TransactionBytes,
    /// Locks affected by one request.
    Locks,
    /// Open pins affected by one request.
    OpenPins,
    /// Xattrs affected by one request.
    Xattrs,
    /// Lease duration ticks.
    LeaseDuration,
    /// Retained idempotent lease-operation results.
    LeaseOperations,
    /// Retained change commits.
    ChangeHistory,
}

/// A state request or record exceeds its configured bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateLimitError {
    /// Resource category.
    pub kind: StateLimitKind,
    /// Requested or decoded amount.
    pub actual: u64,
    /// Configured maximum.
    pub maximum: u64,
}

impl StateLimitError {
    /// Constructs a state limit failure.
    pub const fn new(kind: StateLimitKind, actual: u64, maximum: u64) -> Self {
        Self {
            kind,
            actual,
            maximum,
        }
    }
}

impl fmt::Display for StateLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} amount {} exceeds configured maximum {}",
            self.kind, self.actual, self.maximum
        )
    }
}

impl std::error::Error for StateLimitError {}

fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_inconsistent_values_fail() {
        assert_eq!(
            StateLimits::new(StateLimitValues::default()),
            Ok(StateLimits::default())
        );
        let zero = StateLimitValues {
            max_changes: 0,
            ..StateLimitValues::default()
        };
        assert!(matches!(
            StateLimits::new(zero),
            Err(InvalidStateLimits::Zero {
                field: "max_changes"
            })
        ));
        let inconsistent = StateLimitValues {
            max_mutation_result_bytes: 2,
            max_transaction_bytes: 1,
            ..StateLimitValues::default()
        };
        assert!(matches!(
            StateLimits::new(inconsistent),
            Err(InvalidStateLimits::Inconsistent { .. })
        ));
    }

    #[test]
    fn request_checks_fail_before_amplification() {
        let limits = StateLimits::default();
        assert!(
            limits
                .require_count(StateLimitKind::Changes, 1_025, limits.max_changes())
                .is_err()
        );
        assert!(
            limits
                .require_bytes(
                    StateLimitKind::MutationResult,
                    limits.max_mutation_result_bytes() + 1,
                    limits.max_mutation_result_bytes(),
                )
                .is_err()
        );
    }
}
