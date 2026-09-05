//! Shared immutable object preparation and verified manifest loading.

use crate::{
    AmbiguityError, AmbiguousOperation, ContentRef, CorruptionError, CreationDefaults, Digest,
    FileId, KeySpace, LimitError, LimitKind, MissingObjectKind, MutationId, ObjectKey,
    PreparedContent, PutIfAbsent, StorageError, StorageLimits, TargetError, TargetOperation,
    TargetStore,
    format::{
        BlobRef, FileManifest, ManifestLayout, ObjectKind, decode_envelope, decode_manifest,
        encode_envelope, encode_manifest,
    },
    keys::PreparationKey,
};

/// Runtime-neutral repository for immutable logical file content.
#[derive(Clone, Debug)]
pub struct ContentRepository<S> {
    target: S,
    keys: KeySpace,
    defaults: CreationDefaults,
    limits: StorageLimits,
}

impl<S: TargetStore> ContentRepository<S> {
    /// Constructs a writable repository after validating target guarantees and key bounds.
    pub fn new(
        target: S,
        private_prefix: impl Into<String>,
        defaults: CreationDefaults,
        limits: StorageLimits,
    ) -> Result<Self, crate::ConfigurationError> {
        target.guarantees().validate_writable()?;
        let keys = KeySpace::new(private_prefix, limits)?;
        Ok(Self {
            target,
            keys,
            defaults,
            limits,
        })
    }

    /// Returns the caller-provided target adapter.
    pub const fn target(&self) -> &S {
        &self.target
    }

    /// Returns the validated private keyspace.
    pub const fn keys(&self) -> &KeySpace {
        &self.keys
    }

    /// Returns defaults used only for newly created files.
    pub const fn defaults(&self) -> CreationDefaults {
        self.defaults
    }

    /// Returns checked amplification and retry limits.
    pub const fn limits(&self) -> StorageLimits {
        self.limits
    }

    /// Returns an object-backed publisher borrowing this repository.
    pub const fn publisher(&self) -> crate::ObjectHeadPublisher<'_, S> {
        crate::ObjectHeadPublisher::new(self)
    }

    /// Loads, hashes, decodes, and context-validates an immutable manifest.
    pub async fn load_manifest(
        &self,
        content: &ContentRef,
    ) -> Result<FileManifest, StorageError<S::Error>> {
        let key = content.manifest_key().clone();
        let preparation = self.validate_manifest_key(content.file_id(), &key)?;
        preparation.validate_result_generation(content.generation())?;
        let bytes = self
            .get_required(key.clone(), MissingObjectKind::Manifest)
            .await?;
        if Digest::blake3(&bytes) != content.manifest_digest() {
            return Err(CorruptionError::DigestMismatch.into());
        }
        let manifest = decode_manifest(&bytes, self.limits).map_err(StorageError::from)?;
        manifest
            .validate_content_ref(content)
            .map_err(StorageError::from)?;
        self.validate_manifest_payloads(&manifest)?;
        Ok(manifest)
    }

    /// Verifies that a portable reference can be independently reopened.
    pub async fn validate_content(
        &self,
        content: &ContentRef,
    ) -> Result<(), StorageError<S::Error>> {
        self.load_manifest(content).await.map(|_| ())
    }

    /// Confirms the version-1 content-only durability barrier.
    ///
    /// Version 1 has no write-back state: successful immutable puts and head CAS
    /// operations are already durable by the target contract. This validates the
    /// referenced immutable manifest and does not claim inode, namespace, or other
    /// future filesystem metadata durability.
    pub async fn sync_content(&self, content: &ContentRef) -> Result<(), StorageError<S::Error>> {
        self.validate_content(content).await
    }

    /// Prepares initial immutable content using the configured default method.
    ///
    /// The default is consulted only here. Every later operation dispatches from
    /// the method persisted in `content` and its checked manifest.
    pub async fn prepare_create(
        &self,
        file_id: FileId,
        mutation_id: MutationId,
        attempt: u32,
        bytes: &[u8],
    ) -> Result<PreparedContent, StorageError<S::Error>> {
        match self.defaults.method() {
            crate::StorageMethod::Raw => {
                crate::layout::create_raw(self, file_id, mutation_id, attempt, bytes).await
            }
            crate::StorageMethod::BlockSplit => {
                crate::layout::create_block_split(self, file_id, mutation_id, attempt, bytes).await
            }
        }
    }

    /// Reads a positioned range according to the method persisted in the manifest.
    pub async fn read(
        &self,
        content: &ContentRef,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, StorageError<S::Error>> {
        check_input_limit(LimitKind::Read, length, self.limits.max_read_bytes())?;
        let manifest = self.load_manifest(content).await?;
        match manifest.method() {
            crate::StorageMethod::Raw => {
                crate::layout::read_raw(self, &manifest, offset, length).await
            }
            crate::StorageMethod::BlockSplit => {
                crate::layout::read_block_split(self, &manifest, offset, length).await
            }
        }
    }

    /// Prepares a positioned write against one immutable base content version.
    pub async fn prepare_write(
        &self,
        content: &ContentRef,
        mutation_id: MutationId,
        attempt: u32,
        offset: u64,
        data: &[u8],
    ) -> Result<PreparedContent, StorageError<S::Error>> {
        check_input_limit(LimitKind::Write, data.len(), self.limits.max_write_bytes())?;
        crate::layout::LogicalRange::from_usize(offset, data.len())?;
        let manifest = self.load_manifest(content).await?;
        match manifest.method() {
            crate::StorageMethod::Raw => {
                crate::layout::write_raw(
                    self,
                    content,
                    &manifest,
                    mutation_id,
                    attempt,
                    offset,
                    data,
                )
                .await
            }
            crate::StorageMethod::BlockSplit => {
                crate::layout::write_block_split(
                    self,
                    content,
                    &manifest,
                    mutation_id,
                    attempt,
                    offset,
                    data,
                )
                .await
            }
        }
    }

    /// Prepares a positioned first write against the distinguished unpublished-file base.
    ///
    /// The configured creation method is bound into the preparation identity. The returned
    /// content always has generation one, and no mutable object head is published.
    pub async fn prepare_write_from_new(
        &self,
        file_id: FileId,
        mutation_id: MutationId,
        attempt: u32,
        offset: u64,
        data: &[u8],
    ) -> Result<PreparedContent, StorageError<S::Error>> {
        check_input_limit(LimitKind::Write, data.len(), self.limits.max_write_bytes())?;
        crate::layout::LogicalRange::from_usize(offset, data.len())?;
        match self.defaults.method() {
            crate::StorageMethod::Raw => {
                crate::layout::write_raw_from_new(self, file_id, mutation_id, attempt, offset, data)
                    .await
            }
            crate::StorageMethod::BlockSplit => {
                crate::layout::write_block_split_from_new(
                    self,
                    file_id,
                    mutation_id,
                    attempt,
                    offset,
                    data,
                )
                .await
            }
        }
    }

    /// Prepares a logical-size change against one immutable base content version.
    pub async fn prepare_truncate(
        &self,
        content: &ContentRef,
        mutation_id: MutationId,
        attempt: u32,
        logical_size: u64,
    ) -> Result<PreparedContent, StorageError<S::Error>> {
        let manifest = self.load_manifest(content).await?;
        match manifest.method() {
            crate::StorageMethod::Raw => {
                crate::layout::truncate_raw(
                    self,
                    content,
                    &manifest,
                    mutation_id,
                    attempt,
                    logical_size,
                )
                .await
            }
            crate::StorageMethod::BlockSplit => {
                crate::layout::truncate_block_split(
                    self,
                    content,
                    &manifest,
                    mutation_id,
                    attempt,
                    logical_size,
                )
                .await
            }
        }
    }

    /// Prepares an initial logical-size publication from the unpublished-file base.
    ///
    /// Zero and nonzero sizes both produce a canonical generation-one manifest. Raw content
    /// materializes the checked zero-filled logical file; block-split content retains only holes.
    pub async fn prepare_truncate_from_new(
        &self,
        file_id: FileId,
        mutation_id: MutationId,
        attempt: u32,
        logical_size: u64,
    ) -> Result<PreparedContent, StorageError<S::Error>> {
        match self.defaults.method() {
            crate::StorageMethod::Raw => {
                crate::layout::truncate_raw_from_new(
                    self,
                    file_id,
                    mutation_id,
                    attempt,
                    logical_size,
                )
                .await
            }
            crate::StorageMethod::BlockSplit => {
                crate::layout::truncate_block_split_from_new(
                    self,
                    file_id,
                    mutation_id,
                    attempt,
                    logical_size,
                )
                .await
            }
        }
    }

    pub(crate) async fn store_raw_payload(
        &self,
        file_id: FileId,
        identity: crate::PreparationIdentity,
        attempt: u32,
        plaintext: &[u8],
    ) -> Result<BlobRef, StorageError<S::Error>> {
        let key = self.keys.raw_payload(file_id, identity, attempt);
        self.store_payload(key, plaintext).await
    }

    pub(crate) async fn store_block_payload(
        &self,
        file_id: FileId,
        identity: crate::PreparationIdentity,
        attempt: u32,
        block_index: u64,
        plaintext: &[u8],
    ) -> Result<BlobRef, StorageError<S::Error>> {
        let key = self
            .keys
            .block_payload(file_id, identity, attempt, block_index);
        self.store_payload(key, plaintext).await
    }

    pub(crate) async fn load_payload(
        &self,
        blob: &BlobRef,
    ) -> Result<Vec<u8>, StorageError<S::Error>> {
        let bytes = self
            .get_required(blob.key().clone(), MissingObjectKind::Payload)
            .await?;
        let envelope = decode_envelope(ObjectKind::Payload, &bytes, self.limits.max_object_bytes())
            .map_err(StorageError::from)?;
        let payload = envelope.payload();
        let actual = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        if actual != blob.stored_len() || actual != blob.plaintext_len() {
            return Err(CorruptionError::InvalidLength {
                field: "identity payload",
                expected: blob.plaintext_len(),
                actual,
            }
            .into());
        }
        if Digest::blake3(payload) != blob.digest() {
            return Err(CorruptionError::DigestMismatch.into());
        }
        Ok(payload.to_vec())
    }

    pub(crate) async fn prepare_manifest(
        &self,
        manifest: &FileManifest,
        identity: crate::PreparationIdentity,
        attempt: u32,
        content_changed: bool,
    ) -> Result<PreparedContent, StorageError<S::Error>> {
        let key = self.keys.manifest(manifest.file_id(), identity, attempt);
        let encoded = encode_manifest(manifest, self.limits).map_err(StorageError::from)?;
        self.put_immutable(key.clone(), encoded.clone(), MissingObjectKind::Manifest)
            .await?;
        let reference = manifest.content_ref(key, Digest::blake3(&encoded));
        Ok(PreparedContent::new(
            reference,
            content_changed,
            identity,
            attempt,
        ))
    }

    pub(crate) fn validate_manifest_key(
        &self,
        file_id: FileId,
        key: &ObjectKey,
    ) -> Result<PreparationKey, StorageError<S::Error>> {
        self.keys
            .parse_manifest(file_id, key)
            .map_err(StorageError::from)
    }

    pub(crate) fn validate_manifest_payloads(
        &self,
        manifest: &FileManifest,
    ) -> Result<(), StorageError<S::Error>> {
        match manifest.layout() {
            ManifestLayout::Raw { blob } => {
                if let Some(blob) = blob {
                    self.keys
                        .validate_raw_payload(manifest.file_id(), blob.key())?;
                }
            }
            ManifestLayout::BlockSplit { blocks, .. } => {
                for entry in blocks {
                    self.keys.validate_block_payload(
                        manifest.file_id(),
                        entry.index(),
                        entry.blob().key(),
                    )?;
                }
            }
        }
        Ok(())
    }

    async fn store_payload(
        &self,
        key: ObjectKey,
        plaintext: &[u8],
    ) -> Result<BlobRef, StorageError<S::Error>> {
        let plaintext_len = u64::try_from(plaintext.len()).map_err(|_| {
            LimitError::new(
                LimitKind::Object,
                u64::MAX,
                u64::try_from(self.limits.max_object_bytes()).unwrap_or(u64::MAX),
            )
        })?;
        let encoded = encode_envelope(
            ObjectKind::Payload,
            plaintext,
            self.limits.max_object_bytes(),
        )
        .map_err(StorageError::from)?;
        self.put_immutable(key.clone(), encoded, MissingObjectKind::Payload)
            .await?;
        Ok(BlobRef::new(
            key,
            plaintext_len,
            plaintext_len,
            Digest::blake3(plaintext),
        ))
    }

    async fn put_immutable(
        &self,
        key: ObjectKey,
        bytes: Vec<u8>,
        kind: MissingObjectKind,
    ) -> Result<(), StorageError<S::Error>> {
        let outcome = self
            .target
            .put_if_absent(key.clone(), bytes.clone())
            .await
            .map_err(|source| {
                StorageError::Target(TargetError {
                    operation: TargetOperation::PutIfAbsent,
                    key: key.clone(),
                    source,
                })
            })?;
        match outcome {
            PutIfAbsent::Created { .. } => Ok(()),
            PutIfAbsent::AlreadyExists { .. } => {
                let existing = self.get_required(key, kind).await?;
                if existing == bytes {
                    Ok(())
                } else {
                    Err(CorruptionError::ImmutableCollision.into())
                }
            }
            PutIfAbsent::Ambiguous => self.resolve_ambiguous_immutable(key, bytes, kind).await,
        }
    }

    async fn resolve_ambiguous_immutable(
        &self,
        key: ObjectKey,
        desired: Vec<u8>,
        kind: MissingObjectKind,
    ) -> Result<(), StorageError<S::Error>> {
        let max_bytes = match kind {
            MissingObjectKind::Manifest => self.limits.max_manifest_bytes(),
            MissingObjectKind::Head | MissingObjectKind::Payload => self.limits.max_object_bytes(),
        };
        match self.target.get(key.clone(), max_bytes).await {
            Ok(Some(object)) if object.bytes() == desired => Ok(()),
            Ok(Some(_)) => Err(CorruptionError::ImmutableCollision.into()),
            Ok(None) | Err(_) => Err(StorageError::Ambiguous(AmbiguityError {
                operation: AmbiguousOperation::ImmutableCreation,
                key,
            })),
        }
    }

    async fn get_required(
        &self,
        key: ObjectKey,
        kind: MissingObjectKind,
    ) -> Result<Vec<u8>, StorageError<S::Error>> {
        let max_bytes = match kind {
            MissingObjectKind::Manifest => self.limits.max_manifest_bytes(),
            MissingObjectKind::Head | MissingObjectKind::Payload => self.limits.max_object_bytes(),
        };
        let object = self
            .target
            .get(key.clone(), max_bytes)
            .await
            .map_err(|source| {
                StorageError::Target(TargetError {
                    operation: TargetOperation::Get,
                    key: key.clone(),
                    source,
                })
            })?;
        let object = object.ok_or_else(|| StorageError::Missing { kind, key })?;
        if object.bytes().len() > self.limits.max_object_bytes() {
            return Err(LimitError::new(
                LimitKind::Object,
                u64::try_from(object.bytes().len()).unwrap_or(u64::MAX),
                u64::try_from(self.limits.max_object_bytes()).unwrap_or(u64::MAX),
            )
            .into());
        }
        Ok(object.into_bytes())
    }
}

fn check_input_limit<E>(
    kind: LimitKind,
    actual: usize,
    limit: usize,
) -> Result<(), StorageError<E>> {
    if actual > limit {
        Err(LimitError::new(
            kind,
            u64::try_from(actual).unwrap_or(u64::MAX),
            u64::try_from(limit).unwrap_or(u64::MAX),
        )
        .into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseContentIdentity, OperationFingerprint, PreparationIdentity, StorageMethod,
        TargetOperation,
        testing::{MemoryTarget, TracePhase, block_on},
    };

    fn repository(target: MemoryTarget) -> ContentRepository<MemoryTarget> {
        ContentRepository::new(
            target,
            "private",
            CreationDefaults::new(StorageMethod::Raw),
            StorageLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn payload_precedes_manifest_and_reference_reopens() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let file_id = FileId::from_u128(1);
        let mutation_id = MutationId::from_u128(2);
        let identity =
            PreparationIdentity::for_create(mutation_id, StorageMethod::Raw, b"abc").unwrap();
        let prepared = block_on(async {
            let blob = repository
                .store_raw_payload(file_id, identity, 0, b"abc")
                .await
                .unwrap();
            let manifest = FileManifest::raw(file_id, 1, 3, Some(blob));
            repository
                .prepare_manifest(&manifest, identity, 0, true)
                .await
                .unwrap()
        });
        block_on(repository.validate_content(prepared.content())).unwrap();

        let puts = target
            .trace()
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.operation == TargetOperation::PutIfAbsent && event.phase == TracePhase::Before
            })
            .map(|event| event.key)
            .collect::<Vec<_>>();
        assert_eq!(puts.len(), 2);
        assert!(puts[0].as_str().ends_with("/raw"));
        assert!(puts[1].as_str().contains("/manifests/"));
    }

    #[test]
    fn first_positioned_write_is_generation_one_and_does_not_publish_a_head() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let file_id = FileId::from_u128(10);
        let mutation_id = MutationId::from_u128(11);
        let prepared =
            block_on(repository.prepare_write_from_new(file_id, mutation_id, 3, 2, b"x")).unwrap();
        assert_eq!(prepared.content().generation(), 1);
        assert_eq!(prepared.identity().base(), BaseContentIdentity::NEW_FILE);
        assert_eq!(
            prepared.identity(),
            PreparationIdentity::for_write_from_new(mutation_id, StorageMethod::Raw, 2, b"x")
                .unwrap()
        );
        assert_eq!(prepared.attempt(), 3);
        assert!(prepared.content_changed());
        assert_eq!(
            block_on(repository.read(prepared.content(), 0, 3)).unwrap(),
            b"\0\0x"
        );
        assert!(target.trace().unwrap().iter().all(|event| {
            !event.key.as_str().contains("/heads/")
                && event.operation != TargetOperation::CompareExchange
        }));
    }

    #[test]
    fn first_write_raw_bound_and_block_sparse_gap_are_enforced_before_upload() {
        let raw_target = MemoryTarget::new();
        let raw_limits = StorageLimits::new(crate::StorageLimitValues {
            max_raw_file_bytes: 8,
            ..crate::StorageLimitValues::default()
        })
        .unwrap();
        let raw = ContentRepository::new(
            raw_target.clone(),
            "raw-bound",
            CreationDefaults::new(StorageMethod::Raw),
            raw_limits,
        )
        .unwrap();
        assert!(matches!(
            block_on(raw.prepare_write_from_new(
                FileId::from_u128(20),
                MutationId::from_u128(21),
                0,
                8,
                b"x",
            )),
            Err(StorageError::Limit(LimitError {
                kind: LimitKind::RawFile,
                ..
            }))
        ));
        assert_eq!(raw_target.object_count().unwrap(), 0);

        let block_target = MemoryTarget::new();
        let block = ContentRepository::new(
            block_target.clone(),
            "block-sparse",
            CreationDefaults::new(StorageMethod::BlockSplit),
            StorageLimits::default(),
        )
        .unwrap();
        let offset = u64::from(crate::BLOCK_SIZE_V1) * 10 + 5;
        let prepared = block_on(block.prepare_write_from_new(
            FileId::from_u128(22),
            MutationId::from_u128(23),
            0,
            offset,
            b"x",
        ))
        .unwrap();
        let manifest = block_on(block.load_manifest(prepared.content())).unwrap();
        let ManifestLayout::BlockSplit { blocks, .. } = manifest.layout() else {
            panic!("expected block-split manifest");
        };
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].index(), 10);
        assert_eq!(block_target.object_count().unwrap(), 2);
        assert_eq!(
            block_on(block.read(prepared.content(), offset - 2, 3)).unwrap(),
            b"\0\0x"
        );

        let zero_target = MemoryTarget::new();
        let zero_block = ContentRepository::new(
            zero_target.clone(),
            "block-zero",
            CreationDefaults::new(StorageMethod::BlockSplit),
            StorageLimits::default(),
        )
        .unwrap();
        let zero = block_on(zero_block.prepare_write_from_new(
            FileId::from_u128(24),
            MutationId::from_u128(25),
            0,
            offset,
            &[0; 4],
        ))
        .unwrap();
        let manifest = block_on(zero_block.load_manifest(zero.content())).unwrap();
        let ManifestLayout::BlockSplit { blocks, .. } = manifest.layout() else {
            panic!("expected block-split manifest");
        };
        assert!(blocks.is_empty());
        assert_eq!(zero_target.object_count().unwrap(), 1);
    }

    #[test]
    fn initial_truncate_builds_canonical_generation_one_zero_content() {
        for (ordinal, method) in [StorageMethod::Raw, StorageMethod::BlockSplit]
            .into_iter()
            .enumerate()
        {
            let target = MemoryTarget::new();
            let repository = ContentRepository::new(
                target.clone(),
                format!("truncate-{ordinal}"),
                CreationDefaults::new(method),
                StorageLimits::default(),
            )
            .unwrap();
            let file_id = FileId::from_u128(30 + ordinal as u128);
            let mutation_id = MutationId::from_u128(40 + ordinal as u128);
            let empty =
                block_on(repository.prepare_truncate_from_new(file_id, mutation_id, 0, 0)).unwrap();
            assert_eq!(empty.content().generation(), 1);
            assert_eq!(empty.content().logical_size(), 0);
            assert_eq!(empty.identity().base(), BaseContentIdentity::NEW_FILE);
            assert_eq!(
                empty.identity(),
                PreparationIdentity::for_truncate_from_new(mutation_id, method, 0)
            );
            assert_eq!(
                block_on(repository.read(empty.content(), 0, 1)).unwrap(),
                b""
            );

            let extended = block_on(repository.prepare_truncate_from_new(
                FileId::from_u128(50 + ordinal as u128),
                MutationId::from_u128(60 + ordinal as u128),
                1,
                9,
            ))
            .unwrap();
            assert_eq!(extended.content().generation(), 1);
            assert_eq!(extended.content().logical_size(), 9);
            assert_eq!(
                block_on(repository.read(extended.content(), 0, 9)).unwrap(),
                [0; 9]
            );
            let manifest = block_on(repository.load_manifest(extended.content())).unwrap();
            if let ManifestLayout::BlockSplit { blocks, .. } = manifest.layout() {
                assert!(blocks.is_empty());
            }
        }
    }

    #[test]
    fn existing_immutable_key_is_verified_exactly() {
        let target = MemoryTarget::new();
        let repository = repository(target);
        let file_id = FileId::from_u128(1);
        let mutation_id = MutationId::from_u128(2);
        let identity = PreparationIdentity::new(
            mutation_id,
            BaseContentIdentity::NEW_FILE,
            OperationFingerprint::new([7; 32]),
        );
        block_on(repository.store_raw_payload(file_id, identity, 0, b"one")).unwrap();
        let error =
            block_on(repository.store_raw_payload(file_id, identity, 0, b"two")).unwrap_err();
        assert!(matches!(
            error,
            StorageError::Corruption(CorruptionError::ImmutableCollision)
        ));
    }

    #[test]
    fn response_lost_immutable_put_is_resolved_by_exact_readback() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let file_id = FileId::from_u128(1);
        let mutation_id = MutationId::from_u128(2);
        let identity =
            PreparationIdentity::for_create(mutation_id, StorageMethod::Raw, b"abc").unwrap();
        let payload_key = repository.keys().raw_payload(file_id, identity, 0);
        target
            .inject_failure_for(
                TargetOperation::PutIfAbsent,
                crate::testing::FailureTiming::After,
                payload_key,
            )
            .unwrap();

        let prepared = block_on(repository.prepare_create(file_id, mutation_id, 0, b"abc"))
            .expect("exact readback proves the immutable dependency");
        block_on(repository.validate_content(prepared.content())).unwrap();
        assert_eq!(target.object_count().unwrap(), 2);
    }

    #[test]
    fn ambiguous_immutable_readback_detects_collision() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let file_id = FileId::from_u128(1);
        let mutation_id = MutationId::from_u128(2);
        let identity = PreparationIdentity::new(
            mutation_id,
            BaseContentIdentity::NEW_FILE,
            OperationFingerprint::new([7; 32]),
        );
        block_on(repository.store_raw_payload(file_id, identity, 0, b"one")).unwrap();
        let payload_key = repository.keys().raw_payload(file_id, identity, 0);
        target
            .inject_failure_for(
                TargetOperation::PutIfAbsent,
                crate::testing::FailureTiming::After,
                payload_key,
            )
            .unwrap();

        assert!(matches!(
            block_on(repository.store_raw_payload(file_id, identity, 0, b"two")),
            Err(StorageError::Corruption(
                CorruptionError::ImmutableCollision
            ))
        ));
    }

    #[test]
    fn ambiguous_immutable_absence_remains_typed_and_stops_dependencies() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let file_id = FileId::from_u128(1);
        let mutation_id = MutationId::from_u128(2);
        let identity =
            PreparationIdentity::for_create(mutation_id, StorageMethod::Raw, b"abc").unwrap();
        let payload_key = repository.keys().raw_payload(file_id, identity, 0);
        target
            .inject_ambiguous_put_without_commit_for(payload_key.clone())
            .unwrap();

        assert!(matches!(
            block_on(repository.prepare_create(file_id, mutation_id, 0, b"abc")),
            Err(StorageError::Ambiguous(AmbiguityError {
                operation: AmbiguousOperation::ImmutableCreation,
                key,
            })) if key == payload_key
        ));
        assert_eq!(target.object_count().unwrap(), 0);
    }

    #[test]
    fn failed_immutable_readback_leaves_only_unreachable_data_and_retry_recovers() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone());
        let file_id = FileId::from_u128(1);
        let mutation_id = MutationId::from_u128(2);
        let identity =
            PreparationIdentity::for_create(mutation_id, StorageMethod::Raw, b"abc").unwrap();
        let payload_key = repository.keys().raw_payload(file_id, identity, 0);
        target
            .inject_failure_for(
                TargetOperation::PutIfAbsent,
                crate::testing::FailureTiming::After,
                payload_key.clone(),
            )
            .unwrap();
        target
            .inject_failure_for(
                TargetOperation::Get,
                crate::testing::FailureTiming::Before,
                payload_key.clone(),
            )
            .unwrap();

        assert!(matches!(
            block_on(repository.prepare_create(file_id, mutation_id, 0, b"abc")),
            Err(StorageError::Ambiguous(AmbiguityError {
                operation: AmbiguousOperation::ImmutableCreation,
                key,
            })) if key == payload_key
        ));
        assert_eq!(target.object_count().unwrap(), 1);

        let prepared = block_on(repository.prepare_create(file_id, mutation_id, 0, b"abc"))
            .expect("retry verifies the late immutable object and finishes preparation");
        block_on(repository.validate_content(prepared.content())).unwrap();
        assert_eq!(target.object_count().unwrap(), 2);
    }

    #[test]
    fn content_and_publication_cannot_cross_repository_prefixes() {
        let target = MemoryTarget::new();
        let repository_a = ContentRepository::new(
            target.clone(),
            "prefix-a",
            CreationDefaults::new(StorageMethod::Raw),
            StorageLimits::default(),
        )
        .unwrap();
        let repository_b = ContentRepository::new(
            target.clone(),
            "prefix-b",
            CreationDefaults::new(StorageMethod::Raw),
            StorageLimits::default(),
        )
        .unwrap();
        let file_id = FileId::from_u128(1);
        let mutation_id = MutationId::from_u128(2);
        let prepared =
            block_on(repository_a.prepare_create(file_id, mutation_id, 0, b"abc")).unwrap();

        target.clear_trace().unwrap();
        assert!(matches!(
            block_on(repository_b.read(prepared.content(), 0, 3)),
            Err(StorageError::Corruption(CorruptionError::ForeignKey))
        ));
        assert!(target.trace().unwrap().is_empty());
        assert!(matches!(
            block_on(
                repository_b
                    .publisher()
                    .create(file_id, mutation_id, &prepared)
            ),
            Err(StorageError::Preparation(
                crate::PreparationError::KeyMismatch
            ))
        ));
    }
}
