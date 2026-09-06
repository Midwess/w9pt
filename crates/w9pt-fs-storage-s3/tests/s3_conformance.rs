#![allow(missing_docs)]

use std::{env, error::Error};

use aws_sdk_s3::{
    Client, Config,
    config::{BehaviorVersion, Credentials, Region},
};
#[cfg(feature = "representation-test")]
use w9pt_fs_storage::{
    CompressionPolicy, ContentCipher, ContentContextId, FileContextScope, FileCryptoContext,
    FileId, FileStoragePolicy, MasterKey, MasterKeyId, SecureEntropy, generate_content_metadata,
    open_committed_context,
};
use w9pt_fs_storage::{
    ContentRepository, CreationDefaults, StorageLimits, StorageMethod, TargetGuarantees,
    TargetStore,
    testing::{
        check_repository_conformance, check_target_conformance, check_target_pair_conformance,
    },
};
use w9pt_fs_storage_s3::{
    S3ConfigurationError, S3ProviderProfile, S3QualificationNamespace, S3Target, S3TargetConfig,
};

mod compatibility_bridge {
    use w9pt_fs_storage::{
        CompareExchange, ObjectKey, ObjectRange, ObjectVersion, PutIfAbsent, TargetGuarantees,
        TargetObject, TargetStore,
    };
    use w9pt_fs_storage_s3::{S3Error, S3QualificationNamespace, S3Target};

    /// Compatibility-only bridge created exclusively after live behavior probes.
    #[derive(Clone, Debug)]
    pub(super) struct CompatibilityProbeTarget {
        inner: S3Target,
    }

    impl TargetStore for CompatibilityProbeTarget {
        type Error = S3Error;

        fn guarantees(&self) -> TargetGuarantees {
            TargetGuarantees::REQUIRED
        }

        async fn get(
            &self,
            key: ObjectKey,
            max_bytes: usize,
        ) -> Result<Option<TargetObject>, Self::Error> {
            self.inner.get(key, max_bytes).await
        }

        async fn get_range(
            &self,
            key: ObjectKey,
            range: ObjectRange,
        ) -> Result<Option<Vec<u8>>, Self::Error> {
            self.inner.get_range(key, range).await
        }

        async fn put_if_absent(
            &self,
            key: ObjectKey,
            bytes: Vec<u8>,
        ) -> Result<PutIfAbsent, Self::Error> {
            self.inner.put_if_absent(key, bytes).await
        }

        async fn compare_exchange(
            &self,
            key: ObjectKey,
            expected: Option<ObjectVersion>,
            bytes: Vec<u8>,
        ) -> Result<CompareExchange, Self::Error> {
            self.inner.compare_exchange(key, expected, bytes).await
        }
    }

    pub(super) async fn probe_and_wrap(
        first: &S3Target,
        second: &S3Target,
        namespace: &S3QualificationNamespace,
    ) -> Result<(CompatibilityProbeTarget, CompatibilityProbeTarget), S3Error> {
        ensure_unqualified(first, second)?;
        S3Target::probe_pair(first, second, namespace).await?;
        ensure_unqualified(first, second)?;
        Ok((
            CompatibilityProbeTarget {
                inner: first.clone(),
            },
            CompatibilityProbeTarget {
                inner: second.clone(),
            },
        ))
    }

    fn ensure_unqualified(first: &S3Target, second: &S3Target) -> Result<(), S3Error> {
        if first.is_qualified()
            || second.is_qualified()
            || first.guarantees() != TargetGuarantees::NONE
            || second.guarantees() != TargetGuarantees::NONE
        {
            return Err(S3Error::Qualification {
                reason: "compatible-provider bridge requires unqualified candidates",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct LiveSettings {
    bucket: String,
    region: String,
    prefix: String,
    expected_owner: Option<String>,
    cleanup: bool,
}

impl LiveSettings {
    fn from_environment() -> Result<Option<Self>, String> {
        let required = env::var("W9PT_S3_TEST_REQUIRED").as_deref() == Ok("1");
        let value = |name: &str| env::var(name).ok().filter(|value| !value.is_empty());
        let bucket = value("W9PT_S3_TEST_BUCKET");
        let region = value("W9PT_S3_TEST_REGION");
        let prefix = value("W9PT_S3_TEST_PREFIX");
        if bucket.is_none() && region.is_none() && prefix.is_none() && !required {
            return Ok(None);
        }
        let settings = Self {
            bucket: bucket.ok_or("W9PT_S3_TEST_BUCKET is required")?,
            region: region.ok_or("W9PT_S3_TEST_REGION is required")?,
            prefix: prefix.ok_or("W9PT_S3_TEST_PREFIX is required")?,
            expected_owner: value("W9PT_S3_TEST_EXPECTED_OWNER"),
            cleanup: env::var("W9PT_S3_TEST_CLEANUP").as_deref() == Ok("1"),
        };
        settings.validate_namespace()?;
        settings.validate_cleanup_target()?;
        Ok(Some(settings))
    }

    fn validate_namespace(&self) -> Result<(), String> {
        S3QualificationNamespace::new(self.prefix.clone())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn validate_cleanup_target(&self) -> Result<(), String> {
        if !self.cleanup {
            return Ok(());
        }
        if !self.bucket.contains("w9pt-test") {
            return Err("cleanup requires a bucket name containing w9pt-test".into());
        }
        if self.expected_owner.is_none() {
            return Err("cleanup requires W9PT_S3_TEST_EXPECTED_OWNER".into());
        }
        Ok(())
    }

    fn qualification_namespace(&self) -> Result<S3QualificationNamespace, Box<dyn Error>> {
        Ok(S3QualificationNamespace::new(self.prefix.clone())?)
    }

    fn target_config(&self) -> Result<S3TargetConfig, Box<dyn Error>> {
        let mut builder = S3TargetConfig::builder(self.bucket.clone());
        if let Some(owner) = &self.expected_owner {
            builder = builder.expected_bucket_owner(owner.clone());
        }
        Ok(builder.build()?)
    }
}

async fn new_client(settings: &LiveSettings) -> Client {
    let shared = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(Region::new(settings.region.clone()))
        .load()
        .await;
    Client::new(&shared)
}

fn local_compatibility_client(endpoint: &str) -> Client {
    Client::from_conf(
        Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .credentials_provider(Credentials::new(
                "w9pt-local-access-key",
                "w9pt-local-secret-key",
                None,
                None,
                "w9pt-local-compatibility",
            ))
            .region(Region::new("us-east-1"))
            .endpoint_url(endpoint)
            .force_path_style(true)
            .build(),
    )
}

fn compatibility_candidates(
    endpoint: &str,
    bucket: &str,
) -> Result<(S3Target, S3Target), Box<dyn Error>> {
    let first_client = local_compatibility_client(endpoint);
    let second_client = local_compatibility_client(endpoint);
    let config = S3TargetConfig::builder(bucket).build()?;
    Ok((
        S3Target::new(first_client, config.clone())?,
        S3Target::new(second_client, config)?,
    ))
}

fn assert_production_guards(first: &S3Target, second: &S3Target) -> Result<(), Box<dyn Error>> {
    if first.is_qualified()
        || second.is_qualified()
        || first.guarantees() != TargetGuarantees::NONE
        || second.guarantees() != TargetGuarantees::NONE
    {
        return Err("compatible S3 candidates unexpectedly acquired writable guarantees".into());
    }
    if ContentRepository::new(
        first.clone(),
        "guard/repository",
        CreationDefaults::new(StorageMethod::Raw),
        StorageLimits::default(),
    )
    .is_ok()
    {
        return Err("unqualified S3 target bypassed repository guarantee validation".into());
    }
    Ok(())
}

async fn cleanup_namespace(client: &Client, settings: &LiveSettings) -> Result<(), Box<dyn Error>> {
    if !settings.cleanup {
        return Ok(());
    }
    settings.validate_namespace()?;
    settings.validate_cleanup_target()?;
    let owner = settings
        .expected_owner
        .as_deref()
        .ok_or("cleanup expected-owner validation was bypassed")?;
    let exact_prefix = format!("{}/", settings.prefix);
    let mut continuation = None;
    for _ in 0..100_u32 {
        let output = client
            .list_objects_v2()
            .bucket(&settings.bucket)
            .prefix(&exact_prefix)
            .expected_bucket_owner(owner)
            .set_continuation_token(continuation)
            .max_keys(1_000)
            .send()
            .await?;
        for object in output.contents() {
            let key = object.key().ok_or("listed S3 object has no key")?;
            if !key.starts_with(&exact_prefix) {
                return Err("S3 cleanup listing escaped exact test namespace".into());
            }
            client
                .delete_object()
                .bucket(&settings.bucket)
                .key(key)
                .expected_bucket_owner(owner)
                .send()
                .await?;
        }
        if !output.is_truncated().unwrap_or(false) {
            return Ok(());
        }
        continuation = Some(
            output
                .next_continuation_token()
                .ok_or("truncated cleanup listing omitted continuation token")?
                .to_owned(),
        );
    }
    Err("S3 cleanup exceeded 100 bounded listing pages".into())
}

#[tokio::test(flavor = "multi_thread")]
async fn live_aws_general_purpose_conformance() -> Result<(), Box<dyn Error>> {
    let Some(settings) = LiveSettings::from_environment()? else {
        eprintln!("skipping live S3 conformance; set W9PT_S3_TEST_REQUIRED=1 to require it");
        return Ok(());
    };
    let target_config = settings.target_config()?;
    let qualification_namespace = settings.qualification_namespace()?;
    let first_client = new_client(&settings).await;
    let second_client = new_client(&settings).await;
    cleanup_namespace(&first_client, &settings).await?;

    let first = S3Target::new(first_client.clone(), target_config.clone())?;
    let second = S3Target::new(second_client, target_config)?;
    let (first, second) = S3Target::qualify_pair(first, second, &qualification_namespace).await?;
    let result = async {
        check_target_conformance(&first, &format!("{}/target", settings.prefix)).await?;
        check_target_pair_conformance(&first, &second, &format!("{}/pair", settings.prefix))
            .await?;
        check_repository_conformance(
            first.clone(),
            second.clone(),
            &format!("{}/repository", settings.prefix),
        )
        .await?;
        Ok::<_, Box<dyn Error>>(())
    }
    .await;

    let cleanup = cleanup_namespace(&first_client, &settings).await;
    result?;
    cleanup?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn live_pinned_compatible_provider_is_exercised_but_remains_unsupported()
-> Result<(), Box<dyn Error>> {
    if env::var("W9PT_S3_COMPAT_TEST_REQUIRED").as_deref() != Ok("1") {
        eprintln!(
            "skipping compatible-provider rejection; set W9PT_S3_COMPAT_TEST_REQUIRED=1 to require it"
        );
        return Ok(());
    }
    let endpoint = env::var("W9PT_S3_COMPAT_TEST_ENDPOINT")?;
    let bucket = env::var("W9PT_S3_COMPAT_TEST_BUCKET")?;
    let provider = env::var("W9PT_S3_COMPAT_TEST_PROVIDER")?;
    let version = env::var("W9PT_S3_COMPAT_TEST_VERSION")?;
    let namespace = S3QualificationNamespace::new(env::var("W9PT_S3_COMPAT_TEST_PREFIX")?)?;

    let (first, second) = compatibility_candidates(&endpoint, &bucket)?;
    assert_production_guards(&first, &second)?;
    S3Target::probe_pair(&first, &second, &namespace).await?;
    assert_production_guards(&first, &second)?;
    assert_eq!(
        S3ProviderProfile::qualified_compatible(&provider, &version),
        Err(S3ConfigurationError::UnsupportedProviderProfile)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn live_pinned_compatible_provider_repository_matrix_remains_unqualified()
-> Result<(), Box<dyn Error>> {
    if env::var("W9PT_S3_COMPAT_REPOSITORY_TEST_REQUIRED").as_deref() != Ok("1") {
        eprintln!(
            "skipping compatible repository matrix; set W9PT_S3_COMPAT_REPOSITORY_TEST_REQUIRED=1 to require it"
        );
        return Ok(());
    }
    if !cfg!(feature = "representation-test") {
        return Err(
            "required compatible-provider matrix lacks representation-test features".into(),
        );
    }
    let endpoint = env::var("W9PT_S3_COMPAT_TEST_ENDPOINT")?;
    let bucket = env::var("W9PT_S3_COMPAT_TEST_BUCKET")?;
    let provider = env::var("W9PT_S3_COMPAT_TEST_PROVIDER")?;
    let version = env::var("W9PT_S3_COMPAT_TEST_VERSION")?;
    if provider != "SeaweedFS" || version != "4.42" {
        return Err(
            "repository compatibility matrix requires exact SeaweedFS 4.42 identity".into(),
        );
    }
    let namespace =
        S3QualificationNamespace::new(env::var("W9PT_S3_COMPAT_REPOSITORY_TEST_PREFIX")?)?;
    let (first, second) = compatibility_candidates(&endpoint, &bucket)?;
    assert_production_guards(&first, &second)?;
    assert_eq!(
        S3ProviderProfile::qualified_compatible(&provider, &version),
        Err(S3ConfigurationError::UnsupportedProviderProfile)
    );

    let (wrapped_first, wrapped_second) =
        compatibility_bridge::probe_and_wrap(&first, &second, &namespace).await?;
    check_repository_conformance(
        wrapped_first.clone(),
        wrapped_second.clone(),
        &format!("{}/matrix", namespace.as_str()),
    )
    .await?;
    #[cfg(feature = "representation-test")]
    {
        let contexts = public_test_contexts()?;
        w9pt_fs_storage::testing::check_repository_context_conformance(
            wrapped_first,
            wrapped_second,
            &format!("{}/representation", namespace.as_str()),
            &contexts,
        )
        .await?;
    }

    assert_production_guards(&first, &second)?;
    assert_eq!(
        S3ProviderProfile::qualified_compatible(&provider, &version),
        Err(S3ConfigurationError::UnsupportedProviderProfile)
    );
    Ok(())
}

#[cfg(feature = "representation-test")]
fn public_test_contexts() -> Result<Vec<FileCryptoContext>, Box<dyn Error>> {
    struct PublicEntropy(u8);

    impl SecureEntropy for PublicEntropy {
        type Error = core::convert::Infallible;

        fn fill_secure(&mut self, destination: &mut [u8]) -> Result<(), Self::Error> {
            destination.fill(self.0);
            self.0 = self.0.wrapping_add(1);
            Ok(())
        }
    }

    let master = MasterKey::new(MasterKeyId::new([0xa1; 16]), [0xa2; 32]);
    let mut contexts = Vec::new();
    for (method_index, method) in [StorageMethod::Raw, StorageMethod::BlockSplit]
        .into_iter()
        .enumerate()
    {
        for (policy_index, (compression, cipher)) in [
            (CompressionPolicy::Identity, ContentCipher::None),
            (CompressionPolicy::Lz4BlockV1, ContentCipher::None),
            (CompressionPolicy::Identity, ContentCipher::Aes256SivV1),
            (CompressionPolicy::Lz4BlockV1, ContentCipher::Aes256SivV1),
        ]
        .into_iter()
        .enumerate()
        {
            let ordinal = u8::try_from(method_index * 4 + policy_index + 1)?;
            let file_id = FileId::from_u128(0x50_0000 + u128::from(ordinal));
            let scope = FileContextScope::new(
                [0xb1; 16],
                [ordinal; 16],
                file_id,
                ContentContextId::from_u128(0x60_0000 + u128::from(ordinal)),
            );
            let policy = FileStoragePolicy::new(method, compression, cipher);
            let mut entropy = PublicEntropy(ordinal);
            let candidate = generate_content_metadata(
                scope,
                policy,
                cipher.is_encrypted().then_some(&master),
                &mut entropy,
            )?;
            contexts.push(open_committed_context(
                scope,
                candidate.policy_format(),
                candidate.policy_bytes(),
                candidate.key_commitment().copied(),
                candidate.wrapped_key_bytes(),
                1,
                cipher.is_encrypted().then_some(&master),
            )?);
        }
    }
    Ok(contexts)
}

#[test]
fn cleanup_namespace_validation_rejects_unsafe_prefixes() {
    for prefix in [
        "production",
        "/w9pt-s3-test-a",
        "w9pt-s3-test-a/",
        "../w9pt-s3-test-0123456789abcdef",
        "parent/w9pt-s3-test-short",
        "w9pt-s3-test-0123456789abcdef/child",
    ] {
        let settings = LiveSettings {
            bucket: "w9pt-test-bucket".into(),
            region: "us-east-1".into(),
            prefix: prefix.into(),
            expected_owner: None,
            cleanup: true,
        };
        assert!(settings.validate_namespace().is_err(), "{prefix}");
    }
}

#[test]
fn cleanup_requires_test_bucket_and_expected_owner() {
    let settings = |bucket: &str, owner: Option<&str>| LiveSettings {
        bucket: bucket.into(),
        region: "us-east-1".into(),
        prefix: "team/w9pt-s3-test-0123456789abcdef".into(),
        expected_owner: owner.map(str::to_owned),
        cleanup: true,
    };
    assert!(
        settings("production-bucket", Some("012345678901"))
            .validate_cleanup_target()
            .is_err()
    );
    assert!(
        settings("w9pt-test-bucket", None)
            .validate_cleanup_target()
            .is_err()
    );
    assert!(
        settings("w9pt-test-bucket", Some("012345678901"))
            .validate_cleanup_target()
            .is_ok()
    );
}
