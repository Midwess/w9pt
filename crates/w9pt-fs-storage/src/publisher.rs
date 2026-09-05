//! Conditional object-head publication and bounded conflict rebasing.

use core::future::Future;

use crate::{
    AmbiguityError, CompareExchange, ConflictError, ContentRef, ContentRepository, CorruptionError,
    Digest, FileId, FormatError, MissingObjectKind, MutationId, ObjectVersion,
    OperationFingerprint, PreparationError, PreparedContent, StorageError, TargetError,
    TargetObject, TargetOperation, TargetStore,
    format::{FileHead, decode_head, decode_manifest, encode_head},
};

/// One published content version and the opaque target revision guarding it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedContent {
    content: ContentRef,
    mutation_id: MutationId,
    fingerprint: OperationFingerprint,
    attempt: u32,
    version: ObjectVersion,
}

impl PublishedContent {
    /// Returns the immutable content reference selected by the head.
    pub const fn content(&self) -> &ContentRef {
        &self.content
    }

    /// Returns the last mutation identity recorded in the head.
    pub const fn mutation_id(&self) -> MutationId {
        self.mutation_id
    }

    /// Returns the operation fingerprint recorded by the canonical manifest key.
    pub const fn operation_fingerprint(&self) -> OperationFingerprint {
        self.fingerprint
    }

    /// Returns the immutable-key attempt selected by the published head.
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Returns the opaque target revision required for replacement.
    pub const fn version(&self) -> &ObjectVersion {
        &self.version
    }
}

/// Definitive conditional-publication outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Publication {
    /// The desired head was published, including success resolved by readback.
    Published(PublishedContent),
    /// A different current revision won; no bytes were overwritten by this CAS.
    Conflict,
}

/// Object-backed conditional publisher for standalone content-store use.
#[derive(Clone, Copy, Debug)]
pub struct ObjectHeadPublisher<'a, S> {
    repository: &'a ContentRepository<S>,
}

impl<'a, S: TargetStore> ObjectHeadPublisher<'a, S> {
    pub(crate) const fn new(repository: &'a ContentRepository<S>) -> Self {
        Self { repository }
    }

    /// Loads the current head and reconstructs its content from target-only state.
    pub async fn load(
        &self,
        file_id: FileId,
    ) -> Result<Option<PublishedContent>, StorageError<S::Error>> {
        let key = self.repository.keys().head(file_id);
        let object = self
            .repository
            .target()
            .get(key.clone(), self.repository.limits().max_object_bytes())
            .await
            .map_err(|source| target_error(TargetOperation::Get, key, source))?;
        match object {
            Some(object) => self.decode_published(file_id, object).await.map(Some),
            None => Ok(None),
        }
    }

    /// Creates the first file head if absent.
    pub async fn create(
        &self,
        file_id: FileId,
        mutation_id: MutationId,
        prepared: &PreparedContent,
    ) -> Result<Publication, StorageError<S::Error>> {
        let content = prepared.content();
        if content.file_id() != file_id {
            return Err(CorruptionError::IdentityMismatch { field: "file" }.into());
        }
        if content.generation() != 1 {
            return Err(FormatError::NonCanonical {
                field: "initial generation",
            }
            .into());
        }
        self.validate_preparation(mutation_id, prepared, None)?;
        self.publish_head(file_id, mutation_id, prepared, None)
            .await
    }

    /// Replaces a head only when the loaded opaque target revision still matches.
    pub async fn replace(
        &self,
        expected: &PublishedContent,
        mutation_id: MutationId,
        prepared: &PreparedContent,
    ) -> Result<Publication, StorageError<S::Error>> {
        let content = prepared.content();
        if content.file_id() != expected.content.file_id() {
            return Err(CorruptionError::IdentityMismatch { field: "file" }.into());
        }
        self.validate_preparation(mutation_id, prepared, Some(expected.content()))?;
        if expected.mutation_id == mutation_id {
            if expected.fingerprint != prepared.identity().fingerprint() {
                return Err(PreparationError::FingerprintMismatch.into());
            }
            return Ok(Publication::Published(expected.clone()));
        }
        let next_generation = expected.content.generation().checked_add(1).ok_or(
            FormatError::ArithmeticOverflow {
                field: "content generation",
            },
        )?;
        if content.generation() != next_generation {
            return Err(FormatError::NonCanonical {
                field: "replacement generation",
            }
            .into());
        }
        if !prepared.content_changed() {
            return Err(PreparationError::UnchangedPublication.into());
        }
        self.publish_head(
            content.file_id(),
            mutation_id,
            prepared,
            Some(expected.version.clone()),
        )
        .await
    }

    /// Reapplies a logical operation to the latest content after bounded CAS conflicts.
    ///
    /// The closure receives an owned repository clone, base reference, and attempt
    /// number. Attempt-specific immutable keys keep rebased preparations distinct.
    pub async fn mutate_rebased<F, Fut>(
        &self,
        file_id: FileId,
        mutation_id: MutationId,
        mut prepare: F,
    ) -> Result<PublishedContent, StorageError<S::Error>>
    where
        S: Clone,
        F: FnMut(ContentRepository<S>, ContentRef, u32) -> Fut,
        Fut: Future<Output = Result<PreparedContent, StorageError<S::Error>>>,
    {
        let mut current = self
            .load(file_id)
            .await?
            .ok_or_else(|| StorageError::Missing {
                kind: MissingObjectKind::Head,
                key: self.repository.keys().head(file_id),
            })?;
        let maximum = self.repository.limits().max_publish_retries();
        let mut conflicts = 0_u32;
        let mut fingerprint = None::<OperationFingerprint>;
        loop {
            let prepared =
                prepare(self.repository.clone(), current.content.clone(), conflicts).await?;
            self.validate_preparation(mutation_id, &prepared, Some(current.content()))?;
            if current.mutation_id == mutation_id {
                if current.fingerprint != prepared.identity().fingerprint() {
                    return Err(PreparationError::FingerprintMismatch.into());
                }
                return Ok(current);
            }
            match fingerprint {
                Some(expected) if expected != prepared.identity().fingerprint() => {
                    return Err(PreparationError::FingerprintMismatch.into());
                }
                None => fingerprint = Some(prepared.identity().fingerprint()),
                Some(_) => {}
            }
            if !prepared.content_changed() {
                return Ok(current);
            }
            match self.replace(&current, mutation_id, &prepared).await? {
                Publication::Published(published) => return Ok(published),
                Publication::Conflict => {
                    conflicts = conflicts.saturating_add(1);
                    if conflicts > maximum {
                        return Err(StorageError::Conflict(ConflictError { conflicts }));
                    }
                    current = self
                        .load(file_id)
                        .await?
                        .ok_or_else(|| StorageError::Missing {
                            kind: MissingObjectKind::Head,
                            key: self.repository.keys().head(file_id),
                        })?;
                }
            }
        }
    }

    async fn publish_head(
        &self,
        file_id: FileId,
        mutation_id: MutationId,
        prepared: &PreparedContent,
        expected: Option<ObjectVersion>,
    ) -> Result<Publication, StorageError<S::Error>> {
        let content = prepared.content();
        self.repository.validate_content(content).await?;
        let desired = FileHead::new(
            file_id,
            content.generation(),
            content.manifest_key().clone(),
            content.manifest_digest(),
            mutation_id,
        );
        let bytes = encode_head(&desired, self.repository.limits()).map_err(StorageError::from)?;
        let key = self.repository.keys().head(file_id);
        let outcome = self
            .repository
            .target()
            .compare_exchange(key.clone(), expected, bytes)
            .await
            .map_err(|source| {
                target_error(TargetOperation::CompareExchange, key.clone(), source)
            })?;
        match outcome {
            CompareExchange::Replaced { version } => Ok(Publication::Published(PublishedContent {
                content: content.clone(),
                mutation_id,
                fingerprint: prepared.identity().fingerprint(),
                attempt: prepared.attempt(),
                version,
            })),
            CompareExchange::Conflict { .. } => Ok(Publication::Conflict),
            CompareExchange::Ambiguous => self.resolve_ambiguous(file_id, &desired).await,
        }
    }

    fn validate_preparation(
        &self,
        mutation_id: MutationId,
        prepared: &PreparedContent,
        base: Option<&ContentRef>,
    ) -> Result<(), StorageError<S::Error>> {
        let identity = prepared.identity();
        if identity.mutation_id() != mutation_id {
            return Err(PreparationError::MutationMismatch.into());
        }
        let expected_base = base.map_or(crate::BaseContentIdentity::NEW_FILE, |content| {
            crate::BaseContentIdentity::from_content(content)
        });
        if identity.base() != expected_base {
            return Err(PreparationError::BaseMismatch.into());
        }
        if prepared.content_changed() {
            let expected_key = self.repository.keys().manifest(
                prepared.content().file_id(),
                identity,
                prepared.attempt(),
            );
            if prepared.content().manifest_key() != &expected_key {
                return Err(PreparationError::KeyMismatch.into());
            }
        }
        Ok(())
    }

    async fn resolve_ambiguous(
        &self,
        file_id: FileId,
        desired: &FileHead,
    ) -> Result<Publication, StorageError<S::Error>> {
        let key = self.repository.keys().head(file_id);
        let readback = self
            .repository
            .target()
            .get(key.clone(), self.repository.limits().max_object_bytes())
            .await;
        let Ok(Some(object)) = readback else {
            return Err(StorageError::Ambiguous(AmbiguityError {
                operation: crate::AmbiguousOperation::HeadPublication,
                key,
            }));
        };
        let version = object.version().clone();
        let Ok(head) = decode_head(object.bytes(), self.repository.limits()) else {
            return Err(StorageError::Ambiguous(AmbiguityError {
                operation: crate::AmbiguousOperation::HeadPublication,
                key,
            }));
        };
        if &head != desired {
            return Err(StorageError::Ambiguous(AmbiguityError {
                operation: crate::AmbiguousOperation::HeadPublication,
                key,
            }));
        }
        let (content, preparation) = self.load_content(&head).await.map_err(|_| {
            StorageError::Ambiguous(AmbiguityError {
                operation: crate::AmbiguousOperation::HeadPublication,
                key: self.repository.keys().head(file_id),
            })
        })?;
        Ok(Publication::Published(PublishedContent {
            content,
            mutation_id: desired.mutation_id(),
            fingerprint: preparation.identity.fingerprint(),
            attempt: preparation.attempt,
            version,
        }))
    }

    async fn decode_published(
        &self,
        file_id: FileId,
        object: TargetObject,
    ) -> Result<PublishedContent, StorageError<S::Error>> {
        let version = object.version().clone();
        let head =
            decode_head(object.bytes(), self.repository.limits()).map_err(StorageError::from)?;
        head.validate_file(file_id).map_err(StorageError::from)?;
        let (content, preparation) = self.load_content(&head).await?;
        Ok(PublishedContent {
            content,
            mutation_id: head.mutation_id(),
            fingerprint: preparation.identity.fingerprint(),
            attempt: preparation.attempt,
            version,
        })
    }

    async fn load_content(
        &self,
        head: &FileHead,
    ) -> Result<(ContentRef, crate::keys::PreparationKey), StorageError<S::Error>> {
        let key = head.manifest_key().clone();
        let preparation = self
            .repository
            .validate_manifest_key(head.file_id(), &key)?;
        preparation.validate_result_generation(head.generation())?;
        if preparation.identity.mutation_id() != head.mutation_id() {
            return Err(CorruptionError::IdentityMismatch { field: "mutation" }.into());
        }
        let object = self
            .repository
            .target()
            .get(key.clone(), self.repository.limits().max_manifest_bytes())
            .await
            .map_err(|source| target_error(TargetOperation::Get, key.clone(), source))?
            .ok_or_else(|| StorageError::Missing {
                kind: MissingObjectKind::Manifest,
                key: key.clone(),
            })?;
        if Digest::blake3(object.bytes()) != head.manifest_digest() {
            return Err(CorruptionError::DigestMismatch.into());
        }
        let manifest = decode_manifest(object.bytes(), self.repository.limits())
            .map_err(StorageError::from)?;
        if manifest.file_id() != head.file_id() {
            return Err(CorruptionError::IdentityMismatch { field: "file" }.into());
        }
        if manifest.generation() != head.generation() {
            return Err(CorruptionError::IdentityMismatch {
                field: "generation",
            }
            .into());
        }
        self.repository.validate_manifest_payloads(&manifest)?;
        Ok((
            manifest.content_ref(key, head.manifest_digest()),
            preparation,
        ))
    }
}

fn target_error<E>(
    operation: TargetOperation,
    key: crate::ObjectKey,
    source: E,
) -> StorageError<E> {
    StorageError::Target(TargetError {
        operation,
        key,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CreationDefaults, StorageLimitValues, StorageLimits, StorageMethod, TargetOperation,
        testing::{FailureTiming, MemoryTarget, block_on},
    };

    fn repository(
        target: MemoryTarget,
        max_publish_retries: u32,
    ) -> ContentRepository<MemoryTarget> {
        let values = StorageLimitValues {
            max_publish_retries,
            ..StorageLimitValues::default()
        };
        ContentRepository::new(
            target,
            "private",
            CreationDefaults::new(StorageMethod::Raw),
            StorageLimits::new(values).unwrap(),
        )
        .unwrap()
    }

    async fn prepare_empty(
        repository: &ContentRepository<MemoryTarget>,
        file_id: FileId,
        identity: crate::PreparationIdentity,
        generation: u64,
        attempt: u32,
    ) -> PreparedContent {
        repository
            .prepare_manifest(
                &crate::format::FileManifest::raw(file_id, generation, 0, None),
                identity,
                attempt,
                true,
            )
            .await
            .unwrap()
    }

    #[test]
    fn head_create_load_and_ambiguous_replace_round_trip() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone(), 2);
        let publisher = repository.publisher();
        let file_id = FileId::from_u128(1);
        let first_mutation = MutationId::from_u128(10);
        let second_mutation = MutationId::from_u128(11);
        let first_identity = crate::PreparationIdentity::new(
            first_mutation,
            crate::BaseContentIdentity::NEW_FILE,
            crate::OperationFingerprint::new([1; 32]),
        );
        let first = block_on(prepare_empty(&repository, file_id, first_identity, 1, 0));
        let Publication::Published(published) =
            block_on(publisher.create(file_id, first_mutation, &first)).unwrap()
        else {
            panic!("fresh head must publish");
        };
        assert_eq!(
            block_on(publisher.load(file_id)).unwrap(),
            Some(published.clone())
        );

        let second_identity = crate::PreparationIdentity::new(
            second_mutation,
            crate::BaseContentIdentity::from_content(published.content()),
            crate::OperationFingerprint::new([2; 32]),
        );
        let second = block_on(prepare_empty(&repository, file_id, second_identity, 2, 0));
        target
            .inject_failure(TargetOperation::CompareExchange, FailureTiming::After)
            .unwrap();
        let Publication::Published(resolved) =
            block_on(publisher.replace(&published, second_mutation, &second)).unwrap()
        else {
            panic!("readback must resolve committed CAS");
        };
        assert_eq!(resolved.content().generation(), 2);
        assert_eq!(resolved.mutation_id(), second_mutation);
    }

    #[test]
    fn repeated_revision_changes_exhaust_bounded_rebase() {
        let target = MemoryTarget::new();
        let repository = repository(target.clone(), 2);
        let publisher = repository.publisher();
        let file_id = FileId::from_u128(1);
        let initial_mutation = MutationId::from_u128(10);
        let mutation = MutationId::from_u128(11);
        let first_identity = crate::PreparationIdentity::new(
            initial_mutation,
            crate::BaseContentIdentity::NEW_FILE,
            crate::OperationFingerprint::new([1; 32]),
        );
        let first = block_on(prepare_empty(&repository, file_id, first_identity, 1, 0));
        block_on(publisher.create(file_id, initial_mutation, &first)).unwrap();
        let head_key = repository.keys().head(file_id);

        let result = block_on(publisher.mutate_rebased(
            file_id,
            mutation,
            move |repository, base, attempt| {
                let target = target.clone();
                let head_key = head_key.clone();
                async move {
                    let manifest = crate::format::FileManifest::raw(
                        base.file_id(),
                        base.generation() + 1,
                        0,
                        None,
                    );
                    let prepared = repository
                        .prepare_manifest(
                            &manifest,
                            crate::PreparationIdentity::for_truncate(mutation, &base, 0),
                            attempt,
                            true,
                        )
                        .await?;
                    let same_bytes = target.inspect(&head_key).unwrap().unwrap();
                    assert!(target.corrupt(&head_key, same_bytes).unwrap());
                    Ok(prepared)
                }
            },
        ));
        assert!(matches!(
            result,
            Err(StorageError::Conflict(ConflictError { conflicts: 3 }))
        ));
    }

    #[test]
    fn publisher_rejects_a_mutation_label_different_from_the_preparation() {
        let target = MemoryTarget::new();
        let repository = repository(target, 1);
        let file_id = FileId::from_u128(1);
        let prepared_mutation = MutationId::from_u128(10);
        let identity = crate::PreparationIdentity::new(
            prepared_mutation,
            crate::BaseContentIdentity::NEW_FILE,
            crate::OperationFingerprint::new([1; 32]),
        );
        let prepared = block_on(prepare_empty(&repository, file_id, identity, 1, 0));
        assert!(matches!(
            block_on(
                repository
                    .publisher()
                    .create(file_id, MutationId::from_u128(11), &prepared,)
            ),
            Err(StorageError::Preparation(
                PreparationError::MutationMismatch
            ))
        ));
        assert_eq!(
            block_on(repository.publisher().load(file_id)).unwrap(),
            None
        );
    }

    #[test]
    fn superseded_head_does_not_turn_ambiguous_commit_into_conflict() {
        let target = MemoryTarget::new();
        let repository = repository(target, 1);
        let publisher = repository.publisher();
        let file_id = FileId::from_u128(1);
        let initial_mutation = MutationId::from_u128(1);
        let first_mutation = MutationId::from_u128(2);
        let second_mutation = MutationId::from_u128(3);
        let initial =
            block_on(repository.prepare_create(file_id, initial_mutation, 0, b"")).unwrap();
        let Publication::Published(initial) =
            block_on(publisher.create(file_id, initial_mutation, &initial)).unwrap()
        else {
            panic!("fresh head must publish");
        };
        let first =
            block_on(repository.prepare_write(initial.content(), first_mutation, 0, 0, b"A"))
                .unwrap();
        let Publication::Published(first_published) =
            block_on(publisher.replace(&initial, first_mutation, &first)).unwrap()
        else {
            panic!("first replacement must publish");
        };
        let second = block_on(repository.prepare_write(
            first_published.content(),
            second_mutation,
            0,
            0,
            b"B",
        ))
        .unwrap();
        assert!(matches!(
            block_on(publisher.replace(&first_published, second_mutation, &second)).unwrap(),
            Publication::Published(_)
        ));

        let desired_first = FileHead::new(
            file_id,
            first.content().generation(),
            first.content().manifest_key().clone(),
            first.content().manifest_digest(),
            first_mutation,
        );
        assert!(matches!(
            block_on(publisher.resolve_ambiguous(file_id, &desired_first)),
            Err(StorageError::Ambiguous(_))
        ));
    }

    #[test]
    fn delayed_mutation_retry_requires_matching_fingerprint() {
        let target = MemoryTarget::new();
        let repository = repository(target, 1);
        let publisher = repository.publisher();
        let file_id = FileId::from_u128(1);
        let initial_mutation = MutationId::from_u128(1);
        let write_mutation = MutationId::from_u128(2);
        let initial =
            block_on(repository.prepare_create(file_id, initial_mutation, 0, b"seed")).unwrap();
        let Publication::Published(initial) =
            block_on(publisher.create(file_id, initial_mutation, &initial)).unwrap()
        else {
            panic!("fresh head must publish");
        };
        let write =
            block_on(repository.prepare_write(initial.content(), write_mutation, 0, 0, b"done"))
                .unwrap();
        let Publication::Published(written) =
            block_on(publisher.replace(&initial, write_mutation, &write)).unwrap()
        else {
            panic!("write must publish");
        };

        let matching = block_on(publisher.mutate_rebased(
            file_id,
            write_mutation,
            |repository, base, attempt| async move {
                repository
                    .prepare_write(&base, write_mutation, attempt, 0, b"done")
                    .await
            },
        ))
        .unwrap();
        assert_eq!(matching, written);

        let mismatching = block_on(publisher.mutate_rebased(
            file_id,
            write_mutation,
            |repository, base, attempt| async move {
                repository
                    .prepare_write(&base, write_mutation, attempt, 0, b"other")
                    .await
            },
        ));
        assert!(matches!(
            mismatching,
            Err(StorageError::Preparation(
                PreparationError::FingerprintMismatch
            ))
        ));
    }
}
