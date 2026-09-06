//! Private compatibility target used only after the live SeaweedFS behavior probe.

use aws_sdk_s3::{
    Client, Config,
    config::{BehaviorVersion, Credentials, Region},
};
use w9pt_fs_storage::{
    CompareExchange, ObjectKey, ObjectRange, ObjectVersion, PutIfAbsent, TargetGuarantees,
    TargetObject, TargetStore,
};
use w9pt_fs_storage_s3::{
    S3Error, S3QualificationNamespace, S3Target, S3TargetConfig,
};

/// Test-only target that advertises the guarantees established by one exact live probe.
#[derive(Clone, Debug)]
pub struct ProbedContentTarget {
    inner: S3Target,
}

impl TargetStore for ProbedContentTarget {
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

/// Constructs two ordinary unqualified SeaweedFS-compatible candidates.
pub fn candidates(
    endpoint: &str,
    bucket: &str,
) -> Result<(S3Target, S3Target), Box<dyn std::error::Error + Send + Sync>> {
    let config = S3TargetConfig::builder(bucket).build()?;
    Ok((
        S3Target::new(client(endpoint), config.clone())?,
        S3Target::new(client(endpoint), config)?,
    ))
}

/// Probes both candidates and returns a private wrapper without qualifying production targets.
pub async fn probe_and_wrap(
    first: &S3Target,
    second: &S3Target,
    namespace: &S3QualificationNamespace,
) -> Result<(ProbedContentTarget, ProbedContentTarget), S3Error> {
    ensure_unqualified(first, second)?;
    S3Target::probe_pair(first, second, namespace).await?;
    ensure_unqualified(first, second)?;
    Ok((
        ProbedContentTarget {
            inner: first.clone(),
        },
        ProbedContentTarget {
            inner: second.clone(),
        },
    ))
}

/// Verifies that the production adapter still rejects the compatible provider profile.
pub fn ensure_unqualified(first: &S3Target, second: &S3Target) -> Result<(), S3Error> {
    if first.is_qualified()
        || second.is_qualified()
        || first.guarantees() != TargetGuarantees::NONE
        || second.guarantees() != TargetGuarantees::NONE
    {
        return Err(S3Error::Qualification {
            reason: "composition wrapper requires unqualified candidates",
        });
    }
    Ok(())
}

fn client(endpoint: &str) -> Client {
    Client::from_conf(
        Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .credentials_provider(Credentials::new(
                "w9pt-local-access-key",
                "w9pt-local-secret-key",
                None,
                None,
                "w9pt-content-composition",
            ))
            .region(Region::new("us-east-1"))
            .endpoint_url(endpoint)
            .force_path_style(true)
            .build(),
    )
}
