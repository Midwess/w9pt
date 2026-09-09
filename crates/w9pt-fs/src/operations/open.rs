//! Existing-object open semantics and durable open-pin insertion.

use w9pt::{
    LinuxErrno,
    filesystem::{FilesystemRequest, FilesystemResult, OpenResult},
};
use w9pt_fs_state::{
    CommitRequest, DataGeneration, FilesystemStateStore, InodeAttributeUpdate, InodeKind,
    MutationContext, OpenAccess, OpenPinRecord, OpenRecord, Precondition, PublishContent,
    ReadBatch, ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision,
    StateChange, StateRecord,
};
use w9pt_fs_storage::{
    ContentRepository, FileContextScope, StorageError, TargetStore, open_committed_context,
};

use crate::{
    AccessRequirements, EngineLimits, ExecutionContext, ExportGrant, ExportPolicy,
    ExportPolicyRequest, IdentityScope, IdentitySource, OpenFlagError, OpenPurpose,
    authorization_client_error, check_directory_search, check_inode_access, check_mutation_allowed,
    encode_mutation_result, inode_id_from_handle, mutation_fingerprint, open_handle,
    qid_from_inode, run_mutation, validate_open_flags,
};

use super::mutation::{
    MutationOperationError, MutationPlanError, client, fingerprint_error, runner_error,
};

pub(crate) async fn execute_open<S, T, P, I>(
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
    let (object, flags) = match request.operation {
        w9pt::filesystem::FilesystemOperation::Open { object, flags } => (object, flags),
        _ => return Err(MutationOperationError::Internal),
    };
    let options = validate_open_flags(flags, OpenPurpose::Existing).map_err(|error| {
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
    let mutation = MutationContext::new(
        mutation_id,
        fingerprint,
        execution.client_incarnation,
        execution.retention,
    );
    let filesystem_id = initial_grant.filesystem_id();
    let context = request.context.clone();
    run_mutation(
        state,
        filesystem_id,
        mutation,
        fence,
        w9pt::filesystem::FilesystemResultKind::Opened,
        limits,
        |attempt| {
            let context = context.clone();
            let expected_grant = initial_grant.clone();
            async move {
                let grant = policy
                    .resolve(ExportPolicyRequest::new(context))
                    .await
                    .map_err(MutationPlanError::Policy)?;
                if grant != expected_grant {
                    return Err(client(LinuxErrno::EAGAIN));
                }
                plan_open(
                    state, content, identities, &grant, execution, mutation, fence, object,
                    options, attempt, limits,
                )
                .await
            }
        },
    )
    .await
    .map_err(runner_error)
}

#[allow(clippy::too_many_arguments)]
async fn plan_open<S, T, P, I>(
    state: &S,
    content_repository: &ContentRepository<T>,
    identities: &I,
    grant: &ExportGrant,
    execution: ExecutionContext,
    mutation: MutationContext,
    fence: w9pt_fs_state::WriterFence,
    object: w9pt::filesystem::ObjectHandle,
    options: crate::OpenOptions,
    attempt: u32,
    limits: EngineLimits,
) -> Result<CommitRequest, MutationPlanError<S::Error, T::Error, P, I::Error>>
where
    S: FilesystemStateStore,
    T: TargetStore,
    I: IdentitySource,
{
    let inode_id = inode_id_from_handle(object);
    let state_limits = state.contract().limits();
    let read = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Filesystem, ReadQuery::Inode(inode_id)],
        state_limits,
    )
    .map_err(MutationPlanError::MalformedRead)?;
    let ReadOutcome::Snapshot(snapshot) =
        state.read(read).await.map_err(MutationPlanError::State)?
    else {
        return Err(MutationPlanError::MalformedState);
    };
    let [filesystem, inode] = snapshot.results() else {
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
        record: Some(inode),
        ..
    } = inode
    else {
        return Err(client(LinuxErrno::EBADF));
    };
    let StateRecord::Inode(inode) = inode.as_ref() else {
        return Err(MutationPlanError::MalformedState);
    };
    let access = authorize_open(grant, inode, options)?;
    let mut selected_inode = inode.clone();
    let mut publication = None;
    let mut metadata_revision = None;
    if options.truncate() {
        if inode.kind() != InodeKind::RegularFile {
            return Err(client(LinuxErrno::EISDIR));
        }
        if inode.content().is_some() {
            let content_read = ReadBatch::new(
                grant.filesystem_id(),
                ReadConsistency::LatestLinearizable,
                vec![
                    ReadQuery::Filesystem,
                    ReadQuery::InodeWithContentMetadata(inode_id),
                ],
                state_limits,
            )
            .map_err(MutationPlanError::MalformedRead)?;
            let ReadOutcome::Snapshot(content_snapshot) = state
                .read(content_read)
                .await
                .map_err(MutationPlanError::State)?
            else {
                return Err(MutationPlanError::MalformedState);
            };
            let [filesystem_result, content_result] = content_snapshot.results() else {
                return Err(MutationPlanError::MalformedState);
            };
            validate_filesystem::<S::Error, T::Error, P, I::Error>(filesystem_result, grant)?;
            let ReadResult::InodeWithContentMetadata {
                inode: Some(current_inode),
                metadata: Some(metadata),
                ..
            } = content_result
            else {
                return Err(MutationPlanError::MalformedState);
            };
            authorize_open::<S::Error, T::Error, P, I::Error>(grant, current_inode, options)?;
            let current_content = current_inode
                .content()
                .ok_or(MutationPlanError::MalformedState)?;
            let scope = FileContextScope::new(
                *grant.filesystem_id().as_bytes(),
                *current_inode.inode_id().as_bytes(),
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
            .map_err(|error| MutationPlanError::Target(StorageError::Representation(error)))?;
            let prepared = content_repository
                .prepare_truncate_with_context(
                    current_content,
                    &context,
                    mutation.mutation_id,
                    attempt,
                    0,
                )
                .await
                .map_err(MutationPlanError::Target)?;
            let inode_generation = current_inode
                .inode_generation()
                .checked_next()
                .map_err(|_| client(LinuxErrno::EOVERFLOW))?;
            let data_generation = DataGeneration::new(prepared.content().generation())
                .map_err(|_| MutationPlanError::MalformedState)?;
            publication = Some(PublishContent {
                inode_id,
                expected_base: current_inode
                    .content_base()
                    .ok_or(MutationPlanError::MalformedState)?,
                logical_size: prepared.content().logical_size(),
                data_generation,
                prepared,
                inode_generation,
                attributes: InodeAttributeUpdate {
                    modified: execution.timestamp,
                    changed: execution.timestamp,
                    ..InodeAttributeUpdate::default()
                },
            });
            metadata_revision = Some((metadata.content_file_id(), metadata.revision()));
            selected_inode = current_inode.as_ref().clone();
        }
    }
    let open_id = identities
        .open_id(IdentityScope::new(
            grant.filesystem_id(),
            mutation.mutation_id,
            0,
        ))
        .map_err(MutationPlanError::Identity)?;
    let revision = RecordRevision::new(1).expect("one is a valid placeholder revision");
    let open = OpenRecord::new(
        open_id,
        inode_id,
        execution.client_incarnation,
        access,
        options.append(),
        publication
            .as_ref()
            .map_or(selected_inode.inode_generation(), |value| {
                value.inode_generation
            }),
        revision,
    );
    let pin = OpenPinRecord::new(inode_id, open_id, revision);
    let result = FilesystemResult::Opened(OpenResult {
        qid: qid_from_inode(&selected_inode),
        open: open_handle(open_id),
        io_unit: 0,
    });
    let terminal = encode_mutation_result(&result, limits, state_limits)
        .map_err(MutationPlanError::ResultCodec)?;
    let mut preconditions = vec![
        Precondition::FilesystemPolicyGeneration {
            expected: grant.policy_generation(),
        },
        Precondition::RecordRevision {
            key: RecordKey::Inode(grant.filesystem_id(), inode_id),
            expected: selected_inode.revision(),
        },
        Precondition::InodeGeneration {
            inode_id,
            expected: selected_inode.inode_generation(),
        },
        Precondition::RecordAbsent(RecordKey::Open(grant.filesystem_id(), open_id)),
        Precondition::RecordAbsent(RecordKey::OpenPin(grant.filesystem_id(), inode_id, open_id)),
    ];
    if let Some(value) = &publication {
        preconditions.push(Precondition::ContentBase {
            inode_id,
            expected: value.expected_base,
        });
        preconditions.push(Precondition::DataGeneration {
            inode_id,
            expected: selected_inode.data_generation(),
        });
    }
    if let Some((file_id, revision)) = metadata_revision {
        preconditions.push(Precondition::RecordRevision {
            key: RecordKey::ContentMetadata(grant.filesystem_id(), file_id),
            expected: revision,
        });
    }
    let mut changes = vec![
        StateChange::Insert {
            key: RecordKey::Open(grant.filesystem_id(), open_id),
            record: StateRecord::Open(open),
        },
        StateChange::Insert {
            key: RecordKey::OpenPin(grant.filesystem_id(), inode_id, open_id),
            record: StateRecord::OpenPin(pin),
        },
    ];
    if let Some(value) = publication {
        changes.push(StateChange::PublishContent(value));
    }
    CommitRequest::new(
        grant.filesystem_id(),
        mutation,
        fence,
        preconditions,
        changes,
        terminal,
        state_limits,
    )
    .map_err(MutationPlanError::MalformedCommit)
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

fn authorize_open<S, T, P, I>(
    grant: &ExportGrant,
    inode: &w9pt_fs_state::InodeRecord,
    options: crate::OpenOptions,
) -> Result<OpenAccess, MutationPlanError<S, T, P, I>> {
    match inode.kind() {
        InodeKind::Directory => {
            if !matches!(options.access(), OpenAccess::ReadOnly)
                || options.append()
                || options.truncate()
            {
                return Err(client(LinuxErrno::EISDIR));
            }
            check_directory_search(grant, inode)
                .and_then(|()| check_inode_access(grant, inode, AccessRequirements::READ))
                .map_err(authorization_client_error)
                .map_err(MutationPlanError::Client)?;
            Ok(options.directory_access())
        }
        InodeKind::RegularFile => {
            if options.directory() {
                return Err(client(LinuxErrno::ENOTDIR));
            }
            if matches!(
                options.access(),
                OpenAccess::WriteOnly | OpenAccess::ReadWrite
            ) {
                check_mutation_allowed(grant)
                    .map_err(authorization_client_error)
                    .map_err(MutationPlanError::Client)?;
            }
            let required = match options.access() {
                OpenAccess::ReadOnly => AccessRequirements::READ,
                OpenAccess::WriteOnly => AccessRequirements::WRITE,
                OpenAccess::ReadWrite => AccessRequirements {
                    read: true,
                    write: true,
                    execute: false,
                },
                OpenAccess::DirectoryRead => return Err(MutationPlanError::MalformedState),
            };
            check_inode_access(grant, inode, required)
                .map_err(authorization_client_error)
                .map_err(MutationPlanError::Client)?;
            Ok(options.access())
        }
        InodeKind::Symlink => Err(client(LinuxErrno::ELOOP)),
        InodeKind::CharacterDevice
        | InodeKind::BlockDevice
        | InodeKind::Fifo
        | InodeKind::Socket => Err(client(LinuxErrno::EOPNOTSUPP)),
    }
}

#[cfg(test)]
mod tests {
    use core::{convert::Infallible, future::Future};

    use super::*;
    use w9pt::filesystem::{
        CapabilitySet, ExportId, FilesystemOperation, PrincipalId, RequestContext,
    };
    use w9pt::protocol::{OpenFlags, SessionId};
    use w9pt_fs_state::{
        AcquireLeaseOutcome, AcquireWriterLease, ClientIncarnationId, CommitOutcome,
        DirectoryCookie, DirectoryGeneration, FilesystemId, FilesystemRecord, GroupId, InodeData,
        InodeGeneration, InodeId, InodeRecord, InodeTimes, LeaseDeadline, LeaseDuration, LeaseId,
        LeaseOperationId, ManualLeaseClock, MutationRetention, Precondition,
        PrincipalId as StatePrincipalId, QidPath, ReadResult, RecordRevision, RequestFingerprint,
        StateLimits, StateRevision, WriterIncarnationId, WriterScopeId, WriterTopology,
        testing::MemoryAuthority,
    };
    use w9pt_fs_storage::{
        ContentRepository, CreationDefaults, FileId, MutationId, StorageLimits, StorageMethod,
        testing::{MemoryTarget, block_on},
    };

    use crate::{
        CanonicalIdentity, ExportGrant, IdentityMappingRequest, NumericIdentity,
        ReverseIdentityMappingRequest,
    };

    #[derive(Clone)]
    struct Policy(ExportGrant);

    impl ExportPolicy for Policy {
        type Error = Infallible;

        fn resolve(
            &self,
            _request: ExportPolicyRequest,
        ) -> impl Future<Output = Result<ExportGrant, Self::Error>> + Send {
            core::future::ready(Ok(self.0.clone()))
        }

        fn map_numeric_identity(
            &self,
            request: IdentityMappingRequest,
        ) -> impl Future<Output = Result<NumericIdentity, Self::Error>> + Send {
            core::future::ready(Ok(match request.identity {
                CanonicalIdentity::Principal(_) => NumericIdentity::User(1),
                CanonicalIdentity::Group(_) => NumericIdentity::Group(1),
            }))
        }

        fn map_canonical_identity(
            &self,
            request: ReverseIdentityMappingRequest,
        ) -> impl Future<Output = Result<CanonicalIdentity, Self::Error>> + Send {
            core::future::ready(Ok(match request.identity {
                NumericIdentity::User(_) => {
                    CanonicalIdentity::Principal(self.0.principal().clone())
                }
                NumericIdentity::Group(_) => {
                    CanonicalIdentity::Group(self.0.primary_group().clone())
                }
            }))
        }
    }

    struct Identities;

    impl IdentitySource for Identities {
        type Error = Infallible;

        fn inode_id(&self, _scope: IdentityScope) -> Result<InodeId, Self::Error> {
            Ok(InodeId::from_u128(20))
        }

        fn open_id(&self, _scope: IdentityScope) -> Result<w9pt_fs_state::OpenId, Self::Error> {
            Ok(w9pt_fs_state::OpenId::from_u128(21))
        }

        fn content_file_id(&self, _scope: IdentityScope) -> Result<FileId, Self::Error> {
            Ok(FileId::from_u128(22))
        }
    }

    #[test]
    fn open_inserts_portable_open_and_pin_and_replays_exactly() {
        block_on(async {
            let state_limits = StateLimits::default();
            let engine_limits = EngineLimits::default();
            let filesystem_id = FilesystemId::from_u128(1);
            let root_id = InodeId::from_u128(2);
            let authority = MemoryAuthority::new(
                WriterTopology::SerializableMultiWriter,
                state_limits,
                ManualLeaseClock::new(LeaseDeadline::new(0)),
            );
            let state = authority.open_client();
            let acquire = AcquireWriterLease::new(
                filesystem_id,
                LeaseOperationId::from_u128(3),
                WriterScopeId::from_u128(4),
                WriterIncarnationId::from_u128(5),
                LeaseId::from_u128(6),
                LeaseDuration::new(100).unwrap(),
                state_limits,
            )
            .unwrap();
            let AcquireLeaseOutcome::Granted(lease) =
                state.acquire_writer_lease(acquire).await.unwrap()
            else {
                panic!("lease not granted")
            };
            let now = w9pt_fs_state::UnixTimestamp::new(0, 0).unwrap();
            let owner = StatePrincipalId::new(b"owner".to_vec(), state_limits).unwrap();
            let group = GroupId::new(b"group".to_vec(), state_limits).unwrap();
            let root = InodeRecord::new(
                root_id,
                QidPath::new(1).unwrap(),
                RecordRevision::new(1).unwrap(),
                0o755,
                owner.clone(),
                group.clone(),
                InodeTimes {
                    accessed: now,
                    modified: now,
                    changed: now,
                    created: now,
                },
                0,
                1,
                InodeGeneration::new(1).unwrap(),
                InodeData::Directory {
                    generation: DirectoryGeneration::new(1).unwrap(),
                    parent_inode_id: root_id,
                },
            )
            .unwrap();
            let filesystem = FilesystemRecord::new(
                filesystem_id,
                StateRevision::new(1).unwrap(),
                RecordRevision::new(1).unwrap(),
                root_id,
                QidPath::new(2).unwrap(),
                DirectoryCookie::new(1),
                1,
            )
            .unwrap();
            let bootstrap = CommitRequest::new(
                filesystem_id,
                MutationContext::new(
                    MutationId::from_u128(7),
                    RequestFingerprint::blake3(b"bootstrap"),
                    ClientIncarnationId::from_u128(8),
                    MutationRetention::new(100),
                ),
                lease.fence,
                vec![
                    Precondition::RecordAbsent(RecordKey::Filesystem(filesystem_id)),
                    Precondition::RecordAbsent(RecordKey::Inode(filesystem_id, root_id)),
                ],
                vec![
                    StateChange::Insert {
                        key: RecordKey::Filesystem(filesystem_id),
                        record: StateRecord::Filesystem(filesystem),
                    },
                    StateChange::Insert {
                        key: RecordKey::Inode(filesystem_id, root_id),
                        record: StateRecord::Inode(root),
                    },
                ],
                encode_mutation_result(&FilesystemResult::Released, engine_limits, state_limits)
                    .unwrap(),
                state_limits,
            )
            .unwrap();
            assert!(matches!(
                state.commit(bootstrap).await.unwrap(),
                CommitOutcome::Committed(_)
            ));
            let grant = ExportGrant::new(
                filesystem_id,
                root_id,
                owner,
                group,
                Vec::new(),
                1,
                1,
                false,
                true,
                1,
                CapabilitySet::ALL,
                engine_limits,
            )
            .unwrap();
            let request = FilesystemRequest::new(
                RequestContext::new(
                    SessionId::new(9),
                    PrincipalId::new("principal"),
                    ExportId::new("export"),
                ),
                FilesystemOperation::Open {
                    object: crate::object_handle(root_id),
                    flags: OpenFlags::DIRECTORY,
                },
            );
            let execution = ExecutionContext::new(
                ClientIncarnationId::from_u128(10),
                Some(MutationId::from_u128(11)),
                MutationRetention::new(100),
                Some(lease.fence),
                Some(now),
            );
            let repository = ContentRepository::new(
                MemoryTarget::new(),
                "open-test",
                CreationDefaults::new(StorageMethod::BlockSplit),
                StorageLimits::default(),
            )
            .unwrap();
            let first = execute_open::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                &state,
                &repository,
                &Policy(grant),
                &Identities,
                engine_limits,
                request.clone(),
                execution,
            )
            .await
            .unwrap();
            let replay = execute_open::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                &state,
                &repository,
                &Policy(
                    ExportGrant::new(
                        filesystem_id,
                        root_id,
                        StatePrincipalId::new(b"owner".to_vec(), state_limits).unwrap(),
                        GroupId::new(b"group".to_vec(), state_limits).unwrap(),
                        Vec::new(),
                        1,
                        1,
                        false,
                        true,
                        1,
                        CapabilitySet::ALL,
                        engine_limits,
                    )
                    .unwrap(),
                ),
                &Identities,
                engine_limits,
                request,
                execution,
            )
            .await
            .unwrap();
            assert_eq!(first, replay);
            let read = ReadBatch::new(
                filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![
                    ReadQuery::Open(w9pt_fs_state::OpenId::from_u128(21)),
                    ReadQuery::OpenPinCount(root_id),
                ],
                state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(snapshot) = state.read(read).await.unwrap() else {
                panic!("open state unavailable")
            };
            assert!(matches!(
                &snapshot.results()[0],
                ReadResult::Point { record: Some(record), .. }
                    if matches!(record.as_ref(), StateRecord::Open(open)
                        if open.client_incarnation() == execution.client_incarnation)
            ));
            assert!(matches!(
                &snapshot.results()[1],
                ReadResult::OpenPinCount { count: 1, .. }
            ));
        });
    }

    #[test]
    fn truncate_open_publishes_content_with_the_open_and_pin() {
        use crate::{object_handle, operations::execute_create, testing::TestEnvironment};
        use w9pt_fs_state::{DataGeneration, InodeAttributeUpdate, PublishContent};
        use w9pt_fs_storage::{BaseContentIdentity, FileContextScope, open_committed_context};

        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::BlockSplit).await;
            let create_request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Create {
                    directory: object_handle(environment.root_id),
                    name: "truncate-me".into(),
                    flags: OpenFlags::RDWR,
                    mode: 0o660,
                    gid: 1,
                },
            );
            let created = execute_create(
                &environment.state,
                &environment.repository,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                create_request,
                environment.execution(100),
            )
            .await
            .unwrap();
            let FilesystemResult::Created(created) = created else {
                panic!("unexpected create result")
            };
            let inode_id = inode_id_from_handle(created.object);
            let read = ReadBatch::new(
                environment.filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![ReadQuery::InodeWithContentMetadata(inode_id)],
                environment.state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(snapshot) = environment.state.read(read).await.unwrap()
            else {
                panic!("created file unavailable")
            };
            let ReadResult::InodeWithContentMetadata {
                inode: Some(inode),
                metadata: Some(metadata),
                ..
            } = &snapshot.results()[0]
            else {
                panic!("created context unavailable")
            };
            let context = open_committed_context(
                FileContextScope::new(
                    *environment.filesystem_id.as_bytes(),
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
            .unwrap();
            let seed_mutation = MutationId::from_u128(200);
            let prepared = environment
                .repository
                .prepare_write_from_new_with_context(&context, seed_mutation, 0, 0, b"payload")
                .await
                .unwrap();
            let seed = CommitRequest::new(
                environment.filesystem_id,
                MutationContext::new(
                    seed_mutation,
                    RequestFingerprint::blake3(b"seed content"),
                    environment.client,
                    MutationRetention::new(1_000),
                ),
                environment.fence,
                vec![
                    Precondition::ContentBase {
                        inode_id,
                        expected: BaseContentIdentity::NEW_FILE,
                    },
                    Precondition::RecordRevision {
                        key: RecordKey::ContentMetadata(
                            environment.filesystem_id,
                            metadata.content_file_id(),
                        ),
                        expected: metadata.revision(),
                    },
                ],
                vec![StateChange::PublishContent(PublishContent {
                    inode_id,
                    expected_base: BaseContentIdentity::NEW_FILE,
                    logical_size: prepared.content().logical_size(),
                    data_generation: DataGeneration::new(prepared.content().generation()).unwrap(),
                    inode_generation: inode.inode_generation().checked_next().unwrap(),
                    prepared,
                    attributes: InodeAttributeUpdate::default(),
                })],
                encode_mutation_result(
                    &FilesystemResult::Released,
                    environment.engine_limits,
                    environment.state_limits,
                )
                .unwrap(),
                environment.state_limits,
            )
            .unwrap();
            assert!(matches!(
                environment.state.commit(seed).await.unwrap(),
                CommitOutcome::Committed(_)
            ));

            let truncate_request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Open {
                    object: created.object,
                    flags: OpenFlags::WRONLY | OpenFlags::TRUNC,
                },
            );
            assert!(matches!(
                execute_open(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    truncate_request,
                    environment.execution(300),
                )
                .await
                .unwrap(),
                FilesystemResult::Opened(_)
            ));
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
                panic!("truncated file unavailable")
            };
            assert!(matches!(
                &snapshot.results()[0],
                ReadResult::Point { record: Some(record), .. }
                    if matches!(record.as_ref(), StateRecord::Inode(inode)
                        if inode.logical_size() == 0
                            && inode.data_generation().map(DataGeneration::get) == Some(2))
            ));
            assert!(matches!(
                &snapshot.results()[1],
                ReadResult::OpenPinCount { count: 2, .. }
            ));
        });
    }

    #[test]
    fn read_only_export_rejects_writable_truncate_before_target_access() {
        use crate::testing::{TestEnvironment, TestPolicy};

        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let created = environment.create_file("read-only", 400).await;
            environment
                .publish_new_content(created.object, b"preserve", 401)
                .await;
            let grant = &environment.policy.0;
            let read_only = TestPolicy(
                ExportGrant::new(
                    grant.filesystem_id(),
                    grant.root_inode_id(),
                    grant.principal().clone(),
                    grant.primary_group().clone(),
                    grant.supplementary_groups().to_vec(),
                    grant.numeric_uid(),
                    grant.numeric_gid(),
                    grant.privileged(),
                    true,
                    grant.policy_generation(),
                    grant.capability_ceiling(),
                    environment.engine_limits,
                )
                .unwrap(),
            );
            environment.target.clear_trace().unwrap();
            let request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Open {
                    object: created.object,
                    flags: OpenFlags::WRONLY | OpenFlags::TRUNC,
                },
            );
            assert!(matches!(
                execute_open(
                    &environment.state,
                    &environment.repository,
                    &read_only,
                    &environment.identities,
                    environment.engine_limits,
                    request,
                    environment.execution(402),
                )
                .await,
                Err(MutationOperationError::Client(error)) if error.errno == LinuxErrno::EROFS
            ));
            assert!(environment.target.trace().unwrap().is_empty());
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
                panic!("file state unavailable")
            };
            assert!(matches!(
                &snapshot.results()[0],
                ReadResult::Point { record: Some(record), .. }
                    if matches!(record.as_ref(), StateRecord::Inode(inode)
                        if inode.logical_size() == 8)
            ));
        });
    }
}
