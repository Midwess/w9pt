//! S3 implementation of the backend-neutral target contract.

use aws_sdk_s3::config::{Builder as SdkConfigBuilder, retry::RetryConfig};
use aws_sdk_s3::{
    Client,
    operation::{
        get_object::builders::GetObjectFluentBuilder,
        head_object::builders::HeadObjectFluentBuilder,
        put_object::builders::PutObjectFluentBuilder,
    },
    primitives::ByteStream,
    types::{ChecksumAlgorithm, RequestPayer},
};

use crate::{
    S3ConfigurationError, S3Error, S3Operation, S3QualificationNamespace, S3TargetConfig,
    body::collect_exact,
    classify::{
        MutationFailure, ReadFailure, classify_get_error, classify_head_error, classify_put_error,
        request_ids,
    },
    qualification::{RequestUri, validate_aws_general_purpose_uri},
    range::{ResponseStatus, checked_http_range, validate_content_range},
    version::{decode_etag, encode_etag},
};
use w9pt_fs_storage::{
    CompareExchange, ObjectKey, ObjectRange, ObjectVersion, PutIfAbsent, TargetGuarantees,
    TargetObject, TargetStore,
    testing::{TargetConformanceError, check_target_operations, check_target_pair_operations},
};

#[derive(Clone, Debug)]
struct HeadMetadata {
    length: usize,
    etag: String,
    version: ObjectVersion,
}

/// Amazon S3 adapter for backend-neutral immutable target objects.
///
/// The supplied [`Client`] is cloned through the SDK's shared handle. The
/// adapter never loads configuration or creates a runtime.
#[derive(Clone, Debug)]
pub struct S3Target {
    client: Client,
    config: S3TargetConfig,
    qualified: bool,
}

impl S3Target {
    /// Creates an unqualified adapter candidate from caller-owned inputs.
    ///
    /// Direct operations are available for offline and live qualification, but
    /// [`TargetStore::guarantees`] advertises no writable semantics until two
    /// independently configured candidates pass [`Self::qualify_pair`].
    pub fn new(client: Client, config: S3TargetConfig) -> Result<Self, S3ConfigurationError> {
        Ok(Self {
            client,
            config,
            qualified: false,
        })
    }

    /// Returns the checked target configuration.
    pub const fn config(&self) -> &S3TargetConfig {
        &self.config
    }

    /// Reports whether live single-client and concurrent pair probes succeeded.
    pub const fn is_qualified(&self) -> bool {
        self.qualified
    }

    /// Qualifies two independently configured clients for writable use.
    ///
    /// The probes create immutable and mutable objects below `namespace`. The
    /// caller owns test-prefix isolation and cleanup. No target becomes qualified
    /// unless every exact-read, range, conditional-write, concurrency, and
    /// read-after-write probe succeeds.
    pub async fn qualify_pair(
        mut first: Self,
        mut second: Self,
        namespace: &S3QualificationNamespace,
    ) -> Result<(Self, Self), S3Error> {
        first.verify_aws_endpoint(namespace).await?;
        second.verify_aws_endpoint(namespace).await?;
        Self::probe_pair(&first, &second, namespace).await?;
        first.qualified = true;
        second.qualified = true;
        Ok((first, second))
    }

    /// Runs provider behavior probes without granting writable guarantees.
    ///
    /// This is used to evaluate an exact compatible-provider artifact. Passing
    /// these probes does not establish production durability or create a
    /// supported provider profile.
    pub async fn probe_pair(
        first: &Self,
        second: &Self,
        namespace: &S3QualificationNamespace,
    ) -> Result<(), S3Error> {
        if first.config != second.config {
            return Err(S3Error::Qualification {
                reason: "client configurations differ",
            });
        }
        let longest_suffix = "/pair/concurrent-immutable".len();
        let qualification_length =
            namespace
                .as_str()
                .len()
                .checked_add(longest_suffix)
                .ok_or(S3Error::Qualification {
                    reason: "qualification key length overflow",
                })?;
        if qualification_length > first.config.max_key_bytes() {
            return Err(S3Error::Qualification {
                reason: "qualification namespace exceeds key bound",
            });
        }
        check_target_operations(first, &format!("{}/single", namespace.as_str()))
            .await
            .map_err(qualification_error)?;
        check_target_pair_operations(first, second, &format!("{}/pair", namespace.as_str()))
            .await
            .map_err(qualification_error)?;
        Ok(())
    }

    pub(crate) const fn client(&self) -> &Client {
        &self.client
    }

    async fn verify_aws_endpoint(
        &self,
        namespace: &S3QualificationNamespace,
    ) -> Result<(), S3Error> {
        let observer = RequestUri::default();
        let key = format!("{}/endpoint-check", namespace.as_str());
        let request = self.apply_head_routing(
            self.client()
                .head_object()
                .bucket(self.config.bucket())
                .key(key),
        );
        let _ = request
            .customize()
            .interceptor(observer.clone())
            .send()
            .await;
        let uri = observer.get().ok_or(S3Error::Qualification {
            reason: "client did not produce an observable endpoint request",
        })?;
        validate_aws_general_purpose_uri(&uri, self.config.bucket())
    }

    pub(crate) fn mutation_config_override() -> SdkConfigBuilder {
        SdkConfigBuilder::new().retry_config(Self::mutation_retry_config())
    }

    fn mutation_retry_config() -> RetryConfig {
        RetryConfig::disabled()
    }

    pub(crate) fn key_text<'a>(&self, key: &'a ObjectKey) -> Result<&'a str, S3Error> {
        checked_key_text(key, self.config.max_key_bytes())
    }

    async fn head_object(&self, key: &ObjectKey) -> Result<Option<HeadMetadata>, S3Error> {
        let key = self.key_text(key)?;
        let request = self.apply_head_routing(
            self.client()
                .head_object()
                .bucket(self.config.bucket())
                .key(key),
        );
        let output = match request.send().await {
            Ok(output) => output,
            Err(error) => {
                return match classify_head_error(&error) {
                    ReadFailure::Missing => Ok(None),
                    ReadFailure::PreconditionFailed => Err(S3Error::invalid_response(
                        S3Operation::Head,
                        "unexpected precondition failure",
                        request_ids(&error),
                    )),
                    ReadFailure::Error(error) => Err(error),
                };
            }
        };
        let ids = request_ids(&output);
        let length = output.content_length().ok_or_else(|| {
            S3Error::invalid_response(S3Operation::Head, "missing content length", ids.clone())
        })?;
        let length = usize::try_from(length).map_err(|_| {
            S3Error::invalid_response(
                S3Operation::Head,
                "negative or unrepresentable content length",
                ids.clone(),
            )
        })?;
        let etag = output.e_tag().ok_or_else(|| {
            S3Error::invalid_response(S3Operation::Head, "missing ETag", ids.clone())
        })?;
        let version = encode_etag(etag, self.config.max_etag_bytes()).map_err(|_| {
            S3Error::invalid_response(S3Operation::Head, "invalid ETag", ids.clone())
        })?;
        Ok(Some(HeadMetadata {
            length,
            etag: etag.to_owned(),
            version,
        }))
    }

    async fn resolve_current_version(
        &self,
        key: &ObjectKey,
        operation: S3Operation,
        require_present: bool,
    ) -> Result<Option<ObjectVersion>, S3Error> {
        let attempts = self.config.max_resolution_attempts();
        for attempt in 1..=attempts {
            match self.head_object(key).await {
                Ok(Some(metadata)) => return Ok(Some(metadata.version)),
                Ok(None) if !require_present => return Ok(None),
                Ok(None) => {}
                Err(error) if resolution_observation_is_retryable(&error) => {}
                Err(error) => return Err(error),
            }
            if attempt == attempts {
                return Err(S3Error::CurrentStateResolutionExhausted {
                    operation,
                    attempts,
                });
            }
        }
        unreachable!("configuration rejects a zero current-state resolution bound")
    }

    async fn get_complete(
        &self,
        key: ObjectKey,
        maximum: usize,
    ) -> Result<Option<TargetObject>, S3Error> {
        let maximum = maximum.min(self.config.max_body_bytes());
        let mut races = 0_u32;
        loop {
            let Some(head) = self.head_object(&key).await? else {
                return Ok(None);
            };
            if head.length > maximum {
                return Err(S3Error::Limit {
                    resource: "complete object",
                    actual: u64::try_from(head.length).unwrap_or(u64::MAX),
                    maximum: u64::try_from(maximum).unwrap_or(u64::MAX),
                });
            }
            match self.get_head_version(&key, &head).await? {
                CompleteRead::Object(object) => return Ok(Some(object)),
                CompleteRead::VersionRace => {
                    races = races
                        .checked_add(1)
                        .ok_or(S3Error::ReadRaceExhausted { attempts: u32::MAX })?;
                    if races > self.config.max_read_retries() {
                        return Err(S3Error::ReadRaceExhausted { attempts: races });
                    }
                }
            }
        }
    }

    async fn get_exact_range(
        &self,
        key: ObjectKey,
        range: ObjectRange,
    ) -> Result<Option<Vec<u8>>, S3Error> {
        let Some(http_range) = checked_http_range(range, self.config.max_range_bytes())? else {
            let Some(head) = self.head_object(&key).await? else {
                return Ok(None);
            };
            let offset = usize::try_from(range.start()).map_err(|_| S3Error::ExactRange {
                reason: "empty range offset is unrepresentable",
                request_ids: Default::default(),
            })?;
            if offset > head.length {
                return Err(S3Error::ExactRange {
                    reason: "empty range starts beyond object length",
                    request_ids: Default::default(),
                });
            }
            return Ok(Some(Vec::new()));
        };

        let status = ResponseStatus::default();
        let request = self.apply_get_routing(
            self.client()
                .get_object()
                .bucket(self.config.bucket())
                .key(self.key_text(&key)?)
                .range(http_range.header()),
        );
        let output = match request.customize().interceptor(status.clone()).send().await {
            Ok(output) => output,
            Err(error) => {
                return match classify_get_error(&error, S3Operation::GetRange) {
                    ReadFailure::Missing => Ok(None),
                    ReadFailure::PreconditionFailed => Err(S3Error::ExactRange {
                        reason: "unexpected range precondition failure",
                        request_ids: request_ids(&error),
                    }),
                    ReadFailure::Error(error) => Err(error),
                };
            }
        };
        let ids = request_ids(&output);
        if status.get() != Some(206) {
            return Err(S3Error::ExactRange {
                reason: "S3 ignored range or returned non-206 status",
                request_ids: ids,
            });
        }
        let content_length = output.content_length().ok_or_else(|| S3Error::ExactRange {
            reason: "missing range content length",
            request_ids: ids.clone(),
        })?;
        let content_length = usize::try_from(content_length).map_err(|_| S3Error::ExactRange {
            reason: "negative or unrepresentable range content length",
            request_ids: ids.clone(),
        })?;
        if content_length != http_range.length() {
            return Err(S3Error::ExactRange {
                reason: "range content length mismatch",
                request_ids: ids,
            });
        }
        let content_range = output.content_range().ok_or_else(|| S3Error::ExactRange {
            reason: "missing Content-Range",
            request_ids: ids.clone(),
        })?;
        validate_content_range(content_range, range, ids.clone())?;
        let etag = output.e_tag().ok_or_else(|| S3Error::ExactRange {
            reason: "missing range ETag",
            request_ids: ids.clone(),
        })?;
        encode_etag(etag, self.config.max_etag_bytes()).map_err(|_| S3Error::ExactRange {
            reason: "invalid range ETag",
            request_ids: ids,
        })?;
        let bytes = collect_exact(
            output.body,
            http_range.length(),
            self.config.max_range_bytes(),
            self.config.body_timeout(),
        )
        .await?;
        Ok(Some(bytes))
    }

    async fn create_immutable(
        &self,
        key: ObjectKey,
        bytes: Vec<u8>,
    ) -> Result<PutIfAbsent, S3Error> {
        self.key_text(&key)?;
        if bytes.len() > self.config.max_body_bytes() {
            return Err(S3Error::Limit {
                resource: "put body",
                actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                maximum: u64::try_from(self.config.max_body_bytes()).unwrap_or(u64::MAX),
            });
        }
        let content_length = i64::try_from(bytes.len()).map_err(|_| S3Error::Limit {
            resource: "put body",
            actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            maximum: i64::MAX as u64,
        })?;
        let request = self.apply_put_routing(
            self.client()
                .put_object()
                .bucket(self.config.bucket())
                .key(self.key_text(&key)?)
                .if_none_match("*")
                .content_length(content_length)
                .checksum_algorithm(ChecksumAlgorithm::Crc32C)
                .body(ByteStream::from(bytes)),
        );
        let output = match request
            .customize()
            .config_override(Self::mutation_config_override())
            .send()
            .await
        {
            Ok(output) => output,
            Err(error) => {
                return match classify_put_error(&error, S3Operation::PutIfAbsent) {
                    MutationFailure::PreconditionFailed => {
                        let current = self
                            .resolve_current_version(&key, S3Operation::PutIfAbsent, true)
                            .await?
                            .expect("present-current resolution cannot return absence");
                        Ok(PutIfAbsent::AlreadyExists { version: current })
                    }
                    MutationFailure::ConditionalConflict => {
                        let current = self
                            .resolve_current_version(&key, S3Operation::PutIfAbsent, true)
                            .await?
                            .expect("present-current resolution cannot return absence");
                        Ok(PutIfAbsent::AlreadyExists { version: current })
                    }
                    MutationFailure::Missing => Err(S3Error::service(
                        S3Operation::PutIfAbsent,
                        Some(404),
                        error
                            .as_service_error()
                            .and_then(aws_sdk_s3::error::ProvideErrorMetadata::code),
                        request_ids(&error),
                    )),
                    MutationFailure::Ambiguous => Ok(PutIfAbsent::Ambiguous),
                    MutationFailure::Error(error) => Err(error),
                };
            }
        };
        let Some(etag) = output.e_tag() else {
            return Ok(PutIfAbsent::Ambiguous);
        };
        let Ok(version) = encode_etag(etag, self.config.max_etag_bytes()) else {
            return Ok(PutIfAbsent::Ambiguous);
        };
        Ok(PutIfAbsent::Created { version })
    }

    async fn replace_conditionally(
        &self,
        key: ObjectKey,
        expected: Option<ObjectVersion>,
        bytes: Vec<u8>,
    ) -> Result<CompareExchange, S3Error> {
        self.key_text(&key)?;
        let expected_etag = expected
            .as_ref()
            .map(|version| decode_etag(version, self.config.max_etag_bytes()))
            .transpose()?;
        if bytes.len() > self.config.max_body_bytes() {
            return Err(S3Error::Limit {
                resource: "CAS body",
                actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                maximum: u64::try_from(self.config.max_body_bytes()).unwrap_or(u64::MAX),
            });
        }
        let content_length = i64::try_from(bytes.len()).map_err(|_| S3Error::Limit {
            resource: "CAS body",
            actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            maximum: i64::MAX as u64,
        })?;
        let mut request = self
            .client()
            .put_object()
            .bucket(self.config.bucket())
            .key(self.key_text(&key)?)
            .content_length(content_length)
            .checksum_algorithm(ChecksumAlgorithm::Crc32C)
            .body(ByteStream::from(bytes));
        request = match expected_etag {
            Some(etag) => request.if_match(etag),
            None => request.if_none_match("*"),
        };
        let output = match self
            .apply_put_routing(request)
            .customize()
            .config_override(Self::mutation_config_override())
            .send()
            .await
        {
            Ok(output) => output,
            Err(error) => {
                return match classify_put_error(&error, S3Operation::CompareExchange) {
                    MutationFailure::PreconditionFailed | MutationFailure::ConditionalConflict => {
                        let current = self
                            .resolve_current_version(&key, S3Operation::CompareExchange, false)
                            .await?;
                        Ok(CompareExchange::Conflict { current })
                    }
                    MutationFailure::Missing if expected.is_some() => {
                        let current = self
                            .resolve_current_version(&key, S3Operation::CompareExchange, false)
                            .await?;
                        Ok(CompareExchange::Conflict { current })
                    }
                    MutationFailure::Missing => Err(S3Error::service(
                        S3Operation::CompareExchange,
                        Some(404),
                        error
                            .as_service_error()
                            .and_then(aws_sdk_s3::error::ProvideErrorMetadata::code),
                        request_ids(&error),
                    )),
                    MutationFailure::Ambiguous => Ok(CompareExchange::Ambiguous),
                    MutationFailure::Error(error) => Err(error),
                };
            }
        };
        let Some(etag) = output.e_tag() else {
            return Ok(CompareExchange::Ambiguous);
        };
        let Ok(version) = encode_etag(etag, self.config.max_etag_bytes()) else {
            return Ok(CompareExchange::Ambiguous);
        };
        Ok(CompareExchange::Replaced { version })
    }

    async fn get_head_version(
        &self,
        key: &ObjectKey,
        head: &HeadMetadata,
    ) -> Result<CompleteRead, S3Error> {
        let key_text = self.key_text(key)?;
        let request = self.apply_get_routing(
            self.client()
                .get_object()
                .bucket(self.config.bucket())
                .key(key_text)
                .if_match(&head.etag),
        );
        let output = match request.send().await {
            Ok(output) => output,
            Err(error) => {
                return match classify_get_error(&error, S3Operation::Get) {
                    ReadFailure::PreconditionFailed => Ok(CompleteRead::VersionRace),
                    ReadFailure::Missing => Err(S3Error::invalid_response(
                        S3Operation::Get,
                        "object disappeared after HEAD",
                        request_ids(&error),
                    )),
                    ReadFailure::Error(error) => Err(error),
                };
            }
        };
        let ids = request_ids(&output);
        let length = output.content_length().ok_or_else(|| {
            S3Error::invalid_response(S3Operation::Get, "missing content length", ids.clone())
        })?;
        let length = usize::try_from(length).map_err(|_| {
            S3Error::invalid_response(
                S3Operation::Get,
                "negative or unrepresentable content length",
                ids.clone(),
            )
        })?;
        if length != head.length {
            return Err(S3Error::invalid_response(
                S3Operation::Get,
                "content length differs from HEAD",
                ids,
            ));
        }
        let etag = output.e_tag().ok_or_else(|| {
            S3Error::invalid_response(S3Operation::Get, "missing ETag", ids.clone())
        })?;
        if etag != head.etag {
            return Err(S3Error::invalid_response(
                S3Operation::Get,
                "ETag differs from HEAD",
                ids,
            ));
        }
        let bytes = collect_exact(
            output.body,
            head.length,
            self.config.max_body_bytes(),
            self.config.body_timeout(),
        )
        .await?;
        Ok(CompleteRead::Object(TargetObject::new(
            bytes,
            head.version.clone(),
        )))
    }

    fn apply_head_routing(&self, mut request: HeadObjectFluentBuilder) -> HeadObjectFluentBuilder {
        if let Some(owner) = self.config.expected_bucket_owner() {
            request = request.expected_bucket_owner(owner);
        }
        if self.config.requester_pays() {
            request = request.request_payer(RequestPayer::Requester);
        }
        request
    }

    fn apply_get_routing(&self, mut request: GetObjectFluentBuilder) -> GetObjectFluentBuilder {
        if let Some(owner) = self.config.expected_bucket_owner() {
            request = request.expected_bucket_owner(owner);
        }
        if self.config.requester_pays() {
            request = request.request_payer(RequestPayer::Requester);
        }
        request
    }

    fn apply_put_routing(&self, mut request: PutObjectFluentBuilder) -> PutObjectFluentBuilder {
        if let Some(owner) = self.config.expected_bucket_owner() {
            request = request.expected_bucket_owner(owner);
        }
        if self.config.requester_pays() {
            request = request.request_payer(RequestPayer::Requester);
        }
        request
    }
}

enum CompleteRead {
    Object(TargetObject),
    VersionRace,
}

impl TargetStore for S3Target {
    type Error = S3Error;

    fn guarantees(&self) -> TargetGuarantees {
        if self.qualified {
            TargetGuarantees::REQUIRED
        } else {
            TargetGuarantees::NONE
        }
    }

    async fn get(
        &self,
        key: ObjectKey,
        max_bytes: usize,
    ) -> Result<Option<TargetObject>, Self::Error> {
        self.get_complete(key, max_bytes).await
    }

    async fn get_range(
        &self,
        key: ObjectKey,
        range: ObjectRange,
    ) -> Result<Option<Vec<u8>>, Self::Error> {
        self.get_exact_range(key, range).await
    }

    async fn put_if_absent(
        &self,
        key: ObjectKey,
        bytes: Vec<u8>,
    ) -> Result<PutIfAbsent, Self::Error> {
        self.create_immutable(key, bytes).await
    }

    async fn compare_exchange(
        &self,
        key: ObjectKey,
        expected: Option<ObjectVersion>,
        bytes: Vec<u8>,
    ) -> Result<CompareExchange, Self::Error> {
        self.replace_conditionally(key, expected, bytes).await
    }
}

fn qualification_error(error: TargetConformanceError<S3Error>) -> S3Error {
    match error {
        TargetConformanceError::Target(error) => error,
        TargetConformanceError::Configuration(_) => S3Error::Qualification {
            reason: "candidate unexpectedly required prior guarantees",
        },
        TargetConformanceError::Key(_) => S3Error::Qualification {
            reason: "qualification generated an invalid key",
        },
        TargetConformanceError::Assertion(reason) => S3Error::Qualification { reason },
    }
}

fn resolution_observation_is_retryable(error: &S3Error) -> bool {
    matches!(error, S3Error::TransientRead { .. })
}

fn checked_key_text(key: &ObjectKey, maximum: usize) -> Result<&str, S3Error> {
    let text = key.as_str();
    if text.len() > maximum {
        return Err(S3Error::KeyTooLong {
            actual: text.len(),
            maximum,
        });
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_override_allows_exactly_one_attempt() {
        assert_eq!(S3Target::mutation_retry_config().max_attempts(), 1);
    }

    #[test]
    fn checked_key_routing_preserves_exact_text() {
        for text in [
            "private/v1/data/a b",
            "private/v1/data/a+b=c,d!e_f~g",
            "private//kept/./verbatim/../text",
            "private/unicode-雪",
        ] {
            let key = ObjectKey::new(text).unwrap();
            assert_eq!(checked_key_text(&key, 1_024).unwrap(), text);
        }
        let key = ObjectKey::new("nine-byte!").unwrap();
        assert!(matches!(
            checked_key_text(&key, 8),
            Err(S3Error::KeyTooLong { .. })
        ));
    }
}
