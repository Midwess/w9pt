//! Atomic empty regular-file creation and open insertion.

use core::num::NonZeroU64;

use w9pt::{
    LinuxErrno,
    filesystem::{CreateResult, FilesystemOperation, FilesystemRequest, FilesystemResult},
};
use w9pt_fs_state::{
    CommitRequest, ContentMetadataRecord, DirectoryEntryRecord, EntryName, FilesystemStateStore,
    GroupId, InodeData, InodeGeneration, InodeKind, InodeRecord, InodeTimes, MutationContext,
    OpenPinRecord, OpenRecord, Precondition, ReadBatch, ReadConsistency, ReadOutcome, ReadQuery,
    ReadResult, RecordKey, RecordRevision, StateChange, StateRecord,
};
use w9pt_fs_storage::{ContentContextId, ContentRepository, FileStoragePolicy, TargetStore};

use crate::{
    CanonicalIdentity, EngineLimits, ExecutionContext, ExportGrant, ExportPolicy,
    ExportPolicyRequest, IdentityScope, IdentitySource, NumericIdentity, OpenFlagError,
    OpenPurpose, ReverseIdentityMappingRequest, authorization_client_error,
    check_directory_mutation, creation_attributes, encode_mutation_result, inode_id_from_handle,
    mutation_fingerprint, object_handle, open_handle, qid_from_inode, run_mutation,
    validate_open_flags,
};

use super::mutation::{
    MutationOperationError, MutationPlanError, client, fingerprint_error, runner_error,
};

pub(crate) async fn execute_create<S, T, P, I>(
    state: &S,
    content: &ContentRepository<T>,
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
    let (directory, name, flags, mode, gid) = match &request.operation {
        FilesystemOperation::Create {
            directory,
            name,
            flags,
            mode,
            gid,
        } => (*directory, name.clone(), *flags, *mode, *gid),
        _ => return Err(MutationOperationError::Internal),
    };
    let options = validate_open_flags(flags, OpenPurpose::Create).map_err(|error| {
        MutationOperationError::Client(w9pt::FilesystemError::new(match error {
            OpenFlagError::InvalidCombination => LinuxErrno::EINVAL,
            OpenFlagError::UnsupportedFlags { .. } => LinuxErrno::EOPNOTSUPP,
        }))
    })?;
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
    let timestamp = execution.timestamp.ok_or(MutationOperationError::Context(
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
        w9pt::filesystem::FilesystemResultKind::Created,
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
                let requested_group = policy
                    .map_canonical_identity(ReverseIdentityMappingRequest::new(
                        filesystem_id,
                        grant.policy_generation(),
                        NumericIdentity::Group(gid),
                    ))
                    .await
                    .map_err(MutationPlanError::Policy)?;
                let CanonicalIdentity::Group(requested_group) = requested_group else {
                    return Err(MutationPlanError::MalformedState);
                };
                plan_create(
                    state,
                    content,
                    identities,
                    &grant,
                    execution,
                    mutation,
                    fence,
                    timestamp,
                    directory,
                    name,
                    mode,
                    requested_group,
                    options,
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
async fn plan_create<S, T, P, I>(
    state: &S,
    content: &ContentRepository<T>,
    identities: &I,
    grant: &ExportGrant,
    execution: ExecutionContext,
    mutation: MutationContext,
    fence: w9pt_fs_state::WriterFence,
    timestamp: w9pt_fs_state::UnixTimestamp,
    directory: w9pt::filesystem::ObjectHandle,
    name: String,
    mode: u32,
    requested_group: GroupId,
    options: crate::OpenOptions,
    limits: EngineLimits,
) -> Result<CommitRequest, MutationPlanError<S::Error, T::Error, P, I::Error>>
where
    S: FilesystemStateStore,
    T: TargetStore,
    I: IdentitySource,
{
    let state_limits = state.contract().limits();
    if name.len() > state_limits.max_entry_name_bytes() {
        return Err(client(LinuxErrno::ENAMETOOLONG));
    }
    let entry_name =
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
                name: entry_name.clone(),
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
    let ReadResult::Point {
        record: Some(filesystem),
        ..
    } = filesystem
    else {
        return Err(MutationPlanError::MalformedState);
    };
    let StateRecord::Filesystem(filesystem) = filesystem.as_ref() else {
        return Err(MutationPlanError::MalformedState);
    };
    if filesystem.filesystem_id() != grant.filesystem_id()
        || filesystem.root_inode_id() != grant.root_inode_id()
        || filesystem.policy_generation() != grant.policy_generation()
    {
        return Err(client(LinuxErrno::EAGAIN));
    }
    let ReadResult::Point {
        record: Some(parent),
        ..
    } = parent
    else {
        return Err(client(LinuxErrno::EBADF));
    };
    let StateRecord::Inode(parent) = parent.as_ref() else {
        return Err(MutationPlanError::MalformedState);
    };
    check_directory_mutation(grant, parent)
        .map_err(authorization_client_error)
        .map_err(MutationPlanError::Client)?;
    if matches!(
        destination,
        ReadResult::Point {
            record: Some(_),
            ..
        }
    ) {
        return Err(client(LinuxErrno::EEXIST));
    }
    if !matches!(destination, ReadResult::Point { record: None, .. }) {
        return Err(MutationPlanError::MalformedState);
    }
    let creation =
        creation_attributes(grant, parent, requested_group, mode, InodeKind::RegularFile)
            .map_err(authorization_client_error)
            .map_err(MutationPlanError::Client)?;
    let inode_id = identities
        .inode_id(IdentityScope::new(
            grant.filesystem_id(),
            mutation.mutation_id,
            0,
        ))
        .map_err(MutationPlanError::Identity)?;
    let open_id = identities
        .open_id(IdentityScope::new(
            grant.filesystem_id(),
            mutation.mutation_id,
            0,
        ))
        .map_err(MutationPlanError::Identity)?;
    let file_id = identities
        .content_file_id(IdentityScope::new(
            grant.filesystem_id(),
            mutation.mutation_id,
            0,
        ))
        .map_err(MutationPlanError::Identity)?;
    let context_id = ContentContextId::new(*file_id.as_bytes());
    let revision = RecordRevision::new(1).expect("one is a valid placeholder revision");
    let inode = InodeRecord::new_regular(
        inode_id,
        filesystem.next_qid_path(),
        revision,
        creation.mode,
        grant.principal().clone(),
        creation.group,
        InodeTimes {
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
            created: timestamp,
        },
        0,
        1,
        InodeGeneration::new(1).expect("one is a valid inode generation"),
        context_id,
        InodeData::RegularFile {
            content_file_id: file_id,
            content: None,
            data_generation: 0,
        },
    )
    .map_err(|_| MutationPlanError::MalformedState)?;
    let storage_policy = FileStoragePolicy::plain(content.defaults().method());
    let metadata = ContentMetadataRecord::new(
        inode_id,
        file_id,
        context_id,
        FileStoragePolicy::FORMAT,
        storage_policy.to_bytes().to_vec(),
        None,
        None,
        revision,
        state_limits,
    )
    .map_err(|_| MutationPlanError::MalformedState)?;
    let dentry = DirectoryEntryRecord::new(
        parent_id,
        entry_name.clone(),
        filesystem.next_directory_cookie(),
        inode_id,
        revision,
    )
    .map_err(|_| MutationPlanError::MalformedState)?;
    let open = OpenRecord::new(
        open_id,
        inode_id,
        execution.client_incarnation,
        options.access(),
        options.append(),
        inode.inode_generation(),
        revision,
    );
    let pin = OpenPinRecord::new(inode_id, open_id, revision);
    let parent_replacement = updated_parent(parent, timestamp)?;
    let result = FilesystemResult::Created(CreateResult {
        object: object_handle(inode_id),
        qid: qid_from_inode(&inode),
        open: open_handle(open_id),
        io_unit: 0,
    });
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
            Precondition::RecordAbsent(RecordKey::DirectoryEntry(
                fs,
                parent_id,
                entry_name.clone(),
            )),
            Precondition::RecordAbsent(RecordKey::Inode(fs, inode_id)),
            Precondition::RecordAbsent(RecordKey::ContentMetadata(fs, file_id)),
            Precondition::RecordAbsent(RecordKey::Open(fs, open_id)),
            Precondition::RecordAbsent(RecordKey::OpenPin(fs, inode_id, open_id)),
        ],
        vec![
            StateChange::Insert {
                key: RecordKey::Inode(fs, inode_id),
                record: StateRecord::Inode(inode),
            },
            StateChange::Insert {
                key: RecordKey::ContentMetadata(fs, file_id),
                record: StateRecord::ContentMetadata(metadata),
            },
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(fs, parent_id, entry_name),
                record: StateRecord::DirectoryEntry(dentry),
            },
            StateChange::Insert {
                key: RecordKey::Open(fs, open_id),
                record: StateRecord::Open(open),
            },
            StateChange::Insert {
                key: RecordKey::OpenPin(fs, inode_id, open_id),
                record: StateRecord::OpenPin(pin),
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

fn updated_parent<S, T, P, I>(
    parent: &InodeRecord,
    timestamp: w9pt_fs_state::UnixTimestamp,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    let generation = parent
        .directory_generation()
        .ok_or(MutationPlanError::MalformedState)?
        .checked_next()
        .map_err(|_| client(LinuxErrno::EOVERFLOW))?;
    let inode_generation = parent
        .inode_generation()
        .checked_next()
        .map_err(|_| client(LinuxErrno::EOVERFLOW))?;
    let mut times = parent.times();
    times.modified = timestamp;
    times.changed = timestamp;
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
        inode_generation,
        InodeData::Directory {
            generation,
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
    use w9pt::protocol::OpenFlags;
    use w9pt_fs_state::{ReadResult, StateRecord};
    use w9pt_fs_storage::{StorageMethod, testing::block_on};

    use crate::testing::TestEnvironment;

    #[test]
    fn create_is_atomic_empty_target_free_and_exactly_replayed() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::BlockSplit).await;
            let request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Create {
                    directory: object_handle(environment.root_id),
                    name: "file".into(),
                    flags: OpenFlags::CREATE | OpenFlags::EXCL | OpenFlags::RDWR,
                    mode: 0o2640,
                    gid: 1,
                },
            );
            let execution = environment.execution(100);
            let first = execute_create(
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
            let replay = execute_create(
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
            assert_eq!(first, replay);
            assert!(environment.target.trace().unwrap().is_empty());
            let FilesystemResult::Created(created) = first else {
                panic!("unexpected create result")
            };
            let inode_id = inode_id_from_handle(created.object);
            let read = ReadBatch::new(
                environment.filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![
                    ReadQuery::Inode(inode_id),
                    ReadQuery::OpenPinCount(inode_id),
                ],
                environment.state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(snapshot) = environment.state.read(read).await.unwrap()
            else {
                panic!("created state unavailable")
            };
            assert!(matches!(
                &snapshot.results()[0],
                ReadResult::Point { record: Some(record), .. }
                    if matches!(record.as_ref(), StateRecord::Inode(inode)
                        if inode.content().is_none()
                            && inode.logical_size() == 0
                            && inode.mode() == 0o2640)
            ));
            assert!(matches!(
                &snapshot.results()[1],
                ReadResult::OpenPinCount { count: 1, .. }
            ));
        });
    }

    #[test]
    fn read_only_create_stops_before_state_and_target_mutation() {
        block_on(async {
            let environment = TestEnvironment::new(true, StorageMethod::Raw).await;
            environment.target.clear_trace().unwrap();
            let request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Create {
                    directory: object_handle(environment.root_id),
                    name: "denied".into(),
                    flags: OpenFlags::WRONLY,
                    mode: 0o600,
                    gid: 1,
                },
            );
            assert!(matches!(
                execute_create(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    request,
                    environment.execution(101),
                )
                .await,
                Err(MutationOperationError::Client(error)) if error.errno == LinuxErrno::EROFS
            ));
            assert!(environment.target.trace().unwrap().is_empty());
        });
    }
}
