//! Typed and redacted adapter failures.

use core::fmt;

/// Invalid S3 adapter configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum S3ConfigurationError {
    /// The bucket identifier is not a supported general-purpose bucket name.
    InvalidBucket {
        /// Stable, non-sensitive reason for rejection.
        reason: &'static str,
    },
    /// The expected owner is not a twelve-digit AWS account identifier.
    InvalidExpectedBucketOwner,
    /// A required numeric bound is zero.
    ZeroBound {
        /// Configuration field that must be nonzero.
        field: &'static str,
    },
    /// A configured bound exceeds a format or provider maximum.
    BoundTooLarge {
        /// Configuration field that exceeded its maximum.
        field: &'static str,
        /// Supplied bound.
        actual: u64,
        /// Largest accepted bound.
        maximum: u64,
    },
    /// The range-body ceiling exceeds the complete-body ceiling.
    RangeExceedsBody,
    /// The stall timeout is longer than the total body timeout.
    StallExceedsTotalTimeout,
    /// A compatible provider lacks complete recorded qualification evidence.
    UnsupportedProviderProfile,
    /// A live qualification namespace is not strictly test-scoped and unique.
    InvalidQualificationNamespace,
}

impl fmt::Display for S3ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBucket { reason } => write!(formatter, "invalid S3 bucket: {reason}"),
            Self::InvalidExpectedBucketOwner => {
                formatter.write_str("expected S3 bucket owner must contain exactly 12 digits")
            }
            Self::ZeroBound { field } => write!(formatter, "S3 configuration {field} is zero"),
            Self::BoundTooLarge {
                field,
                actual,
                maximum,
            } => write!(
                formatter,
                "S3 configuration {field} value {actual} exceeds maximum {maximum}"
            ),
            Self::RangeExceedsBody => {
                formatter.write_str("S3 maximum range bytes exceeds maximum body bytes")
            }
            Self::StallExceedsTotalTimeout => {
                formatter.write_str("S3 body stall timeout exceeds total timeout")
            }
            Self::UnsupportedProviderProfile => {
                formatter.write_str("S3-compatible provider profile is not qualified")
            }
            Self::InvalidQualificationNamespace => formatter.write_str(
                "S3 qualification namespace must end in w9pt-s3-test- plus a unique run ID",
            ),
        }
    }
}

impl std::error::Error for S3ConfigurationError {}

/// S3 operation associated with a redacted adapter failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum S3Operation {
    /// Metadata lookup.
    Head,
    /// Complete object read.
    Get,
    /// Exact range read.
    GetRange,
    /// Immutable conditional creation.
    PutIfAbsent,
    /// Conditional mutable replacement.
    CompareExchange,
}

/// Bounded safe request identifiers retained from an S3 response.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct S3RequestIds {
    request_id: Option<Box<str>>,
    extended_request_id: Option<Box<str>>,
}

impl S3RequestIds {
    const MAX_IDENTIFIER_BYTES: usize = 256;

    pub(crate) fn new(request_id: Option<&str>, extended_request_id: Option<&str>) -> Self {
        Self {
            request_id: request_id.and_then(safe_identifier),
            extended_request_id: extended_request_id.and_then(safe_identifier),
        }
    }

    /// Returns the bounded AWS request identifier, when safe to retain.
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// Returns the bounded extended S3 request identifier, when safe to retain.
    pub fn extended_request_id(&self) -> Option<&str> {
        self.extended_request_id.as_deref()
    }
}

fn safe_identifier(value: &str) -> Option<Box<str>> {
    if value.is_empty()
        || value.len() > S3RequestIds::MAX_IDENTIFIER_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/' | b'+' | b'=')
        })
    {
        None
    } else {
        Some(value.into())
    }
}

/// Typed, redacted failure returned by [`crate::S3Target`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum S3Error {
    /// A repository key exceeds the configured or S3 key ceiling.
    KeyTooLong {
        /// Key length in UTF-8 bytes.
        actual: usize,
        /// Configured maximum UTF-8 byte length.
        maximum: usize,
    },
    /// An S3 ETag or adapter object-version token is invalid.
    InvalidVersion {
        /// Stable reason that contains no target data.
        reason: &'static str,
    },
    /// A caller or response length exceeds a configured bound.
    Limit {
        /// Resource whose length was checked.
        resource: &'static str,
        /// Observed length.
        actual: u64,
        /// Accepted maximum.
        maximum: u64,
    },
    /// A successful S3 response omitted or contradicted required metadata.
    InvalidResponse {
        /// Operation whose response was invalid.
        operation: S3Operation,
        /// Stable reason that contains no response data.
        reason: &'static str,
        /// Safe request correlation identifiers.
        request_ids: S3RequestIds,
    },
    /// The streamed body exceeded its total deadline.
    BodyTotalTimeout,
    /// The streamed body did not yield within its between-chunk deadline.
    BodyStallTimeout,
    /// The SDK body stream failed before clean end-of-stream.
    BodyStream,
    /// The body length differs from the validated response metadata.
    BodyLength {
        /// Exact length required by metadata.
        expected: u64,
        /// Bytes observed before failure or end-of-stream.
        actual: u64,
    },
    /// Every allowed complete-read restart observed an ETag race.
    ReadRaceExhausted {
        /// Total HEAD/GET attempts made.
        attempts: u32,
    },
    /// Current state could not be observed after a conditional write definitely did not commit.
    CurrentStateResolutionExhausted {
        /// Conditional operation whose current state could not be resolved.
        operation: S3Operation,
        /// Total bounded HEAD observations made.
        attempts: u32,
    },
    /// An idempotent read failed with a retry-safe transient observation.
    TransientRead {
        /// Read operation that could not be observed.
        operation: S3Operation,
        /// Safe request correlation identifiers.
        request_ids: S3RequestIds,
    },
    /// S3 did not prove the exact requested half-open range.
    ExactRange {
        /// Stable reason that contains no response data.
        reason: &'static str,
        /// Safe request correlation identifiers.
        request_ids: S3RequestIds,
    },
    /// An operation has not yet been implemented by this adapter build.
    Unsupported {
        /// Operation that is unavailable.
        operation: S3Operation,
    },
    /// S3 rejected a request with a definite service response.
    Service {
        /// Operation that failed.
        operation: S3Operation,
        /// HTTP status, when a response was available.
        status: Option<u16>,
        /// Bounded modeled service code, when safe to retain.
        code: Option<Box<str>>,
        /// Safe request correlation identifiers.
        request_ids: S3RequestIds,
    },
    /// Live target qualification did not establish the required semantics.
    Qualification {
        /// Stable reason that contains no credentials or target data.
        reason: &'static str,
    },
}

impl fmt::Display for S3Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyTooLong { actual, maximum } => {
                write!(
                    formatter,
                    "S3 object key length {actual} exceeds maximum {maximum}"
                )
            }
            Self::InvalidVersion { reason } => write!(formatter, "invalid S3 version: {reason}"),
            Self::Limit {
                resource,
                actual,
                maximum,
            } => write!(
                formatter,
                "S3 {resource} length {actual} exceeds maximum {maximum}"
            ),
            Self::InvalidResponse {
                operation,
                reason,
                request_ids,
            } => write!(
                formatter,
                "invalid S3 {operation:?} response: {reason} (request_id {:?})",
                request_ids.request_id()
            ),
            Self::BodyTotalTimeout => formatter.write_str("S3 body total timeout elapsed"),
            Self::BodyStallTimeout => formatter.write_str("S3 body stalled"),
            Self::BodyStream => formatter.write_str("S3 body stream failed"),
            Self::BodyLength { expected, actual } => write!(
                formatter,
                "S3 body length {actual} does not equal expected length {expected}"
            ),
            Self::ReadRaceExhausted { attempts } => {
                write!(formatter, "S3 complete read raced {attempts} times")
            }
            Self::CurrentStateResolutionExhausted {
                operation,
                attempts,
            } => write!(
                formatter,
                "S3 {operation:?} current-state resolution exhausted after {attempts} attempts"
            ),
            Self::TransientRead {
                operation,
                request_ids,
            } => write!(
                formatter,
                "transient S3 {operation:?} observation failed (request_id {:?})",
                request_ids.request_id()
            ),
            Self::ExactRange {
                reason,
                request_ids,
            } => write!(
                formatter,
                "invalid exact S3 range response: {reason} (request_id {:?})",
                request_ids.request_id()
            ),
            Self::Unsupported { operation } => {
                write!(formatter, "S3 {operation:?} is not implemented")
            }
            Self::Service {
                operation,
                status,
                code,
                request_ids,
            } => write!(
                formatter,
                "S3 {operation:?} failed (status {status:?}, code {code:?}, request_id {:?})",
                request_ids.request_id()
            ),
            Self::Qualification { reason } => {
                write!(formatter, "S3 qualification failed: {reason}")
            }
        }
    }
}

impl std::error::Error for S3Error {}

impl S3Error {
    pub(crate) fn service(
        operation: S3Operation,
        status: Option<u16>,
        code: Option<&str>,
        request_ids: S3RequestIds,
    ) -> Self {
        Self::Service {
            operation,
            status,
            code: code.and_then(safe_identifier),
            request_ids,
        }
    }

    pub(crate) fn invalid_response(
        operation: S3Operation,
        reason: &'static str,
        request_ids: S3RequestIds,
    ) -> Self {
        Self::InvalidResponse {
            operation,
            reason,
            request_ids,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_keep_only_bounded_safe_identifiers() {
        let identifiers = S3RequestIds::new(Some("request-123"), Some("extended/ABC+=="));
        assert_eq!(identifiers.request_id(), Some("request-123"));
        assert_eq!(identifiers.extended_request_id(), Some("extended/ABC+=="));
        assert_eq!(
            S3RequestIds::new(Some("contains space"), None).request_id(),
            None
        );
        assert_eq!(
            S3RequestIds::new(Some(&"x".repeat(257)), None).request_id(),
            None
        );

        let error = S3Error::service(
            S3Operation::Get,
            Some(403),
            Some("AccessDenied"),
            identifiers,
        );
        let display = error.to_string();
        assert!(display.contains("AccessDenied"));
        assert!(display.contains("request-123"));
        for secret in ["credential", "authorization", "signed-url", "object bytes"] {
            assert!(!display.contains(secret));
        }
    }
}
