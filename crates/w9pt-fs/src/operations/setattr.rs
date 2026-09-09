//! Atomic selected inode attributes with optional immutable truncation.

use w9pt::{
    LinuxErrno,
    filesystem::{FilesystemOperation, FilesystemRequest, FilesystemResult},
    protocol::{SetAttributes, SetattrMask},
};
use w9pt_fs_state::{
    CommitRequest, DataGeneration, FilesystemStateStore, GroupId, InodeAttributeUpdate, InodeData,
    InodeKind, InodeRecord, MutationContext, Precondition, PrincipalId, PublishContent, ReadBatch,
    ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision, StateChange,
    StateRecord, UnixTimestamp,
};
use w9pt_fs_storage::{
    ContentRepository, FileContextScope, StorageError, TargetStore, open_committed_context,
};

use crate::{
    AccessRequirements, CanonicalIdentity, EngineLimits, ExecutionContext, ExportGrant,
    ExportPolicy, ExportPolicyRequest, IdentitySource, NumericIdentity,
    ReverseIdentityMappingRequest, authorization_client_error, check_inode_access,
    check_mutation_allowed, check_owner_or_privileged, check_ownership_change,
    encode_mutation_result, inode_id_from_handle, mutation_fingerprint, run_mutation,
};

use super::mutation::{
    MutationOperationError, MutationPlanError, client, fingerprint_error, runner_error,
};

pub(crate) async fn execute_setattr<S, T, P, I>(
    state: &S,
    content: &ContentRepository<T>,
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
    let (object, attributes) = match request.operation {
        FilesystemOperation::Setattr { object, attributes } => (object, attributes),
        _ => return Err(MutationOperationError::Internal),
    };
    attributes
        .validate()
        .map_err(|_| terminal(LinuxErrno::EINVAL))?;
    if attributes.valid == SetattrMask::EMPTY
        || attributes.valid.contains(SetattrMask::MODE) && attributes.mode & !0o7777 != 0
    {
        return Err(terminal(LinuxErrno::EINVAL));
    }
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
        w9pt::filesystem::FilesystemResultKind::AttributesSet,
        limits,
        |attempt| {
            let context = context.clone();
            async move {
                let grant = policy
                    .resolve(ExportPolicyRequest::new(context))
                    .await
                    .map_err(MutationPlanError::Policy)?;
                if grant.filesystem_id() != filesystem_id || grant.root_inode_id() != root_inode_id
                {
                    return Err(client(LinuxErrno::EAGAIN));
                }
                let owner =
                    map_owner::<S::Error, T::Error, P, I::Error>(policy, &grant, attributes)
                        .await?;
                let group =
                    map_group::<S::Error, T::Error, P, I::Error>(policy, &grant, attributes)
                        .await?;
                plan_setattr::<S, T, P::Error, I::Error>(
                    state, content, &grant, mutation, fence, now, object, attributes, owner, group,
                    attempt, limits,
                )
                .await
            }
        },
    )
    .await
    .map_err(runner_error)
}

async fn map_owner<S, T, P, I>(
    policy: &P,
    grant: &ExportGrant,
    attributes: SetAttributes,
) -> Result<Option<PrincipalId>, MutationPlanError<S, T, P::Error, I>>
where
    P: ExportPolicy,
{
    if !attributes.valid.contains(SetattrMask::UID) {
        return Ok(None);
    }
    match policy
        .map_canonical_identity(ReverseIdentityMappingRequest::new(
            grant.filesystem_id(),
            grant.policy_generation(),
            NumericIdentity::User(attributes.uid),
        ))
        .await
        .map_err(MutationPlanError::Policy)?
    {
        CanonicalIdentity::Principal(value) => Ok(Some(value)),
        CanonicalIdentity::Group(_) => Err(MutationPlanError::MalformedState),
    }
}

async fn map_group<S, T, P, I>(
    policy: &P,
    grant: &ExportGrant,
    attributes: SetAttributes,
) -> Result<Option<GroupId>, MutationPlanError<S, T, P::Error, I>>
where
    P: ExportPolicy,
{
    if !attributes.valid.contains(SetattrMask::GID) {
        return Ok(None);
    }
    match policy
        .map_canonical_identity(ReverseIdentityMappingRequest::new(
            grant.filesystem_id(),
            grant.policy_generation(),
            NumericIdentity::Group(attributes.gid),
        ))
        .await
        .map_err(MutationPlanError::Policy)?
    {
        CanonicalIdentity::Group(value) => Ok(Some(value)),
        CanonicalIdentity::Principal(_) => Err(MutationPlanError::MalformedState),
    }
}

#[allow(clippy::too_many_arguments)]
async fn plan_setattr<S, T, P, I>(
    state: &S,
    repository: &ContentRepository<T>,
    grant: &ExportGrant,
    mutation: MutationContext,
    fence: w9pt_fs_state::WriterFence,
    now: UnixTimestamp,
    object: w9pt::filesystem::ObjectHandle,
    attributes: SetAttributes,
    owner: Option<PrincipalId>,
    group: Option<GroupId>,
    attempt: u32,
    limits: EngineLimits,
) -> Result<CommitRequest, MutationPlanError<S::Error, T::Error, P, I>>
where
    S: FilesystemStateStore,
    T: TargetStore,
{
    let state_limits = state.contract().limits();
    let inode_id = inode_id_from_handle(object);
    let first = read_snapshot::<S, T::Error, P, I>(
        state,
        grant,
        vec![ReadQuery::Filesystem, ReadQuery::Inode(inode_id)],
    )
    .await?;
    let inode = point_inode(&first.results()[1])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    authorize(grant, &inode, attributes, owner.as_ref(), group.as_ref())?;
    let update = selected_update(grant, &inode, attributes, owner, group, now)?;
    let result = encode_mutation_result(&FilesystemResult::AttributesSet, limits, state_limits)
        .map_err(MutationPlanError::ResultCodec)?;
    let fs = grant.filesystem_id();

    if !attributes.valid.contains(SetattrMask::SIZE)
        || inode.content().is_none() && attributes.size == 0
    {
        let replacement = rebuild_inode(&inode, &update)?;
        return CommitRequest::new(
            fs,
            mutation,
            fence,
            vec![
                Precondition::FilesystemPolicyGeneration {
                    expected: grant.policy_generation(),
                },
                Precondition::RecordRevision {
                    key: RecordKey::Inode(fs, inode_id),
                    expected: inode.revision(),
                },
                Precondition::InodeGeneration {
                    inode_id,
                    expected: inode.inode_generation(),
                },
            ],
            vec![StateChange::Replace {
                key: RecordKey::Inode(fs, inode_id),
                record: StateRecord::Inode(replacement),
            }],
            result,
            state_limits,
        )
        .map_err(MutationPlanError::MalformedCommit);
    }
    if inode.kind() != InodeKind::RegularFile {
        return Err(client(if inode.kind() == InodeKind::Directory {
            LinuxErrno::EISDIR
        } else {
            LinuxErrno::EINVAL
        }));
    }
    let selected = read_snapshot::<S, T::Error, P, I>(
        state,
        grant,
        vec![
            ReadQuery::Filesystem,
            ReadQuery::InodeWithContentMetadata(inode_id),
        ],
    )
    .await?;
    let ReadResult::InodeWithContentMetadata {
        inode: Some(inode),
        metadata: Some(metadata),
        ..
    } = &selected.results()[1]
    else {
        return Err(MutationPlanError::MalformedState);
    };
    authorize(
        grant,
        inode,
        attributes,
        update.owner.as_ref(),
        update.group.as_ref(),
    )?;
    let context = open_committed_context(
        FileContextScope::new(
            *fs.as_bytes(),
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
    .map_err(|error| MutationPlanError::Target(StorageError::Representation(error)))?;
    let prepared = match inode.content() {
        Some(current) => {
            repository
                .prepare_truncate_with_context(
                    current,
                    &context,
                    mutation.mutation_id,
                    attempt,
                    attributes.size,
                )
                .await
        }
        None => {
            repository
                .prepare_truncate_from_new_with_context(
                    &context,
                    mutation.mutation_id,
                    attempt,
                    attributes.size,
                )
                .await
        }
    }
    .map_err(MutationPlanError::Target)?;
    let publication = PublishContent {
        inode_id,
        expected_base: inode
            .content_base()
            .ok_or(MutationPlanError::MalformedState)?,
        logical_size: prepared.content().logical_size(),
        data_generation: DataGeneration::new(prepared.content().generation())
            .map_err(|_| MutationPlanError::MalformedState)?,
        inode_generation: inode
            .inode_generation()
            .checked_next()
            .map_err(|_| client(LinuxErrno::EOVERFLOW))?,
        prepared,
        attributes: update,
    };
    CommitRequest::new(
        fs,
        mutation,
        fence,
        vec![
            Precondition::FilesystemPolicyGeneration {
                expected: grant.policy_generation(),
            },
            Precondition::RecordRevision {
                key: RecordKey::Inode(fs, inode_id),
                expected: inode.revision(),
            },
            Precondition::InodeGeneration {
                inode_id,
                expected: inode.inode_generation(),
            },
            Precondition::DataGeneration {
                inode_id,
                expected: inode.data_generation(),
            },
            Precondition::ContentBase {
                inode_id,
                expected: publication.expected_base,
            },
            Precondition::RecordRevision {
                key: RecordKey::ContentMetadata(fs, metadata.content_file_id()),
                expected: metadata.revision(),
            },
        ],
        vec![StateChange::PublishContent(publication)],
        result,
        state_limits,
    )
    .map_err(MutationPlanError::MalformedCommit)
}

fn authorize<S, T, P, I>(
    grant: &ExportGrant,
    inode: &InodeRecord,
    attributes: SetAttributes,
    owner: Option<&PrincipalId>,
    group: Option<&GroupId>,
) -> Result<(), MutationPlanError<S, T, P, I>> {
    check_mutation_allowed(grant)
        .map_err(authorization_client_error)
        .map_err(MutationPlanError::Client)?;
    if attributes.valid.contains(SetattrMask::MODE)
        || attributes.valid.contains(SetattrMask::ATIME)
        || attributes.valid.contains(SetattrMask::MTIME)
        || attributes.valid.contains(SetattrMask::CTIME)
    {
        check_owner_or_privileged(grant, inode)
            .map_err(authorization_client_error)
            .map_err(MutationPlanError::Client)?;
    }
    if owner.is_some() || group.is_some() {
        check_ownership_change(grant, inode, owner, group)
            .map_err(authorization_client_error)
            .map_err(MutationPlanError::Client)?;
    }
    if attributes.valid.contains(SetattrMask::SIZE) {
        check_inode_access(grant, inode, AccessRequirements::WRITE)
            .map_err(authorization_client_error)
            .map_err(MutationPlanError::Client)?;
    }
    Ok(())
}

fn selected_update<S, T, P, I>(
    grant: &ExportGrant,
    inode: &InodeRecord,
    attributes: SetAttributes,
    owner: Option<PrincipalId>,
    group: Option<GroupId>,
    now: UnixTimestamp,
) -> Result<InodeAttributeUpdate, MutationPlanError<S, T, P, I>> {
    let mut mode = attributes
        .valid
        .contains(SetattrMask::MODE)
        .then_some(attributes.mode);
    if mode.is_some_and(|value| value & 0o2000 != 0)
        && !grant.privileged()
        && !grant.belongs_to_group(group.as_ref().unwrap_or_else(|| inode.group()))
    {
        mode = mode.map(|value| value & !0o2000);
    }
    Ok(InodeAttributeUpdate {
        mode,
        owner,
        group,
        accessed: selected_time(
            attributes.valid.contains(SetattrMask::ATIME),
            attributes.valid.contains(SetattrMask::ATIME_SET),
            attributes.accessed,
            now,
        )?,
        modified: selected_time(
            attributes.valid.contains(SetattrMask::MTIME),
            attributes.valid.contains(SetattrMask::MTIME_SET),
            attributes.modified,
            now,
        )?,
        changed: Some(now),
        created: None,
    })
}

fn selected_time<S, T, P, I>(
    selected: bool,
    explicit: bool,
    supplied: w9pt::protocol::Timestamp,
    now: UnixTimestamp,
) -> Result<Option<UnixTimestamp>, MutationPlanError<S, T, P, I>> {
    if !selected {
        return Ok(None);
    }
    if !explicit {
        return Ok(Some(now));
    }
    let seconds = i64::try_from(supplied.seconds).map_err(|_| client(LinuxErrno::EOVERFLOW))?;
    let nanoseconds =
        u32::try_from(supplied.nanoseconds).map_err(|_| client(LinuxErrno::EOVERFLOW))?;
    UnixTimestamp::new(seconds, nanoseconds)
        .map(Some)
        .map_err(|_| client(LinuxErrno::EINVAL))
}

fn rebuild_inode<S, T, P, I>(
    inode: &InodeRecord,
    update: &InodeAttributeUpdate,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    let generation = inode
        .inode_generation()
        .checked_next()
        .map_err(|_| client(LinuxErrno::EOVERFLOW))?;
    let revision = RecordRevision::new(1).expect("one is a valid placeholder revision");
    let mode = update.mode.unwrap_or(inode.mode());
    let owner = update
        .owner
        .clone()
        .unwrap_or_else(|| inode.owner().clone());
    let group = update
        .group
        .clone()
        .unwrap_or_else(|| inode.group().clone());
    let times = update.apply_times(inode.times());
    match inode.data().clone() {
        regular @ InodeData::RegularFile { .. } => InodeRecord::new_regular(
            inode.inode_id(),
            inode.qid_path(),
            revision,
            mode,
            owner,
            group,
            times,
            inode.logical_size(),
            inode.link_count(),
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
            mode,
            owner,
            group,
            times,
            inode.logical_size(),
            inode.link_count(),
            generation,
            other,
        ),
    }
    .map_err(|_| MutationPlanError::MalformedState)
}

async fn read_snapshot<S, T, P, I>(
    state: &S,
    grant: &ExportGrant,
    queries: Vec<ReadQuery>,
) -> Result<w9pt_fs_state::StateSnapshot, MutationPlanError<S::Error, T, P, I>>
where
    S: FilesystemStateStore,
{
    let request = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::LatestLinearizable,
        queries,
        state.contract().limits(),
    )
    .map_err(MutationPlanError::MalformedRead)?;
    let ReadOutcome::Snapshot(snapshot) = state
        .read(request)
        .await
        .map_err(MutationPlanError::State)?
    else {
        return Err(MutationPlanError::MalformedState);
    };
    validate_filesystem(&snapshot.results()[0], grant)?;
    Ok(snapshot)
}

fn validate_filesystem<S, T, P, I>(
    result: &ReadResult,
    grant: &ExportGrant,
) -> Result<(), MutationPlanError<S, T, P, I>> {
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
    Ok(())
}

fn point_inode<S, T, P, I>(
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

fn terminal<S, T, P, I>(errno: LinuxErrno) -> MutationOperationError<S, T, P, I> {
    MutationOperationError::Client(w9pt::FilesystemError::new(errno))
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt::protocol::Timestamp;
    use w9pt_fs_storage::{StorageMethod, testing::block_on};

    use crate::{operations::execute_read, testing::TestEnvironment};

    #[test]
    fn metadata_only_setattr_updates_selected_fields_and_replays() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let created = environment.create_file("metadata", 100).await;
            let request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Setattr {
                    object: created.object,
                    attributes: SetAttributes {
                        valid: SetattrMask::MODE
                            | SetattrMask::UID
                            | SetattrMask::GID
                            | SetattrMask::MTIME
                            | SetattrMask::MTIME_SET,
                        mode: 0o600,
                        uid: 1,
                        gid: 1,
                        modified: Timestamp {
                            seconds: 42,
                            nanoseconds: 43,
                        },
                        ..SetAttributes::default()
                    },
                },
            );
            let execution = environment.execution(101);
            let first = execute_setattr(
                &environment.state,
                &environment.repository,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                request.clone(),
                execution,
            )
            .await
            .unwrap();
            let replay = execute_setattr(
                &environment.state,
                &environment.repository,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                request,
                execution,
            )
            .await
            .unwrap();
            assert_eq!(first, FilesystemResult::AttributesSet);
            assert_eq!(first, replay);
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
                panic!("setattr inode unavailable")
            };
            assert!(matches!(
                &snapshot.results()[0],
                ReadResult::Point { record: Some(record), .. }
                    if matches!(record.as_ref(), StateRecord::Inode(inode)
                        if inode.mode() == 0o600
                            && inode.times().modified == UnixTimestamp::new(42, 43).unwrap()
                            && inode.times().changed == environment.now)
            ));
        });
    }

    #[test]
    fn size_and_mode_publish_together_from_new_and_on_shrink() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::BlockSplit).await;
            let created = environment.create_file("sized", 200).await;
            for (mutation, size, mode) in [(201, 9, 0o640), (202, 2, 0o600)] {
                let request = FilesystemRequest::new(
                    environment.context(),
                    FilesystemOperation::Setattr {
                        object: created.object,
                        attributes: SetAttributes {
                            valid: SetattrMask::SIZE | SetattrMask::MODE,
                            size,
                            mode,
                            ..SetAttributes::default()
                        },
                    },
                );
                assert_eq!(
                    execute_setattr(
                        &environment.state,
                        &environment.repository,
                        &environment.policy,
                        &environment.identities,
                        environment.engine_limits,
                        request,
                        environment.execution(mutation),
                    )
                    .await
                    .unwrap(),
                    FilesystemResult::AttributesSet
                );
            }
            assert_eq!(
                execute_read(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    environment.engine_limits,
                    environment.context(),
                    environment.client,
                    created.open,
                    0,
                    8,
                )
                .await
                .unwrap(),
                [0, 0]
            );
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
                panic!("sized inode unavailable")
            };
            assert!(matches!(
                &snapshot.results()[0],
                ReadResult::Point { record: Some(record), .. }
                    if matches!(record.as_ref(), StateRecord::Inode(inode)
                        if inode.logical_size() == 2 && inode.mode() == 0o600)
            ));
        });
    }
}
