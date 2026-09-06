//! Focused orchestration for authoritative per-file content contexts.

use core::fmt;

use w9pt_fs_state::{
    ContentMetadataRecord, FilesystemId, FilesystemStateStore, InodeId, InodeRecord, ReadBatch,
    ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision,
    RewrapContentMetadata, StateChange, StateLimits, StateRecord,
};
use w9pt_fs_storage::{
    CandidateKeyError, FileContextScope, FileCryptoContext, FileStoragePolicy, MasterKey,
    SecureEntropy, generate_content_metadata, open_committed_context, rewrap_content_metadata,
};

/// Builds the matching inode/context inserts for a caller-assembled create transaction.
pub fn content_context_create_changes(
    filesystem_id: FilesystemId,
    inode: InodeRecord,
    metadata: ContentMetadataRecord,
) -> Result<Vec<StateChange>, ContentContextOrchestrationError<core::convert::Infallible>> {
    if inode.content_file_id() != Some(metadata.content_file_id())
        || inode.content_context_id() != Some(metadata.context_id())
        || inode.inode_id() != metadata.owner_inode_id()
        || inode.content().is_some()
    {
        return Err(ContentContextOrchestrationError::Binding);
    }
    Ok(vec![
        StateChange::Insert {
            key: RecordKey::Inode(filesystem_id, inode.inode_id()),
            record: StateRecord::Inode(inode),
        },
        StateChange::Insert {
            key: RecordKey::ContentMetadata(filesystem_id, metadata.content_file_id()),
            record: StateRecord::ContentMetadata(metadata),
        },
    ])
}

/// Generates opaque candidate metadata for inclusion in the same create transaction.
#[allow(clippy::too_many_arguments)]
pub fn generate_content_context<E: SecureEntropy>(
    filesystem_id: FilesystemId,
    inode_id: InodeId,
    file_id: w9pt_fs_storage::FileId,
    context_id: w9pt_fs_storage::ContentContextId,
    policy: FileStoragePolicy,
    master: Option<&MasterKey>,
    entropy: &mut E,
    revision: RecordRevision,
    limits: StateLimits,
) -> Result<ContentMetadataRecord, ContentContextOrchestrationError<E::Error>> {
    let scope = FileContextScope::new(
        *filesystem_id.as_bytes(),
        *inode_id.as_bytes(),
        file_id,
        context_id,
    );
    let candidate = generate_content_metadata(scope, policy, master, entropy)
        .map_err(ContentContextOrchestrationError::Candidate)?;
    ContentMetadataRecord::new(
        inode_id,
        file_id,
        context_id,
        candidate.policy_format(),
        candidate.policy_bytes().to_vec(),
        candidate.key_commitment().copied(),
        candidate.wrapped_key_bytes().map(<[u8]>::to_vec),
        revision,
        limits,
    )
    .map_err(|_| ContentContextOrchestrationError::Binding)
}

/// Reads an inode and its committed winning context from one state snapshot and unwraps it.
pub async fn load_committed_content_context<S: FilesystemStateStore>(
    store: &S,
    filesystem_id: FilesystemId,
    inode_id: InodeId,
    master: Option<&MasterKey>,
) -> Result<
    (InodeRecord, ContentMetadataRecord, FileCryptoContext),
    ContentContextOrchestrationError<S::Error>,
> {
    let limits = store.contract().limits();
    let batch = ReadBatch::new(
        filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::InodeWithContentMetadata(inode_id)],
        limits,
    )
    .map_err(|_| ContentContextOrchestrationError::Binding)?;
    let outcome = store
        .read(batch)
        .await
        .map_err(ContentContextOrchestrationError::State)?;
    let ReadOutcome::Snapshot(snapshot) = outcome else {
        return Err(ContentContextOrchestrationError::Unavailable);
    };
    let ReadResult::InodeWithContentMetadata {
        inode: Some(inode),
        metadata: Some(metadata),
        ..
    } = &snapshot.results()[0]
    else {
        return Err(ContentContextOrchestrationError::Missing);
    };
    let scope = FileContextScope::new(
        *filesystem_id.as_bytes(),
        *inode.inode_id().as_bytes(),
        metadata.content_file_id(),
        metadata.context_id(),
    );
    let context = open_committed_context(
        scope,
        metadata.policy_format(),
        metadata.policy_bytes(),
        metadata.key_commitment().copied(),
        metadata.wrapped_key_bytes(),
        metadata.revision().get(),
        master,
    )
    .map_err(ContentContextOrchestrationError::Representation)?;
    Ok((inode.as_ref().clone(), metadata.as_ref().clone(), context))
}

/// Creates a dedicated state change that rewraps the same DEK under a new master.
pub fn rewrap_content_context<E>(
    filesystem_id: FilesystemId,
    metadata: &ContentMetadataRecord,
    old_master: &MasterKey,
    new_master: &MasterKey,
) -> Result<(RecordKey, StateChange), ContentContextOrchestrationError<E>> {
    let commitment = metadata
        .key_commitment()
        .copied()
        .ok_or(ContentContextOrchestrationError::Binding)?;
    let wrapped = metadata
        .wrapped_key_bytes()
        .ok_or(ContentContextOrchestrationError::Binding)?;
    let scope = FileContextScope::new(
        *filesystem_id.as_bytes(),
        *metadata.owner_inode_id().as_bytes(),
        metadata.content_file_id(),
        metadata.context_id(),
    );
    let replacement = rewrap_content_metadata(
        scope,
        metadata.policy_format(),
        metadata.policy_bytes(),
        commitment,
        wrapped,
        old_master,
        new_master,
    )
    .map_err(ContentContextOrchestrationError::Representation)?;
    let key = RecordKey::ContentMetadata(filesystem_id, metadata.content_file_id());
    Ok((
        key.clone(),
        StateChange::RewrapContentMetadata(RewrapContentMetadata {
            content_file_id: metadata.content_file_id(),
            expected_context_id: metadata.context_id(),
            expected_revision: metadata.revision(),
            wrapped_key_bytes: replacement,
        }),
    ))
}

/// Context generation, state lookup, or cryptographic orchestration failure.
#[derive(Debug)]
pub enum ContentContextOrchestrationError<E> {
    /// Candidate entropy/wrapping failed before commit.
    Candidate(CandidateKeyError<E>),
    /// Authoritative state adapter failed.
    State(E),
    /// State returned a non-snapshot semantic outcome.
    Unavailable,
    /// The selected inode/context was absent.
    Missing,
    /// Inode/context identities or shapes disagree.
    Binding,
    /// Authenticated unwrap/rewrap failed.
    Representation(w9pt_fs_storage::RepresentationError),
}

impl<E: fmt::Display> fmt::Display for ContentContextOrchestrationError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Candidate(error) => error.fmt(formatter),
            Self::State(error) => write!(formatter, "state adapter failed: {error}"),
            Self::Unavailable => formatter.write_str("authoritative context snapshot unavailable"),
            Self::Missing => formatter.write_str("authoritative content context missing"),
            Self::Binding => formatter.write_str("content context binding mismatch"),
            Self::Representation(error) => error.fmt(formatter),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ContentContextOrchestrationError<E> {}
