//! Immutable positioned regular-file reads with explicit no-atime semantics.

use w9pt::{
    FilesystemError, LinuxErrno,
    filesystem::{OpenHandle, RequestContext},
};
use w9pt_fs_state::{
    ClientIncarnationId, FilesystemStateStore, InodeKind, OpenAccess, ReadBatch, ReadConsistency,
    ReadOutcome, ReadQuery, ReadResult, StateLimitError, StateRecord,
};
use w9pt_fs_storage::{
    ContentRepository, FileContextScope, StorageError, TargetStore, open_committed_context,
};

use crate::{
    AccessRequirements, EngineLimits, ExportGrant, ExportPolicy, ExportPolicyRequest,
    authorization_client_error, check_inode_access, open_id_from_handle,
};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_read<S, T, P>(
    state: &S,
    content: &ContentRepository<T>,
    policy: &P,
    _limits: EngineLimits,
    context: RequestContext,
    client_incarnation: ClientIncarnationId,
    open: OpenHandle,
    offset: u64,
    count: u32,
) -> Result<Vec<u8>, DataReadError<S::Error, T::Error, P::Error>>
where
    S: FilesystemStateStore,
    T: TargetStore,
    P: ExportPolicy,
{
    let grant = policy
        .resolve(ExportPolicyRequest::new(context))
        .await
        .map_err(DataReadError::Policy)?;
    let open_id = open_id_from_handle(open);
    let first = read_snapshot(
        state,
        &grant,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Filesystem, ReadQuery::Open(open_id)],
    )
    .await?;
    let open_record = readable_open(&first.results()[1], open_id, client_incarnation)?;
    let inode_id = open_record.inode_id();
    let second = read_snapshot(
        state,
        &grant,
        ReadConsistency::AtLeast(first.revision()),
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Open(open_id),
            ReadQuery::Inode(inode_id),
        ],
    )
    .await?;
    let current_open = readable_open(&second.results()[1], open_id, client_incarnation)?;
    if current_open.inode_id() != inode_id {
        return Err(DataReadError::MalformedState);
    }
    let inode = point_inode(&second.results()[2])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    if inode.kind() != InodeKind::RegularFile {
        return Err(client(if inode.kind() == InodeKind::Directory {
            LinuxErrno::EISDIR
        } else {
            LinuxErrno::EOPNOTSUPP
        }));
    }
    check_inode_access(&grant, &inode, AccessRequirements::READ)
        .map_err(authorization_client_error)
        .map_err(DataReadError::Client)?;
    if inode.content().is_none() {
        if inode.logical_size() != 0 || inode.data_generation().is_some() {
            return Err(DataReadError::MalformedState);
        }
        return Ok(Vec::new());
    }

    let selected = read_snapshot(
        state,
        &grant,
        ReadConsistency::AtLeast(second.revision()),
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Open(open_id),
            ReadQuery::InodeWithContentMetadata(inode_id),
        ],
    )
    .await?;
    let selected_open = readable_open(&selected.results()[1], open_id, client_incarnation)?;
    if selected_open.inode_id() != inode_id {
        return Err(DataReadError::MalformedState);
    }
    let ReadResult::InodeWithContentMetadata {
        inode: Some(inode),
        metadata: Some(metadata),
        ..
    } = &selected.results()[2]
    else {
        return Err(DataReadError::MalformedState);
    };
    check_inode_access(&grant, inode, AccessRequirements::READ)
        .map_err(authorization_client_error)
        .map_err(DataReadError::Client)?;
    let content_ref = inode
        .content()
        .ok_or(DataReadError::MalformedState)?
        .clone();
    let scope = FileContextScope::new(
        *grant.filesystem_id().as_bytes(),
        *inode_id.as_bytes(),
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
        None,
    )
    .map_err(|error| DataReadError::Target(StorageError::Representation(error)))?;
    content
        .read_with_context(
            &content_ref,
            &context,
            offset,
            usize::try_from(count).map_err(|_| client(LinuxErrno::EOVERFLOW))?,
        )
        .await
        .map_err(DataReadError::Target)
}

async fn read_snapshot<S, T, P>(
    state: &S,
    grant: &ExportGrant,
    consistency: ReadConsistency,
    queries: Vec<ReadQuery>,
) -> Result<w9pt_fs_state::StateSnapshot, DataReadError<S::Error, T, P>>
where
    S: FilesystemStateStore,
{
    let request = ReadBatch::new(
        grant.filesystem_id(),
        consistency,
        queries,
        state.contract().limits(),
    )
    .map_err(DataReadError::MalformedRead)?;
    match state.read(request).await.map_err(DataReadError::State)? {
        ReadOutcome::Snapshot(snapshot) => {
            validate_filesystem(&snapshot.results()[0], grant)?;
            Ok(snapshot)
        }
        ReadOutcome::RevisionUnavailable { .. } => Err(DataReadError::RevisionUnavailable),
        ReadOutcome::MalformedRequest(error) => Err(DataReadError::MalformedRead(error)),
        ReadOutcome::ScanBoundTooSmall { .. } => Err(DataReadError::MalformedState),
    }
}

fn validate_filesystem<S, T, P>(
    result: &ReadResult,
    grant: &ExportGrant,
) -> Result<(), DataReadError<S, T, P>> {
    let ReadResult::Point {
        record: Some(record),
        ..
    } = result
    else {
        return Err(DataReadError::MalformedState);
    };
    let StateRecord::Filesystem(filesystem) = record.as_ref() else {
        return Err(DataReadError::MalformedState);
    };
    if filesystem.filesystem_id() != grant.filesystem_id()
        || filesystem.root_inode_id() != grant.root_inode_id()
        || filesystem.policy_generation() != grant.policy_generation()
    {
        return Err(client(LinuxErrno::EAGAIN));
    }
    Ok(())
}

fn readable_open<S, T, P>(
    result: &ReadResult,
    open_id: w9pt_fs_state::OpenId,
    client_incarnation: ClientIncarnationId,
) -> Result<w9pt_fs_state::OpenRecord, DataReadError<S, T, P>> {
    let ReadResult::Point {
        record: Some(record),
        ..
    } = result
    else {
        return Err(client(LinuxErrno::EBADF));
    };
    let StateRecord::Open(open) = record.as_ref() else {
        return Err(DataReadError::MalformedState);
    };
    if open.open_id() != open_id || open.client_incarnation() != client_incarnation {
        return Err(client(LinuxErrno::EBADF));
    }
    if !matches!(open.access(), OpenAccess::ReadOnly | OpenAccess::ReadWrite) {
        return Err(client(if open.access() == OpenAccess::DirectoryRead {
            LinuxErrno::EISDIR
        } else {
            LinuxErrno::EBADF
        }));
    }
    Ok(open.clone())
}

fn point_inode<S, T, P>(
    result: &ReadResult,
) -> Result<Option<w9pt_fs_state::InodeRecord>, DataReadError<S, T, P>> {
    let ReadResult::Point { record, .. } = result else {
        return Err(DataReadError::MalformedState);
    };
    match record.as_deref() {
        Some(StateRecord::Inode(inode)) => Ok(Some(inode.clone())),
        None => Ok(None),
        Some(_) => Err(DataReadError::MalformedState),
    }
}

#[derive(Debug)]
pub(crate) enum DataReadError<S, T, P> {
    Client(FilesystemError),
    State(S),
    Target(StorageError<T>),
    Policy(P),
    MalformedRead(StateLimitError),
    RevisionUnavailable,
    MalformedState,
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
    fn unpublished_read_is_target_free_and_published_read_is_positioned() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::BlockSplit).await;
            let created = environment.create_file("file", 100).await;
            environment.target.clear_trace().unwrap();
            let empty = execute_read(
                &environment.state,
                &environment.repository,
                &environment.policy,
                environment.engine_limits,
                environment.context(),
                environment.client,
                created.open,
                7,
                8,
            )
            .await
            .unwrap();
            assert!(empty.is_empty());
            assert!(environment.target.trace().unwrap().is_empty());

            environment
                .publish_new_content(created.object, b"0123456789", 101)
                .await;
            let positioned = execute_read(
                &environment.state,
                &environment.repository,
                &environment.policy,
                environment.engine_limits,
                environment.context(),
                environment.client,
                created.open,
                3,
                4,
            )
            .await
            .unwrap();
            assert_eq!(positioned, b"3456");

            let inode_id = crate::inode_id_from_handle(created.object);
            let read = ReadBatch::new(
                environment.filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![ReadQuery::Inode(inode_id)],
                environment.state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(snapshot) = environment.state.read(read).await.unwrap()
            else {
                panic!("inode state unavailable")
            };
            let ReadResult::Point {
                record: Some(record),
                ..
            } = &snapshot.results()[0]
            else {
                panic!("inode missing")
            };
            let StateRecord::Inode(inode) = record.as_ref() else {
                panic!("wrong record kind")
            };
            assert_eq!(inode.times().accessed, environment.now);
        });
    }
}
