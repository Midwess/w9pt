//! Checked caller-owned adapter configuration.

use std::time::Duration;

use crate::S3ConfigurationError;

const S3_MAX_KEY_BYTES: usize = 1_024;
const DEFAULT_MAX_ETAG_BYTES: usize = 1_024;
const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_READ_RETRIES: u32 = 2;
const DEFAULT_MAX_RESOLUTION_ATTEMPTS: u32 = 3;
const QUALIFICATION_MARKER: &str = "w9pt-s3-test-";
const MIN_QUALIFICATION_RUN_ID_BYTES: usize = 16;

/// S3 provider semantics accepted by this adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum S3ProviderProfile {
    /// Amazon S3 general-purpose bucket with documented strong consistency.
    AwsGeneralPurpose,
}

impl S3ProviderProfile {
    /// Requests a compatible-provider profile backed by external qualification evidence.
    ///
    /// Version 0.1 has no qualified compatible profiles, so every request fails
    /// closed. A future accepted profile must record exact provider, endpoint,
    /// conditional-write, range, checksum, consistency, durability, versioning,
    /// and fault-test evidence before this constructor can return a profile.
    pub fn qualified_compatible(
        _provider: &str,
        _version: &str,
    ) -> Result<Self, S3ConfigurationError> {
        Err(S3ConfigurationError::UnsupportedProviderProfile)
    }
}

/// Strict caller-supplied namespace used only for live provider qualification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3QualificationNamespace(Box<str>);

impl S3QualificationNamespace {
    /// Validates a relative namespace ending in a unique test-run marker.
    pub fn new(value: impl Into<String>) -> Result<Self, S3ConfigurationError> {
        let value = value.into();
        let components = value.split('/').collect::<Vec<_>>();
        let valid_components = !value.starts_with('/')
            && !value.ends_with('/')
            && !value.bytes().any(|byte| byte.is_ascii_control())
            && components
                .iter()
                .all(|component| !component.is_empty() && *component != "." && *component != "..");
        let valid_run = components.last().is_some_and(|component| {
            component
                .strip_prefix(QUALIFICATION_MARKER)
                .is_some_and(|run| {
                    run.len() >= MIN_QUALIFICATION_RUN_ID_BYTES
                        && run
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                })
        });
        if !valid_components || !valid_run {
            return Err(S3ConfigurationError::InvalidQualificationNamespace);
        }
        Ok(Self(value.into_boxed_str()))
    }

    /// Returns the exact private namespace used by qualification requests.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Explicit deadline policy for consuming one returned streaming body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BodyTimeout {
    total: Duration,
    stall: Duration,
}

impl BodyTimeout {
    /// Creates a checked total and between-chunk deadline policy.
    pub fn new(total: Duration, stall: Duration) -> Result<Self, S3ConfigurationError> {
        if total.is_zero() {
            return Err(S3ConfigurationError::ZeroBound {
                field: "body total timeout",
            });
        }
        if stall.is_zero() {
            return Err(S3ConfigurationError::ZeroBound {
                field: "body stall timeout",
            });
        }
        if stall > total {
            return Err(S3ConfigurationError::StallExceedsTotalTimeout);
        }
        Ok(Self { total, stall })
    }

    /// Returns the total body-consumption deadline.
    pub const fn total(self) -> Duration {
        self.total
    }

    /// Returns the maximum wait between successive body chunks.
    pub const fn stall(self) -> Duration {
        self.stall
    }
}

impl Default for BodyTimeout {
    fn default() -> Self {
        Self {
            total: Duration::from_secs(30),
            stall: Duration::from_secs(5),
        }
    }
}

/// Validated configuration for one S3 target adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct S3TargetConfig {
    bucket: Box<str>,
    expected_bucket_owner: Option<Box<str>>,
    requester_pays: bool,
    provider: S3ProviderProfile,
    max_key_bytes: usize,
    max_etag_bytes: usize,
    max_body_bytes: usize,
    max_range_bytes: usize,
    max_read_retries: u32,
    max_resolution_attempts: u32,
    body_timeout: BodyTimeout,
}

impl S3TargetConfig {
    /// Starts a checked configuration for an Amazon S3 general-purpose bucket.
    pub fn builder(bucket: impl Into<String>) -> S3TargetConfigBuilder {
        S3TargetConfigBuilder::new(bucket)
    }

    /// Returns the exact bucket name sent with every request.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Returns the optional expected AWS account owner.
    pub fn expected_bucket_owner(&self) -> Option<&str> {
        self.expected_bucket_owner.as_deref()
    }

    /// Reports whether requester-pays acknowledgement is sent.
    pub const fn requester_pays(&self) -> bool {
        self.requester_pays
    }

    /// Returns the qualified provider contract.
    pub const fn provider(&self) -> S3ProviderProfile {
        self.provider
    }

    /// Returns the maximum repository key length accepted before dispatch.
    pub const fn max_key_bytes(&self) -> usize {
        self.max_key_bytes
    }

    /// Returns the maximum ETag length accepted from S3.
    pub const fn max_etag_bytes(&self) -> usize {
        self.max_etag_bytes
    }

    /// Returns the hard maximum complete object size.
    pub const fn max_body_bytes(&self) -> usize {
        self.max_body_bytes
    }

    /// Returns the hard maximum exact range size.
    pub const fn max_range_bytes(&self) -> usize {
        self.max_range_bytes
    }

    /// Returns the number of complete-read restarts allowed after the first attempt.
    pub const fn max_read_retries(&self) -> u32 {
        self.max_read_retries
    }

    /// Returns the maximum current-state observations after a conditional conflict.
    pub const fn max_resolution_attempts(&self) -> u32 {
        self.max_resolution_attempts
    }

    /// Returns the body total/stall timeout policy.
    pub const fn body_timeout(&self) -> BodyTimeout {
        self.body_timeout
    }
}

/// Builder that validates all S3 adapter configuration as one unit.
#[derive(Clone, Debug)]
pub struct S3TargetConfigBuilder {
    bucket: String,
    expected_bucket_owner: Option<String>,
    requester_pays: bool,
    provider: S3ProviderProfile,
    max_key_bytes: usize,
    max_etag_bytes: usize,
    max_body_bytes: usize,
    max_range_bytes: usize,
    max_read_retries: u32,
    max_resolution_attempts: u32,
    body_timeout: BodyTimeout,
}

impl S3TargetConfigBuilder {
    fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            expected_bucket_owner: None,
            requester_pays: false,
            provider: S3ProviderProfile::AwsGeneralPurpose,
            max_key_bytes: S3_MAX_KEY_BYTES,
            max_etag_bytes: DEFAULT_MAX_ETAG_BYTES,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_range_bytes: DEFAULT_MAX_BODY_BYTES,
            max_read_retries: DEFAULT_MAX_READ_RETRIES,
            max_resolution_attempts: DEFAULT_MAX_RESOLUTION_ATTEMPTS,
            body_timeout: BodyTimeout::default(),
        }
    }

    /// Sets the expected twelve-digit AWS account owner.
    pub fn expected_bucket_owner(mut self, owner: impl Into<String>) -> Self {
        self.expected_bucket_owner = Some(owner.into());
        self
    }

    /// Enables or disables requester-pays acknowledgement.
    pub const fn requester_pays(mut self, enabled: bool) -> Self {
        self.requester_pays = enabled;
        self
    }

    /// Sets the qualified provider contract.
    pub const fn provider(mut self, provider: S3ProviderProfile) -> Self {
        self.provider = provider;
        self
    }

    /// Sets the maximum accepted repository key length.
    pub const fn max_key_bytes(mut self, value: usize) -> Self {
        self.max_key_bytes = value;
        self
    }

    /// Sets the maximum accepted ETag length.
    pub const fn max_etag_bytes(mut self, value: usize) -> Self {
        self.max_etag_bytes = value;
        self
    }

    /// Sets the hard maximum complete body length.
    pub const fn max_body_bytes(mut self, value: usize) -> Self {
        self.max_body_bytes = value;
        self
    }

    /// Sets the hard maximum exact range length.
    pub const fn max_range_bytes(mut self, value: usize) -> Self {
        self.max_range_bytes = value;
        self
    }

    /// Sets complete-read restarts allowed after the first attempt.
    pub const fn max_read_retries(mut self, value: u32) -> Self {
        self.max_read_retries = value;
        self
    }

    /// Sets the bounded current-state observation count.
    pub const fn max_resolution_attempts(mut self, value: u32) -> Self {
        self.max_resolution_attempts = value;
        self
    }

    /// Sets the streaming body deadline policy.
    pub const fn body_timeout(mut self, value: BodyTimeout) -> Self {
        self.body_timeout = value;
        self
    }

    /// Validates all fields and returns an immutable configuration.
    pub fn build(self) -> Result<S3TargetConfig, S3ConfigurationError> {
        validate_bucket(&self.bucket)?;
        if self.expected_bucket_owner.as_deref().is_some_and(|owner| {
            owner.len() != 12 || !owner.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return Err(S3ConfigurationError::InvalidExpectedBucketOwner);
        }
        validate_nonzero("max_key_bytes", self.max_key_bytes)?;
        validate_nonzero("max_etag_bytes", self.max_etag_bytes)?;
        validate_nonzero("max_body_bytes", self.max_body_bytes)?;
        validate_nonzero("max_range_bytes", self.max_range_bytes)?;
        validate_nonzero(
            "max_resolution_attempts",
            usize::try_from(self.max_resolution_attempts).unwrap_or(usize::MAX),
        )?;
        if self.max_key_bytes > S3_MAX_KEY_BYTES {
            return Err(S3ConfigurationError::BoundTooLarge {
                field: "max_key_bytes",
                actual: u64::try_from(self.max_key_bytes).unwrap_or(u64::MAX),
                maximum: S3_MAX_KEY_BYTES as u64,
            });
        }
        if self.max_etag_bytes > usize::from(u16::MAX) {
            return Err(S3ConfigurationError::BoundTooLarge {
                field: "max_etag_bytes",
                actual: u64::try_from(self.max_etag_bytes).unwrap_or(u64::MAX),
                maximum: u64::from(u16::MAX),
            });
        }
        if self.max_range_bytes > self.max_body_bytes {
            return Err(S3ConfigurationError::RangeExceedsBody);
        }
        Ok(S3TargetConfig {
            bucket: self.bucket.into_boxed_str(),
            expected_bucket_owner: self.expected_bucket_owner.map(String::into_boxed_str),
            requester_pays: self.requester_pays,
            provider: self.provider,
            max_key_bytes: self.max_key_bytes,
            max_etag_bytes: self.max_etag_bytes,
            max_body_bytes: self.max_body_bytes,
            max_range_bytes: self.max_range_bytes,
            max_read_retries: self.max_read_retries,
            max_resolution_attempts: self.max_resolution_attempts,
            body_timeout: self.body_timeout,
        })
    }
}

fn validate_nonzero(field: &'static str, value: usize) -> Result<(), S3ConfigurationError> {
    if value == 0 {
        Err(S3ConfigurationError::ZeroBound { field })
    } else {
        Ok(())
    }
}

fn validate_bucket(bucket: &str) -> Result<(), S3ConfigurationError> {
    if !(3..=63).contains(&bucket.len()) {
        return Err(S3ConfigurationError::InvalidBucket {
            reason: "name length must be 3 through 63 bytes",
        });
    }
    if !bucket.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
    }) {
        return Err(S3ConfigurationError::InvalidBucket {
            reason: "name contains unsupported characters",
        });
    }
    if !bucket
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        || !bucket
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(S3ConfigurationError::InvalidBucket {
            reason: "name must begin and end with a letter or digit",
        });
    }
    if bucket.contains("..") || bucket.contains(".-") || bucket.contains("-.") {
        return Err(S3ConfigurationError::InvalidBucket {
            reason: "name contains adjacent invalid punctuation",
        });
    }
    if bucket.split('.').count() == 4 && bucket.split('.').all(|part| part.parse::<u8>().is_ok()) {
        return Err(S3ConfigurationError::InvalidBucket {
            reason: "IP-address-style names are unsupported",
        });
    }
    if ["xn--", "sthree-", "amzn-s3-demo-"]
        .iter()
        .any(|prefix| bucket.starts_with(prefix))
        || ["-s3alias", "--ol-s3", ".mrap", "--x-s3", "--table-s3"]
            .iter()
            .any(|suffix| bucket.ends_with(suffix))
    {
        return Err(S3ConfigurationError::InvalidBucket {
            reason: "name uses a reserved non-general-purpose form",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bounded_and_explicit() {
        let config = S3TargetConfig::builder("w9pt-test-bucket")
            .expected_bucket_owner("012345678901")
            .requester_pays(true)
            .build()
            .unwrap();
        assert_eq!(config.bucket(), "w9pt-test-bucket");
        assert_eq!(config.expected_bucket_owner(), Some("012345678901"));
        assert!(config.requester_pays());
        assert_eq!(config.provider(), S3ProviderProfile::AwsGeneralPurpose);
        assert_eq!(config.max_key_bytes(), 1_024);
        assert_eq!(config.max_etag_bytes(), 1_024);
        assert_eq!(config.max_body_bytes(), 64 * 1024 * 1024);
        assert_eq!(config.max_range_bytes(), 64 * 1024 * 1024);
        assert_eq!(config.max_read_retries(), 2);
        assert_eq!(config.max_resolution_attempts(), 3);
    }

    #[test]
    fn invalid_names_owners_limits_and_timeouts_are_rejected() {
        for bucket in ["ab", "Uppercase", "127.0.0.1", "bad..dots", "-leading"] {
            assert!(S3TargetConfig::builder(bucket).build().is_err(), "{bucket}");
        }
        assert!(
            S3TargetConfig::builder("valid-bucket")
                .expected_bucket_owner("123")
                .build()
                .is_err()
        );
        assert!(
            S3TargetConfig::builder("valid-bucket")
                .max_etag_bytes(0)
                .build()
                .is_err()
        );
        assert!(
            S3TargetConfig::builder("valid-bucket")
                .max_key_bytes(1_025)
                .build()
                .is_err()
        );
        assert!(
            S3TargetConfig::builder("valid-bucket")
                .max_body_bytes(10)
                .max_range_bytes(11)
                .build()
                .is_err()
        );
        assert!(BodyTimeout::new(Duration::ZERO, Duration::from_secs(1)).is_err());
        assert!(BodyTimeout::new(Duration::from_secs(1), Duration::from_secs(2)).is_err());
    }

    #[test]
    fn unqualified_compatible_profile_is_explicitly_unsupported() {
        assert_eq!(
            S3ProviderProfile::qualified_compatible("SeaweedFS", "4.42"),
            Err(S3ConfigurationError::UnsupportedProviderProfile)
        );
    }

    #[test]
    fn unsupported_bucket_forms_and_weak_qualification_namespaces_are_rejected() {
        for bucket in [
            "xn--bucket",
            "sthree-bucket",
            "amzn-s3-demo-bucket",
            "bucket-s3alias",
            "bucket--ol-s3",
            "bucket.mrap",
            "bucket--zone--x-s3",
            "bucket--table-s3",
        ] {
            assert!(S3TargetConfig::builder(bucket).build().is_err(), "{bucket}");
        }
        for namespace in [
            "production",
            "production/w9pt-s3-test-short",
            "w9pt-s3-test-1234567890123456/child",
            "../w9pt-s3-test-1234567890123456",
        ] {
            assert!(
                S3QualificationNamespace::new(namespace).is_err(),
                "{namespace}"
            );
        }
        assert_eq!(
            S3QualificationNamespace::new("team/w9pt-s3-test-0123456789abcdef")
                .unwrap()
                .as_str(),
            "team/w9pt-s3-test-0123456789abcdef"
        );
    }
}
