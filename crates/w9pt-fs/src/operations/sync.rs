//! Write-through data-only and full durability barriers.

use w9pt::{FilesystemError, LinuxErrno, filesystem::RequestContext};
use w9pt_fs_state::{
    ClientIncarnationId, FilesystemStateStore, InodeKind, ReadBatch, ReadConsistency, ReadOutcome,
    ReadQuery, ReadResult, StateRecord,
};
use w9pt_fs_storage::{
    ContentRepository, FileContextScope, StorageError, TargetStore, open_committed_context,
};

use crate::{EngineLimits, ExportGrant, ExportPolicy, ExportPolicyRequest, open_id_from_handle};

use super::read::DataReadError;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_fsync<S, T, P>(
    state: &S,
    content: &ContentRepository<T>,
    policy: &P,
    _limits: EngineLimits,
    context: RequestContext,
    client_incarnation: ClientIncarnationId,
    open: w9pt::filesystem::OpenHandle,
    data_only: bool,
) -> Result<(), DataReadError<S::Error, T::Error, P::Error>>
where
    S: FilesystemStateStore,
    T: TargetStore,
    P: ExportPolicy,
{
    let target = content.target().guarantees();
    if !target.durable_writes
        || !target.atomic_put_if_absent
        || !target.atomic_compare_exchange
        || !target.read_after_write
    {
        return Err(client(LinuxErrno::EOPNOTSUPP));
    }
    let contract = state.contract();
    if !data_only
        && (!contract.is_production_ready() || !contract.guarantees().durable_commit_acknowledgment)
    {
        return Err(client(LinuxErrno::EOPNOTSUPP));
    }
    let grant = policy
        .resolve(ExportPolicyRequest::new(context))
        .await
        .map_err(DataReadError::Policy)?;
    let open_id = open_id_from_handle(open);
    let first = snapshot::<S, T::Error, P::Error>(
        state,
        &grant,
        vec![ReadQuery::Filesystem, ReadQuery::Open(open_id)],
    )
    .await?;
    let open = point_open(&first.results()[1])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    if open.client_incarnation() != client_incarnation {
        return Err(client(LinuxErrno::EBADF));
    }
    let inode_id = open.inode_id();
    let kind = snapshot::<S, T::Error, P::Error>(
        state,
        &grant,
        vec![ReadQuery::Filesystem, ReadQuery::Inode(inode_id)],
    )
    .await?;
    let inode = point_inode(&kind.results()[1])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    if inode.kind() != InodeKind::RegularFile {
        return Err(client(if inode.kind() == InodeKind::Directory {
            LinuxErrno::EISDIR
        } else {
            LinuxErrno::EOPNOTSUPP
        }));
    }
    if inode.content().is_none() {
        return Ok(());
    }
    let selected = snapshot::<S, T::Error, P::Error>(
        state,
        &grant,
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Open(open_id),
            ReadQuery::InodeWithContentMetadata(inode_id),
        ],
    )
    .await?;
    let current_open =
        point_open(&selected.results()[1])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    if current_open.client_incarnation() != client_incarnation
        || current_open.inode_id() != inode_id
    {
        return Err(client(LinuxErrno::EBADF));
    }
    let ReadResult::InodeWithContentMetadata {
        inode: Some(inode),
        metadata: Some(metadata),
        ..
    } = &selected.results()[2]
    else {
        return Err(DataReadError::MalformedState);
    };
    let content_ref = inode
        .content()
        .ok_or(DataReadError::MalformedState)?
        .clone();
    let context = open_committed_context(
        FileContextScope::new(
            *grant.filesystem_id().as_bytes(),
            *inode_id.as_bytes(),
            metadata.content_file_id(),
            metadata.context_id(),
        ),
        metadata.policy_format(),
        metadata.policy_bytes(),
        metadata.key_commitment().copied(),
        metadata.wrapped_key_bytes(),
        metadata.revision().get(),
        None,
    )
    .map_err(|error| DataReadError::Target(StorageError::Representation(error)))?;
    content
        .sync_content_with_context(&content_ref, &context)
        .await
        .map_err(DataReadError::Target)
}

async fn snapshot<S, T, P>(
    state: &S,
    grant: &ExportGrant,
    queries: Vec<ReadQuery>,
) -> Result<w9pt_fs_state::StateSnapshot, DataReadError<S::Error, T, P>>
where
    S: FilesystemStateStore,
{
    let request = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::LatestLinearizable,
        queries,
        state.contract().limits(),
    )
    .map_err(DataReadError::MalformedRead)?;
    let ReadOutcome::Snapshot(snapshot) =
        state.read(request).await.map_err(DataReadError::State)?
    else {
        return Err(DataReadError::RevisionUnavailable);
    };
    let ReadResult::Point {
        record: Some(filesystem),
        ..
    } = &snapshot.results()[0]
    else {
        return Err(DataReadError::MalformedState);
    };
    let StateRecord::Filesystem(filesystem) = filesystem.as_ref() else {
        return Err(DataReadError::MalformedState);
    };
    if filesystem.filesystem_id() != grant.filesystem_id()
        || filesystem.root_inode_id() != grant.root_inode_id()
        || filesystem.policy_generation() != grant.policy_generation()
    {
        return Err(client(LinuxErrno::EAGAIN));
    }
    Ok(snapshot)
}

fn point_open<S, T, P>(
    result: &ReadResult,
) -> Result<Option<w9pt_fs_state::OpenRecord>, DataReadError<S, T, P>> {
    point(result, |record| match record {
        StateRecord::Open(value) => Some(value.clone()),
        _ => None,
    })
}

fn point_inode<S, T, P>(
    result: &ReadResult,
) -> Result<Option<w9pt_fs_state::InodeRecord>, DataReadError<S, T, P>> {
    point(result, |record| match record {
        StateRecord::Inode(value) => Some(value.clone()),
        _ => None,
    })
}

fn point<S, T, P, R>(
    result: &ReadResult,
    convert: impl FnOnce(&StateRecord) -> Option<R>,
) -> Result<Option<R>, DataReadError<S, T, P>> {
    let ReadResult::Point { record, .. } = result else {
        return Err(DataReadError::MalformedState);
    };
    match record.as_deref() {
        Some(record) => convert(record)
            .map(Some)
            .ok_or(DataReadError::MalformedState),
        None => Ok(None),
    }
}

const fn client<S, T, P>(errno: LinuxErrno) -> DataReadError<S, T, P> {
    DataReadError::Client(FilesystemError::new(errno))
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt_fs_storage::{StorageMethod, testing::block_on};

    use crate::testing::TestEnvironment;

    #[test]
    fn data_only_sync_validates_content_and_reference_state_rejects_full_sync() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let created = environment.create_file("sync", 100).await;
            environment
                .publish_new_content(created.object, b"durable", 101)
                .await;
            assert!(
                execute_fsync(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    environment.engine_limits,
                    environment.context(),
                    environment.client,
                    created.open,
                    true,
                )
                .await
                .is_ok()
            );
            assert!(matches!(
                execute_fsync(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    environment.engine_limits,
                    environment.context(),
                    environment.client,
                    created.open,
                    false,
                )
                .await,
                Err(DataReadError::Client(error)) if error.errno == LinuxErrno::EOPNOTSUPP
            ));
        });
    }
}
