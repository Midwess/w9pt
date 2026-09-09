//! Atomic directory and symbolic-link creation.

use core::num::NonZeroU64;

use w9pt::{
    LinuxErrno,
    filesystem::{FilesystemOperation, FilesystemRequest, FilesystemResult, NodeResult},
};
use w9pt_fs_state::{
    CommitRequest, DirectoryEntryRecord, DirectoryGeneration, EntryName, FilesystemStateStore,
    GroupId, InodeData, InodeGeneration, InodeKind, InodeRecord, InodeTimes, MutationContext,
    Precondition, ReadBatch, ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey,
    RecordRevision, StateChange, StateRecord, SymlinkTarget,
};
use w9pt_fs_storage::TargetStore;

use crate::{
    CanonicalIdentity, EngineLimits, ExecutionContext, ExportGrant, ExportPolicy,
    ExportPolicyRequest, IdentityScope, IdentitySource, NumericIdentity,
    ReverseIdentityMappingRequest, authorization_client_error, check_directory_mutation,
    creation_attributes, encode_mutation_result, inode_id_from_handle, mutation_fingerprint,
    object_handle, qid_from_inode, run_mutation,
};

use super::mutation::{
    MutationOperationError, MutationPlanError, client, fingerprint_error, runner_error,
};

#[derive(Clone)]
enum NewNode {
    Directory { mode: u32 },
    Symlink { target: String },
}

impl NewNode {
    const fn kind(&self) -> InodeKind {
        match self {
            Self::Directory { .. } => InodeKind::Directory,
            Self::Symlink { .. } => InodeKind::Symlink,
        }
    }

    const fn requested_mode(&self) -> u32 {
        match self {
            Self::Directory { mode } => *mode,
            Self::Symlink { .. } => 0o777,
        }
    }

    const fn result_kind(&self) -> w9pt::filesystem::FilesystemResultKind {
        match self {
            Self::Directory { .. } => w9pt::filesystem::FilesystemResultKind::DirectoryCreated,
            Self::Symlink { .. } => w9pt::filesystem::FilesystemResultKind::SymlinkCreated,
        }
    }
}

pub(crate) async fn execute_mkdir<S, T, P, I>(
    state: &S,
    policy: &P,
    identities: &I,
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
    let (directory, name, mode, gid) = match &request.operation {
        FilesystemOperation::Mkdir {
            directory,
            name,
            mode,
            gid,
        } => (*directory, name.clone(), *mode, *gid),
        _ => return Err(MutationOperationError::Internal),
    };
    execute_new_node::<S, T, P, I>(
        state,
        policy,
        identities,
        limits,
        request,
        execution,
        directory,
        name,
        gid,
        NewNode::Directory { mode },
    )
    .await
}

pub(crate) async fn execute_symlink<S, T, P, I>(
    state: &S,
    policy: &P,
    identities: &I,
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
    let (directory, name, target, gid) = match &request.operation {
        FilesystemOperation::Symlink {
            directory,
            name,
            target,
            gid,
        } => (*directory, name.clone(), target.clone(), *gid),
        _ => return Err(MutationOperationError::Internal),
    };
    execute_new_node::<S, T, P, I>(
        state,
        policy,
        identities,
        limits,
        request,
        execution,
        directory,
        name,
        gid,
        NewNode::Symlink { target },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_new_node<S, T, P, I>(
    state: &S,
    policy: &P,
    identities: &I,
    limits: EngineLimits,
    request: FilesystemRequest,
    execution: ExecutionContext,
    directory: w9pt::filesystem::ObjectHandle,
    name: String,
    gid: u32,
    node: NewNode,
) -> Result<FilesystemResult, MutationOperationError<S::Error, T::Error, P::Error, I::Error>>
where
    S: FilesystemStateStore,
    T: TargetStore,
    P: ExportPolicy,
    I: IdentitySource,
{
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
        node.result_kind(),
        limits,
        |_| {
            let context = context.clone();
            let name = name.clone();
            let node = node.clone();
            async move {
                let grant = policy
                    .resolve(ExportPolicyRequest::new(context))
                    .await
                    .map_err(MutationPlanError::Policy)?;
                if grant.filesystem_id() != filesystem_id || grant.root_inode_id() != root_inode_id
                {
                    return Err(client(LinuxErrno::EAGAIN));
                }
                let requested_group = match policy
                    .map_canonical_identity(ReverseIdentityMappingRequest::new(
                        filesystem_id,
                        grant.policy_generation(),
                        NumericIdentity::Group(gid),
                    ))
                    .await
                    .map_err(MutationPlanError::Policy)?
                {
                    CanonicalIdentity::Group(value) => value,
                    CanonicalIdentity::Principal(_) => {
                        return Err(MutationPlanError::MalformedState);
                    }
                };
                plan_new_node::<S, T::Error, P::Error, I>(
                    state,
                    identities,
                    &grant,
                    mutation,
                    fence,
                    now,
                    directory,
                    name,
                    requested_group,
                    node,
                    limits,
                )
                .await
            }
        },
    )
    .await
    .map_err(runner_error)
}

#[allow(clippy::too_many_arguments)]
async fn plan_new_node<S, T, P, I>(
    state: &S,
    identities: &I,
    grant: &ExportGrant,
    mutation: MutationContext,
    fence: w9pt_fs_state::WriterFence,
    now: w9pt_fs_state::UnixTimestamp,
    directory: w9pt::filesystem::ObjectHandle,
    name: String,
    requested_group: GroupId,
    node: NewNode,
    limits: EngineLimits,
) -> Result<CommitRequest, MutationPlanError<S::Error, T, P, I::Error>>
where
    S: FilesystemStateStore,
    I: IdentitySource,
{
    let state_limits = state.contract().limits();
    if name.len() > state_limits.max_entry_name_bytes() {
        return Err(client(LinuxErrno::ENAMETOOLONG));
    }
    let name =
        EntryName::new(name.into_bytes(), state_limits).map_err(|_| client(LinuxErrno::EINVAL))?;
    let parent_id = inode_id_from_handle(directory);
    let read = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::LatestLinearizable,
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Inode(parent_id),
            ReadQuery::DirectoryEntry {
                parent_inode_id: parent_id,
                name: name.clone(),
            },
        ],
        state_limits,
    )
    .map_err(MutationPlanError::MalformedRead)?;
    let ReadOutcome::Snapshot(snapshot) =
        state.read(read).await.map_err(MutationPlanError::State)?
    else {
        return Err(MutationPlanError::MalformedState);
    };
    let [filesystem, parent, destination] = snapshot.results() else {
        return Err(MutationPlanError::MalformedState);
    };
    let filesystem = filesystem_record(filesystem, grant)?;
    let parent = inode_record(parent)?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    check_directory_mutation(grant, &parent)
        .map_err(authorization_client_error)
        .map_err(MutationPlanError::Client)?;
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
    let creation = creation_attributes(
        grant,
        &parent,
        requested_group,
        node.requested_mode(),
        node.kind(),
    )
    .map_err(authorization_client_error)
    .map_err(MutationPlanError::Client)?;
    let inode_id = identities
        .inode_id(IdentityScope::new(
            grant.filesystem_id(),
            mutation.mutation_id,
            0,
        ))
        .map_err(MutationPlanError::Identity)?;
    let revision = RecordRevision::new(1).expect("one is a valid placeholder revision");
    let data = match node {
        NewNode::Directory { .. } => InodeData::Directory {
            generation: DirectoryGeneration::new(1).expect("one is nonzero"),
            parent_inode_id: parent_id,
        },
        NewNode::Symlink { target } => InodeData::Symlink {
            target: SymlinkTarget::new(target.into_bytes(), state_limits)
                .map_err(|_| client(LinuxErrno::ENAMETOOLONG))?,
        },
    };
    let logical_size = match &data {
        InodeData::Symlink { target } => {
            u64::try_from(target.as_bytes().len()).map_err(|_| client(LinuxErrno::EOVERFLOW))?
        }
        _ => 0,
    };
    let inode = InodeRecord::new(
        inode_id,
        filesystem.next_qid_path(),
        revision,
        creation.mode,
        grant.principal().clone(),
        creation.group,
        InodeTimes {
            accessed: now,
            modified: now,
            changed: now,
            created: now,
        },
        logical_size,
        1,
        InodeGeneration::new(1).expect("one is nonzero"),
        data,
    )
    .map_err(|_| MutationPlanError::MalformedState)?;
    let dentry = DirectoryEntryRecord::new(
        parent_id,
        name.clone(),
        filesystem.next_directory_cookie(),
        inode_id,
        revision,
    )
    .map_err(|_| MutationPlanError::MalformedState)?;
    let parent_replacement = updated_parent(&parent, now)?;
    let node_result = NodeResult {
        object: object_handle(inode_id),
        qid: qid_from_inode(&inode),
    };
    let result = match inode.kind() {
        InodeKind::Directory => FilesystemResult::DirectoryCreated(node_result),
        InodeKind::Symlink => FilesystemResult::SymlinkCreated(node_result),
        _ => return Err(MutationPlanError::MalformedState),
    };
    let terminal = encode_mutation_result(&result, limits, state_limits)
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
            Precondition::RecordAbsent(RecordKey::DirectoryEntry(fs, parent_id, name.clone())),
            Precondition::RecordAbsent(RecordKey::Inode(fs, inode_id)),
        ],
        vec![
            StateChange::Insert {
                key: RecordKey::Inode(fs, inode_id),
                record: StateRecord::Inode(inode),
            },
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(fs, parent_id, name),
                record: StateRecord::DirectoryEntry(dentry),
            },
            StateChange::Replace {
                key: RecordKey::Inode(fs, parent_id),
                record: StateRecord::Inode(parent_replacement),
            },
            StateChange::AdvanceQidPath {
                count: NonZeroU64::new(1).expect("one is nonzero"),
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

fn updated_parent<S, T, P, I>(
    parent: &InodeRecord,
    now: w9pt_fs_state::UnixTimestamp,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    let mut times = parent.times();
    times.modified = now;
    times.changed = now;
    InodeRecord::new(
        parent.inode_id(),
        parent.qid_path(),
        RecordRevision::new(1).expect("one is a valid placeholder revision"),
        parent.mode(),
        parent.owner().clone(),
        parent.group().clone(),
        times,
        parent.logical_size(),
        parent.link_count(),
        parent
            .inode_generation()
            .checked_next()
            .map_err(|_| client(LinuxErrno::EOVERFLOW))?,
        InodeData::Directory {
            generation: parent
                .directory_generation()
                .ok_or(MutationPlanError::MalformedState)?
                .checked_next()
                .map_err(|_| client(LinuxErrno::EOVERFLOW))?,
            parent_inode_id: parent
                .directory_parent()
                .ok_or(MutationPlanError::MalformedState)?,
        },
    )
    .map_err(|_| MutationPlanError::MalformedState)
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt_fs_storage::{StorageMethod, testing::block_on};

    use crate::{operations::execute_walk, testing::TestEnvironment};

    #[test]
    fn mkdir_and_symlink_allocate_stable_nodes_and_replay() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let mkdir = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Mkdir {
                    directory: object_handle(environment.root_id),
                    name: "dir".into(),
                    mode: 0o750,
                    gid: 1,
                },
            );
            let execution = environment.execution(100);
            let first = execute_mkdir::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                &environment.state,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                mkdir.clone(),
                execution,
            )
            .await
            .unwrap();
            let replay = execute_mkdir::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                &environment.state,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                mkdir,
                execution,
            )
            .await
            .unwrap();
            assert_eq!(first, replay);
            let symlink = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Symlink {
                    directory: object_handle(environment.root_id),
                    name: "link".into(),
                    target: "dir/target".into(),
                    gid: 1,
                },
            );
            assert!(matches!(
                execute_symlink::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                    &environment.state,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    symlink,
                    environment.execution(101),
                )
                .await
                .unwrap(),
                FilesystemResult::SymlinkCreated(_)
            ));
            let walked = execute_walk(
                &environment.state,
                &environment.policy,
                environment.engine_limits,
                environment.context(),
                object_handle(environment.root_id),
                vec!["dir".into()],
            )
            .await
            .unwrap();
            assert_eq!(walked.elements.len(), 1);
        });
    }
}
