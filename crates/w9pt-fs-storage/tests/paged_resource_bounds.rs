#![allow(missing_docs)]

use core::fmt;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use w9pt_fs_storage::{
    BLOCK_MAP_FANOUT, BLOCK_SIZE, CompareExchange, ContentRef, ContentRepository, CreationDefaults,
    Digest, FileCryptoContext, FileId, KeySpace, MutationId, ObjectKey, ObjectProvenance,
    ObjectRange, ObjectVersion, PreparationIdentity, PutIfAbsent, StorageLimitValues,
    StorageLimits, StorageMethod, TargetGuarantees, TargetObject, TargetStore, encode_object,
    format::{
        BlobRef, BlockMapPage, BranchEntry, FileManifest, LeafEntry, ObjectKind, PageRef,
        encode_block_map_page, encode_manifest, page_coverage,
    },
    testing::block_on,
};

const DENSE_BLOCKS: u64 = 131_073;

#[derive(Clone)]
struct DenseGenerator {
    keys: KeySpace,
    file_id: FileId,
    identity: PreparationIdentity,
    limits: StorageLimits,
    payload_digest: Digest,
    payload_stored_len: u64,
    context: Arc<FileCryptoContext>,
}

impl DenseGenerator {
    fn page(&self, level: u8, first: u64) -> Option<(PageRef, Vec<u8>)> {
        if first >= DENSE_BLOCKS {
            return None;
        }
        let page = if level == 0 {
            let count = (DENSE_BLOCKS - first).min(u64::from(BLOCK_MAP_FANOUT));
            BlockMapPage::leaf(
                self.file_id,
                first,
                (0..count)
                    .map(|slot| {
                        let index = first + slot;
                        LeafEntry::new(
                            u8::try_from(slot).unwrap(),
                            BlobRef::new(
                                self.keys
                                    .block_payload(self.file_id, self.identity, 0, index),
                                u64::from(BLOCK_SIZE),
                                self.payload_stored_len,
                                self.payload_digest,
                            ),
                        )
                    })
                    .collect(),
            )
        } else {
            let child_coverage = page_coverage(level - 1).unwrap();
            let entries = (0..BLOCK_MAP_FANOUT)
                .filter_map(|slot| {
                    let child_first = first + u64::from(slot) * child_coverage;
                    self.page(level - 1, child_first)
                        .map(|(child, _)| BranchEntry::new(u8::try_from(slot).unwrap(), child))
                })
                .collect::<Vec<_>>();
            BlockMapPage::branch(self.file_id, level, first, entries)
        };
        let canonical = encode_block_map_page(&page, self.limits).unwrap();
        let provenance = ObjectProvenance::MapPage {
            identity: self.identity,
            attempt: 0,
            level,
            first_block: first,
        };
        let key = self
            .keys
            .map_page(self.file_id, self.identity, 0, level, first);
        let encoded = encode_object(
            if level == 0 {
                ObjectKind::LeafMap
            } else {
                ObjectKind::BranchMap
            },
            &key,
            &canonical,
            provenance,
            &self.context,
            false,
            self.limits.max_map_page_bytes(),
        )
        .unwrap();
        let (count, highest) = page.summary().unwrap();
        let reference = PageRef::new(
            key,
            u64::try_from(encoded.len()).unwrap(),
            Digest::blake3(&encoded),
            level,
            first,
            count,
            highest,
        );
        Some((reference, encoded))
    }
}

#[derive(Clone)]
struct SyntheticDenseTarget {
    generator: DenseGenerator,
    manifest_key: ObjectKey,
    manifest: Arc<Vec<u8>>,
    canonical_payload: Arc<Vec<u8>>,
    work: Arc<Mutex<(u32, u32, u32)>>,
    overlays: Arc<Mutex<BTreeMap<ObjectKey, Vec<u8>>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SyntheticError {
    TooLarge,
    UnsupportedMutation,
    InvalidRange,
}

impl fmt::Display for SyntheticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "synthetic target error: {self:?}")
    }
}

impl std::error::Error for SyntheticError {}

impl TargetStore for SyntheticDenseTarget {
    type Error = SyntheticError;

    fn guarantees(&self) -> TargetGuarantees {
        TargetGuarantees::REQUIRED
    }

    async fn get(
        &self,
        key: ObjectKey,
        max_bytes: usize,
    ) -> Result<Option<TargetObject>, Self::Error> {
        let overlay = self.overlays.lock().unwrap().get(&key).cloned();
        let bytes = if overlay.is_some() {
            if key.as_str().contains("/maps/") {
                self.work.lock().unwrap().0 += 1;
            }
            if key.as_str().contains("/blocks/") {
                self.work.lock().unwrap().1 += 1;
            }
            overlay
        } else if key == self.manifest_key {
            Some(self.manifest.as_ref().clone())
        } else if key.as_str().contains("/maps/") {
            let mut parts = key.as_str().rsplit('/');
            let first = u64::from_str_radix(parts.next().ok_or(SyntheticError::InvalidRange)?, 16)
                .map_err(|_| SyntheticError::InvalidRange)?;
            let level = u8::from_str_radix(parts.next().ok_or(SyntheticError::InvalidRange)?, 16)
                .map_err(|_| SyntheticError::InvalidRange)?;
            self.work.lock().unwrap().0 += 1;
            self.generator.page(level, first).map(|(_, bytes)| bytes)
        } else if key.as_str().contains("/blocks/") {
            self.work.lock().unwrap().1 += 1;
            let index = u64::from_str_radix(
                key.as_str()
                    .rsplit('/')
                    .next()
                    .ok_or(SyntheticError::InvalidRange)?,
                16,
            )
            .map_err(|_| SyntheticError::InvalidRange)?;
            Some(
                encode_object(
                    ObjectKind::Payload,
                    &key,
                    &self.canonical_payload,
                    ObjectProvenance::BlockPayload {
                        identity: self.generator.identity,
                        attempt: 0,
                        block_index: index,
                    },
                    &self.generator.context,
                    true,
                    self.generator.limits.max_object_bytes(),
                )
                .map_err(|_| SyntheticError::InvalidRange)?,
            )
        } else {
            None
        };
        if bytes.as_ref().is_some_and(|bytes| bytes.len() > max_bytes) {
            return Err(SyntheticError::TooLarge);
        }
        Ok(bytes.map(|bytes| TargetObject::new(bytes, ObjectVersion::new(vec![1]))))
    }

    async fn get_range(
        &self,
        key: ObjectKey,
        range: ObjectRange,
    ) -> Result<Option<Vec<u8>>, Self::Error> {
        let Some(object) = self.get(key, usize::MAX).await? else {
            return Ok(None);
        };
        let start = usize::try_from(range.start()).map_err(|_| SyntheticError::InvalidRange)?;
        let end = usize::try_from(range.end()).map_err(|_| SyntheticError::InvalidRange)?;
        Ok(object.bytes().get(start..end).map(<[u8]>::to_vec))
    }

    async fn put_if_absent(
        &self,
        key: ObjectKey,
        bytes: Vec<u8>,
    ) -> Result<PutIfAbsent, Self::Error> {
        let mut overlays = self.overlays.lock().unwrap();
        if overlays.contains_key(&key) {
            return Ok(PutIfAbsent::AlreadyExists {
                version: ObjectVersion::new(vec![1]),
            });
        }
        if key.as_str().contains("/maps/") {
            self.work.lock().unwrap().2 += 1;
        }
        overlays.insert(key, bytes);
        Ok(PutIfAbsent::Created {
            version: ObjectVersion::new(vec![1]),
        })
    }

    async fn compare_exchange(
        &self,
        _key: ObjectKey,
        _expected: Option<ObjectVersion>,
        _bytes: Vec<u8>,
    ) -> Result<CompareExchange, Self::Error> {
        Err(SyntheticError::UnsupportedMutation)
    }
}

#[test]
fn one_block_read_over_dense_map_beyond_flat_limit_keeps_depth_bounded() {
    let default_values = StorageLimitValues::default();
    let limits = StorageLimits::new(StorageLimitValues {
        max_map_working_bytes: 4 * 1024 * 1024,
        ..default_values
    })
    .unwrap();
    let keys = KeySpace::new("synthetic", limits).unwrap();
    let file_id = FileId::from_u128(1);
    let mutation = MutationId::from_u128(2);
    let identity = PreparationIdentity::for_write_from_new(
        mutation,
        StorageMethod::BlockSplit,
        0,
        b"synthetic-dense-map",
    )
    .unwrap();
    let canonical_block = vec![7; BLOCK_SIZE as usize];
    let context = Arc::new(FileCryptoContext::plain_for_file(
        file_id,
        StorageMethod::BlockSplit,
    ));
    let sample_payload_key = keys.block_payload(file_id, identity, 0, 0);
    let sample_payload = encode_object(
        ObjectKind::Payload,
        &sample_payload_key,
        &canonical_block,
        ObjectProvenance::BlockPayload {
            identity,
            attempt: 0,
            block_index: 0,
        },
        &context,
        true,
        limits.max_object_bytes(),
    )
    .unwrap();
    let generator = DenseGenerator {
        keys: keys.clone(),
        file_id,
        identity,
        limits,
        payload_digest: Digest::blake3(&canonical_block),
        payload_stored_len: u64::try_from(sample_payload.len()).unwrap(),
        context: context.clone(),
    };
    let (root, _) = generator.page(2, 0).unwrap();
    assert_eq!(root.materialized_block_count(), DENSE_BLOCKS);
    let logical_size = DENSE_BLOCKS * u64::from(BLOCK_SIZE);
    let manifest = FileManifest::block_split(file_id, 1, logical_size, Some(root));
    let canonical_manifest = encode_manifest(&manifest, limits).unwrap();
    let manifest_key = keys.manifest(file_id, identity, 0);
    let encoded_manifest = encode_object(
        ObjectKind::Manifest,
        &manifest_key,
        &canonical_manifest,
        ObjectProvenance::Manifest {
            identity,
            attempt: 0,
            generation: 1,
        },
        &context,
        false,
        limits.max_manifest_bytes(),
    )
    .unwrap();
    let content = ContentRef::from_persisted(
        file_id,
        1,
        logical_size,
        manifest_key.clone(),
        Digest::blake3(&encoded_manifest),
        StorageMethod::BlockSplit,
    )
    .unwrap();
    let work = Arc::new(Mutex::new((0, 0, 0)));
    let target = SyntheticDenseTarget {
        generator,
        manifest_key,
        manifest: Arc::new(encoded_manifest),
        canonical_payload: Arc::new(canonical_block),
        work: work.clone(),
        overlays: Arc::new(Mutex::new(BTreeMap::new())),
    };
    let repository = ContentRepository::new(
        target,
        "synthetic",
        CreationDefaults::new(StorageMethod::BlockSplit),
        limits,
    )
    .unwrap();
    let index = 130_000;
    assert_eq!(
        block_on(repository.read(&content, index * u64::from(BLOCK_SIZE), 1)).unwrap(),
        [7]
    );
    assert_eq!(*work.lock().unwrap(), (3, 1, 0));

    *work.lock().unwrap() = (0, 0, 0);
    let changed = block_on(repository.prepare_write(
        &content,
        MutationId::from_u128(3),
        0,
        index * u64::from(BLOCK_SIZE),
        b"\x08",
    ))
    .unwrap();
    let write_work = *work.lock().unwrap();
    assert!(
        write_work.0 <= 20,
        "write map reads exceeded bounded two-pass paths: {write_work:?}"
    );
    assert_eq!(write_work.2, 3);
    assert_eq!(
        block_on(repository.read(changed.content(), index * u64::from(BLOCK_SIZE), 1,)).unwrap(),
        [8]
    );

    *work.lock().unwrap() = (0, 0, 0);
    let shrunk = block_on(repository.prepare_truncate(
        &content,
        MutationId::from_u128(4),
        0,
        u64::from(BLOCK_SIZE),
    ))
    .unwrap();
    let prune_work = *work.lock().unwrap();
    assert!(
        prune_work.0 <= 16,
        "suffix pruning traversed unrelated dense pages: {prune_work:?}"
    );
    let shrunk_manifest = block_on(repository.load_manifest(shrunk.content())).unwrap();
    let w9pt_fs_storage::format::ManifestLayout::BlockSplit {
        root: Some(root), ..
    } = shrunk_manifest.layout()
    else {
        panic!("expected retained one-block root");
    };
    assert_eq!(root.level(), 0);
    assert_eq!(root.materialized_block_count(), 1);
}
