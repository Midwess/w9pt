//! Checked bounds for semantic evaluation and retained execution data.

use core::fmt;

const MAX_RETRY_BOUND: u32 = 1_024;
const MAX_DEPTH_BOUND: u32 = u16::MAX as u32;

/// Caller-configurable values used to construct [`EngineLimits`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineLimitValues {
    /// Maximum definitive conflicts retried after the initial semantic plan.
    pub max_conflict_retries: u32,
    /// Maximum identical attempts used to resolve an ambiguous commit.
    pub max_ambiguity_attempts: u32,
    /// Maximum number of components resolved by one walk.
    pub max_walk_depth: u32,
    /// Maximum directory ancestors inspected for confinement or cycle checks.
    pub max_ancestor_depth: u32,
    /// Maximum supplementary groups accepted from an export grant.
    pub max_supplementary_groups: u32,
    /// Maximum entries requested from one authoritative directory page.
    pub max_directory_page_entries: u32,
    /// Maximum aggregate bytes requested for one authoritative directory page.
    pub max_directory_page_bytes: usize,
    /// Maximum encoded bytes in one terminal-result codec value.
    pub max_result_bytes: usize,
    /// Maximum encoded bytes in one canonical operation fingerprint input.
    pub max_fingerprint_bytes: usize,
    /// Maximum authoritative queries issued in one consistent read.
    pub max_read_queries: u32,
    /// Maximum preconditions and changes retained in one semantic commit plan.
    pub max_commit_items: u32,
    /// Maximum aggregate bytes retained by one in-flight engine execution.
    pub max_retained_bytes: usize,
}

impl Default for EngineLimitValues {
    fn default() -> Self {
        Self {
            max_conflict_retries: 8,
            max_ambiguity_attempts: 8,
            max_walk_depth: 256,
            max_ancestor_depth: 256,
            max_supplementary_groups: 1_024,
            max_directory_page_entries: 4_096,
            max_directory_page_bytes: 8 * 1024 * 1024,
            max_result_bytes: 1024 * 1024,
            max_fingerprint_bytes: 1024 * 1024,
            max_read_queries: 256,
            max_commit_items: 1_024,
            max_retained_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Validated immutable semantic-engine limits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EngineLimits(EngineLimitValues);

impl EngineLimits {
    /// Validates caller-provided semantic bounds.
    ///
    /// Zero conflict retries are valid and request a single semantic attempt.
    /// Every capacity and ambiguity-resolution bound must otherwise be nonzero.
    pub fn new(values: EngineLimitValues) -> Result<Self, InvalidEngineLimits> {
        for (field, value) in [
            (
                "max_ambiguity_attempts",
                u64::from(values.max_ambiguity_attempts),
            ),
            ("max_walk_depth", u64::from(values.max_walk_depth)),
            ("max_ancestor_depth", u64::from(values.max_ancestor_depth)),
            (
                "max_supplementary_groups",
                u64::from(values.max_supplementary_groups),
            ),
            (
                "max_directory_page_entries",
                u64::from(values.max_directory_page_entries),
            ),
            (
                "max_directory_page_bytes",
                usize_to_u64(values.max_directory_page_bytes),
            ),
            ("max_result_bytes", usize_to_u64(values.max_result_bytes)),
            (
                "max_fingerprint_bytes",
                usize_to_u64(values.max_fingerprint_bytes),
            ),
            ("max_read_queries", u64::from(values.max_read_queries)),
            ("max_commit_items", u64::from(values.max_commit_items)),
            (
                "max_retained_bytes",
                usize_to_u64(values.max_retained_bytes),
            ),
        ] {
            if value == 0 {
                return Err(InvalidEngineLimits::Zero { field });
            }
        }

        for (field, value, maximum) in [
            (
                "max_conflict_retries",
                values.max_conflict_retries,
                MAX_RETRY_BOUND,
            ),
            (
                "max_ambiguity_attempts",
                values.max_ambiguity_attempts,
                MAX_RETRY_BOUND,
            ),
            ("max_walk_depth", values.max_walk_depth, MAX_DEPTH_BOUND),
            (
                "max_ancestor_depth",
                values.max_ancestor_depth,
                MAX_DEPTH_BOUND,
            ),
        ] {
            if value > maximum {
                return Err(InvalidEngineLimits::TooLarge {
                    field,
                    maximum: u64::from(maximum),
                });
            }
        }

        for (field, value) in [
            ("max_directory_page_bytes", values.max_directory_page_bytes),
            ("max_result_bytes", values.max_result_bytes),
            ("max_fingerprint_bytes", values.max_fingerprint_bytes),
        ] {
            if value > values.max_retained_bytes {
                return Err(InvalidEngineLimits::Inconsistent {
                    field,
                    container: "max_retained_bytes",
                });
            }
        }

        Ok(Self(values))
    }

    /// Returns the validated raw values.
    pub const fn values(self) -> EngineLimitValues {
        self.0
    }

    /// Returns the maximum number of definitive conflict retries.
    pub const fn max_conflict_retries(self) -> u32 {
        self.0.max_conflict_retries
    }

    /// Returns the maximum number of identical ambiguity-resolution attempts.
    pub const fn max_ambiguity_attempts(self) -> u32 {
        self.0.max_ambiguity_attempts
    }

    /// Returns the maximum walk depth.
    pub const fn max_walk_depth(self) -> u32 {
        self.0.max_walk_depth
    }

    /// Returns the maximum directory-ancestor depth.
    pub const fn max_ancestor_depth(self) -> u32 {
        self.0.max_ancestor_depth
    }

    /// Returns the maximum supplementary-group count.
    pub const fn max_supplementary_groups(self) -> u32 {
        self.0.max_supplementary_groups
    }

    /// Returns the maximum directory-page entry count.
    pub const fn max_directory_page_entries(self) -> u32 {
        self.0.max_directory_page_entries
    }

    /// Returns the maximum directory-page byte count.
    pub const fn max_directory_page_bytes(self) -> usize {
        self.0.max_directory_page_bytes
    }

    /// Returns the maximum terminal-result codec size.
    pub const fn max_result_bytes(self) -> usize {
        self.0.max_result_bytes
    }

    /// Returns the maximum fingerprint input size.
    pub const fn max_fingerprint_bytes(self) -> usize {
        self.0.max_fingerprint_bytes
    }

    /// Returns the maximum consistent-read query count.
    pub const fn max_read_queries(self) -> u32 {
        self.0.max_read_queries
    }

    /// Returns the maximum retained precondition/change count.
    pub const fn max_commit_items(self) -> u32 {
        self.0.max_commit_items
    }

    /// Returns the maximum bytes retained by one execution.
    pub const fn max_retained_bytes(self) -> usize {
        self.0.max_retained_bytes
    }

    /// Checks a walk-component count before resolving state.
    pub fn check_walk_depth(self, actual: usize) -> Result<(), EngineLimitError> {
        require_count(EngineLimitKind::WalkDepth, actual, self.max_walk_depth())
    }

    /// Checks a directory-ancestor count before extending a traversal.
    pub fn check_ancestor_depth(self, actual: usize) -> Result<(), EngineLimitError> {
        require_count(
            EngineLimitKind::AncestorDepth,
            actual,
            self.max_ancestor_depth(),
        )
    }

    /// Checks a supplementary-group count before retaining a grant.
    pub fn check_supplementary_groups(self, actual: usize) -> Result<(), EngineLimitError> {
        require_count(
            EngineLimitKind::SupplementaryGroups,
            actual,
            self.max_supplementary_groups(),
        )
    }

    /// Checks an authoritative directory-page entry count.
    pub fn check_directory_page_entries(self, actual: usize) -> Result<(), EngineLimitError> {
        require_count(
            EngineLimitKind::DirectoryPageEntries,
            actual,
            self.max_directory_page_entries(),
        )
    }

    /// Checks an authoritative directory-page byte count.
    pub fn check_directory_page_bytes(self, actual: usize) -> Result<(), EngineLimitError> {
        require_bytes(
            EngineLimitKind::DirectoryPageBytes,
            actual,
            self.max_directory_page_bytes(),
        )
    }

    /// Checks an encoded terminal-result byte count.
    pub fn check_result_bytes(self, actual: usize) -> Result<(), EngineLimitError> {
        require_bytes(
            EngineLimitKind::ResultBytes,
            actual,
            self.max_result_bytes(),
        )
    }

    /// Checks a canonical fingerprint-input byte count.
    pub fn check_fingerprint_bytes(self, actual: usize) -> Result<(), EngineLimitError> {
        require_bytes(
            EngineLimitKind::FingerprintBytes,
            actual,
            self.max_fingerprint_bytes(),
        )
    }

    /// Checks the number of queries in one consistent read.
    pub fn check_read_queries(self, actual: usize) -> Result<(), EngineLimitError> {
        require_count(
            EngineLimitKind::ReadQueries,
            actual,
            self.max_read_queries(),
        )
    }

    /// Checks the number of preconditions and changes in one commit plan.
    pub fn check_commit_items(self, actual: usize) -> Result<(), EngineLimitError> {
        require_count(
            EngineLimitKind::CommitItems,
            actual,
            self.max_commit_items(),
        )
    }

    /// Checks aggregate bytes retained by one execution.
    pub fn check_retained_bytes(self, actual: usize) -> Result<(), EngineLimitError> {
        require_bytes(
            EngineLimitKind::RetainedBytes,
            actual,
            self.max_retained_bytes(),
        )
    }
}

/// Invalid relationship between configured engine bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidEngineLimits {
    /// A required positive limit is zero.
    Zero {
        /// Stable configuration field name.
        field: &'static str,
    },
    /// A safety-constrained limit exceeds its hard maximum.
    TooLarge {
        /// Stable configuration field name.
        field: &'static str,
        /// Largest accepted value.
        maximum: u64,
    },
    /// A child allocation cannot fit within its aggregate retained bound.
    Inconsistent {
        /// Stable child field name.
        field: &'static str,
        /// Stable containing field name.
        container: &'static str,
    },
}

impl fmt::Display for InvalidEngineLimits {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero { field } => write!(formatter, "engine limit {field} must be nonzero"),
            Self::TooLarge { field, maximum } => {
                write!(formatter, "engine limit {field} exceeds {maximum}")
            }
            Self::Inconsistent { field, container } => {
                write!(formatter, "engine limit {field} exceeds {container}")
            }
        }
    }
}

impl std::error::Error for InvalidEngineLimits {}

/// Semantic resource category whose configured bound was exceeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineLimitKind {
    /// Walk components.
    WalkDepth,
    /// Directory ancestors.
    AncestorDepth,
    /// Supplementary groups.
    SupplementaryGroups,
    /// Entries in one directory page.
    DirectoryPageEntries,
    /// Bytes in one directory page.
    DirectoryPageBytes,
    /// Encoded terminal-result bytes.
    ResultBytes,
    /// Canonical fingerprint input bytes.
    FingerprintBytes,
    /// Queries in one consistent read.
    ReadQueries,
    /// Preconditions and changes in one commit plan.
    CommitItems,
    /// Aggregate retained execution bytes.
    RetainedBytes,
}

/// An engine request or retained plan exceeds a configured bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineLimitError {
    /// Resource category.
    pub kind: EngineLimitKind,
    /// Requested or decoded amount.
    pub actual: u64,
    /// Configured maximum.
    pub maximum: u64,
}

impl fmt::Display for EngineLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} amount {} exceeds configured maximum {}",
            self.kind, self.actual, self.maximum
        )
    }
}

impl std::error::Error for EngineLimitError {}

fn require(kind: EngineLimitKind, actual: u64, maximum: u64) -> Result<(), EngineLimitError> {
    if actual > maximum {
        Err(EngineLimitError {
            kind,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn require_count(
    kind: EngineLimitKind,
    actual: usize,
    maximum: u32,
) -> Result<(), EngineLimitError> {
    require(kind, usize_to_u64(actual), u64::from(maximum))
}

fn require_bytes(
    kind: EngineLimitKind,
    actual: usize,
    maximum: usize,
) -> Result<(), EngineLimitError> {
    require(kind, usize_to_u64(actual), usize_to_u64(maximum))
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        assert_eq!(
            EngineLimits::new(EngineLimitValues::default()),
            Ok(EngineLimits::default())
        );
    }

    #[test]
    fn zero_hard_maximum_and_relationships_are_checked() {
        let zero = EngineLimitValues {
            max_read_queries: 0,
            ..EngineLimitValues::default()
        };
        assert_eq!(
            EngineLimits::new(zero),
            Err(InvalidEngineLimits::Zero {
                field: "max_read_queries"
            })
        );

        let excessive = EngineLimitValues {
            max_conflict_retries: MAX_RETRY_BOUND + 1,
            ..EngineLimitValues::default()
        };
        assert!(matches!(
            EngineLimits::new(excessive),
            Err(InvalidEngineLimits::TooLarge {
                field: "max_conflict_retries",
                ..
            })
        ));

        let inconsistent = EngineLimitValues {
            max_directory_page_bytes: 1,
            max_result_bytes: 2,
            max_fingerprint_bytes: 1,
            max_retained_bytes: 1,
            ..EngineLimitValues::default()
        };
        assert!(matches!(
            EngineLimits::new(inconsistent),
            Err(InvalidEngineLimits::Inconsistent {
                field: "max_result_bytes",
                ..
            })
        ));
    }

    #[test]
    fn request_checks_use_checked_widening() {
        let limits = EngineLimits::default();
        assert!(limits.check_walk_depth(usize::MAX).is_err());
        assert!(
            limits
                .check_result_bytes(limits.max_result_bytes() + 1)
                .is_err()
        );
    }
}
