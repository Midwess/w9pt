//! Atomic hard-link insertion.

use core::num::NonZeroU64;

use w9pt::{
    LinuxErrno,
    filesystem::{FilesystemOperation, FilesystemRequest, FilesystemResult},
};
use w9pt_fs_state::{
    CommitRequest, DirectoryEntryRecord, EntryName, FilesystemStateStore, InodeData, InodeKind,
    InodeRecord, MutationContext, Precondition, ReadBatch, ReadConsistency, ReadOutcome, ReadQuery,
    ReadResult, RecordKey, RecordRevision, StateChange, StateRecord,
};
use w9pt_fs_storage::TargetStore;

use crate::{
    EngineLimits, ExecutionContext, ExportGrant, ExportPolicy, ExportPolicyRequest, IdentitySource,
    authorization_client_error, check_directory_mutation, encode_mutation_result,
    inode_id_from_handle, mutation_fingerprint, run_mutation,
};

use super::mutation::{
    MutationOperationError, MutationPlanError, client, fingerprint_error, runner_error,
};

pub(crate) async fn execute_link<S, T, P, I>(
    state: &S,
    policy: &P,
    _identities: &I,
    limits: EngineLimits,
    request: FilesystemRequest,
    execution: ExecutionContext,
) -> Result<FilesystemResult, MutationOperationError<S::Error, T::Error, P::Error, I::Error>>
where
    S: FilesystemStateStore,
    T: TargetStore,
    P: ExportPolicy,
    I: IdentitySource,
{
    let (directory, target, name) = match &request.operation {
        FilesystemOperation::Link {
            directory,
            target,
            name,
        } => (*directory, *target, name.clone()),
        _ => return Err(MutationOperationError::Internal),
    };
    let initial_grant = policy
        .resolve(ExportPolicyRequest::new(request.context.clone()))
        .await
        .map_err(MutationOperationError::Policy)?;
    let fingerprint = mutation_fingerprint(&request, &execution, &initial_grant, limits)
        .map_err(fingerprint_error)?;
    let mutation_id = execution
        .mutation_id
        .ok_or(MutationOperationError::Context(
            crate::ExecutionContextError::MissingMutationId,
        ))?;
    let fence = execution.fence.ok_or(MutationOperationError::Context(
        crate::ExecutionContextError::MissingWriterFence,
    ))?;
    let now = execution.timestamp.ok_or(MutationOperationError::Context(
        crate::ExecutionContextError::MissingTimestamp,
    ))?;
    let mutation = MutationContext::new(
        mutation_id,
        fingerprint,
        execution.client_incarnation,
        execution.retention,
    );
    let filesystem_id = initial_grant.filesystem_id();
    let root_inode_id = initial_grant.root_inode_id();
    let context = request.context.clone();
    run_mutation(
        state,
        filesystem_id,
        mutation,
        fence,
        w9pt::filesystem::FilesystemResultKind::Linked,
        limits,
        |_| {
            let context = context.clone();
            let name = name.clone();
            async move {
                let grant = policy
                    .resolve(ExportPolicyRequest::new(context))
                    .await
                    .map_err(MutationPlanError::Policy)?;
                if grant.filesystem_id() != filesystem_id || grant.root_inode_id() != root_inode_id
                {
                    return Err(client(LinuxErrno::EAGAIN));
                }
                plan_link::<S, T::Error, P::Error, I::Error>(
                    state, &grant, mutation, fence, now, directory, target, name, limits,
                )
                .await
            }
        },
    )
    .await
    .map_err(runner_error)
}

#[allow(clippy::too_many_arguments)]
async fn plan_link<S, T, P, I>(
    state: &S,
    grant: &ExportGrant,
    mutation: MutationContext,
    fence: w9pt_fs_state::WriterFence,
    now: w9pt_fs_state::UnixTimestamp,
    directory: w9pt::filesystem::ObjectHandle,
    target: w9pt::filesystem::ObjectHandle,
    name: String,
    limits: EngineLimits,
) -> Result<CommitRequest, MutationPlanError<S::Error, T, P, I>>
where
    S: FilesystemStateStore,
{
    let state_limits = state.contract().limits();
    if name.len() > state_limits.max_entry_name_bytes() {
        return Err(client(LinuxErrno::ENAMETOOLONG));
    }
    let name =
        EntryName::new(name.into_bytes(), state_limits).map_err(|_| client(LinuxErrno::EINVAL))?;
    let parent_id = inode_id_from_handle(directory);
    let target_id = inode_id_from_handle(target);
    let request = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::LatestLinearizable,
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Inode(parent_id),
            ReadQuery::Inode(target_id),
            ReadQuery::DirectoryEntry {
                parent_inode_id: parent_id,
                name: name.clone(),
            },
        ],
        state_limits,
    )
    .map_err(MutationPlanError::MalformedRead)?;
    let ReadOutcome::Snapshot(snapshot) = state
        .read(request)
        .await
        .map_err(MutationPlanError::State)?
    else {
        return Err(MutationPlanError::MalformedState);
    };
    let [filesystem, parent, target, destination] = snapshot.results() else {
        return Err(MutationPlanError::MalformedState);
    };
    let filesystem = filesystem_record(filesystem, grant)?;
    let parent = inode_record(parent)?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    let target = inode_record(target)?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    check_directory_mutation(grant, &parent)
        .map_err(authorization_client_error)
        .map_err(MutationPlanError::Client)?;
    if target.kind() == InodeKind::Directory {
        return Err(client(LinuxErrno::EPERM));
    }
    if !matches!(destination, ReadResult::Point { record: None, .. }) {
        return Err(
            if matches!(
                destination,
                ReadResult::Point {
                    record: Some(_),
                    ..
                }
            ) {
                client(LinuxErrno::EEXIST)
            } else {
                MutationPlanError::MalformedState
            },
        );
    }
    let revision = RecordRevision::new(1).expect("one is a valid placeholder revision");
    let entry = DirectoryEntryRecord::new(
        parent_id,
        name.clone(),
        filesystem.next_directory_cookie(),
        target_id,
        revision,
    )
    .map_err(|_| MutationPlanError::MalformedState)?;
    let target_replacement = updated_target(&target, now)?;
    let parent_replacement = updated_parent(&parent, now)?;
    let terminal = encode_mutation_result(&FilesystemResult::Linked, limits, state_limits)
        .map_err(MutationPlanError::ResultCodec)?;
    let fs = grant.filesystem_id();
    CommitRequest::new(
        fs,
        mutation,
        fence,
        vec![
            Precondition::FilesystemPolicyGeneration {
                expected: grant.policy_generation(),
            },
            Precondition::RecordRevision {
                key: RecordKey::Filesystem(fs),
                expected: filesystem.record_revision(),
            },
            Precondition::RecordRevision {
                key: RecordKey::Inode(fs, parent_id),
                expected: parent.revision(),
            },
            Precondition::DirectoryGeneration {
                inode_id: parent_id,
                expected: parent
                    .directory_generation()
                    .ok_or(MutationPlanError::MalformedState)?,
            },
            Precondition::RecordRevision {
                key: RecordKey::Inode(fs, target_id),
                expected: target.revision(),
            },
            Precondition::LinkCount {
                inode_id: target_id,
                expected: target.link_count(),
            },
            Precondition::RecordAbsent(RecordKey::DirectoryEntry(fs, parent_id, name.clone())),
        ],
        vec![
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(fs, parent_id, name),
                record: StateRecord::DirectoryEntry(entry),
            },
            StateChange::Replace {
                key: RecordKey::Inode(fs, target_id),
                record: StateRecord::Inode(target_replacement),
            },
            StateChange::Replace {
                key: RecordKey::Inode(fs, parent_id),
                record: StateRecord::Inode(parent_replacement),
            },
            StateChange::AdvanceDirectoryCookie {
                count: NonZeroU64::new(1).expect("one is nonzero"),
            },
        ],
        terminal,
        state_limits,
    )
    .map_err(MutationPlanError::MalformedCommit)
}

fn filesystem_record<S, T, P, I>(
    result: &ReadResult,
    grant: &ExportGrant,
) -> Result<w9pt_fs_state::FilesystemRecord, MutationPlanError<S, T, P, I>> {
    let ReadResult::Point {
        record: Some(record),
        ..
    } = result
    else {
        return Err(MutationPlanError::MalformedState);
    };
    let StateRecord::Filesystem(filesystem) = record.as_ref() else {
        return Err(MutationPlanError::MalformedState);
    };
    if filesystem.filesystem_id() != grant.filesystem_id()
        || filesystem.root_inode_id() != grant.root_inode_id()
        || filesystem.policy_generation() != grant.policy_generation()
    {
        return Err(client(LinuxErrno::EAGAIN));
    }
    Ok(filesystem.clone())
}

fn inode_record<S, T, P, I>(
    result: &ReadResult,
) -> Result<Option<InodeRecord>, MutationPlanError<S, T, P, I>> {
    let ReadResult::Point { record, .. } = result else {
        return Err(MutationPlanError::MalformedState);
    };
    match record.as_deref() {
        Some(StateRecord::Inode(inode)) => Ok(Some(inode.clone())),
        None => Ok(None),
        Some(_) => Err(MutationPlanError::MalformedState),
    }
}

fn updated_target<S, T, P, I>(
    inode: &InodeRecord,
    now: w9pt_fs_state::UnixTimestamp,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    let links = inode
        .link_count()
        .checked_add(1)
        .ok_or_else(|| client(LinuxErrno::EOVERFLOW))?;
    rebuild(inode, links, now, inode.data().clone())
}

fn updated_parent<S, T, P, I>(
    inode: &InodeRecord,
    now: w9pt_fs_state::UnixTimestamp,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    let generation = inode
        .directory_generation()
        .ok_or(MutationPlanError::MalformedState)?
        .checked_next()
        .map_err(|_| client(LinuxErrno::EOVERFLOW))?;
    let parent_id = inode
        .directory_parent()
        .ok_or(MutationPlanError::MalformedState)?;
    rebuild(
        inode,
        inode.link_count(),
        now,
        InodeData::Directory {
            generation,
            parent_inode_id: parent_id,
        },
    )
}

fn rebuild<S, T, P, I>(
    inode: &InodeRecord,
    links: u64,
    now: w9pt_fs_state::UnixTimestamp,
    data: InodeData,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    let mut times = inode.times();
    times.changed = now;
    if matches!(data, InodeData::Directory { .. }) {
        times.modified = now;
    }
    let generation = inode
        .inode_generation()
        .checked_next()
        .map_err(|_| client(LinuxErrno::EOVERFLOW))?;
    let revision = RecordRevision::new(1).expect("one is a valid placeholder revision");
    match data {
        regular @ InodeData::RegularFile { .. } => InodeRecord::new_regular(
            inode.inode_id(),
            inode.qid_path(),
            revision,
            inode.mode(),
            inode.owner().clone(),
            inode.group().clone(),
            times,
            inode.logical_size(),
            links,
            generation,
            inode
                .content_context_id()
                .ok_or(MutationPlanError::MalformedState)?,
            regular,
        ),
        other => InodeRecord::new(
            inode.inode_id(),
            inode.qid_path(),
            revision,
            inode.mode(),
            inode.owner().clone(),
            inode.group().clone(),
            times,
            inode.logical_size(),
            links,
            generation,
            other,
        ),
    }
    .map_err(|_| MutationPlanError::MalformedState)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{operations::execute_walk, testing::TestEnvironment};
    use w9pt_fs_storage::{StorageMethod, testing::block_on};

    #[test]
    fn hard_link_preserves_qid_and_advances_link_count() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let created = environment.create_file("original", 100).await;
            let request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Link {
                    directory: crate::object_handle(environment.root_id),
                    target: created.object,
                    name: "alias".into(),
                },
            );
            assert_eq!(
                execute_link::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                    &environment.state,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    request,
                    environment.execution(101),
                )
                .await
                .unwrap(),
                FilesystemResult::Linked
            );
            let walked = execute_walk(
                &environment.state,
                &environment.policy,
                environment.engine_limits,
                environment.context(),
                crate::object_handle(environment.root_id),
                vec!["alias".into()],
            )
            .await
            .unwrap();
            assert_eq!(walked.elements[0].qid, created.qid);
            let inode_id = inode_id_from_handle(created.object);
            let read = ReadBatch::new(
                environment.filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![ReadQuery::Inode(inode_id)],
                environment.state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(snapshot) = environment.state.read(read).await.unwrap()
            else {
                panic!("inode unavailable")
            };
            assert!(matches!(&snapshot.results()[0],
                ReadResult::Point { record: Some(record), .. }
                    if matches!(record.as_ref(), StateRecord::Inode(inode) if inode.link_count() == 2)));
        });
    }
}
