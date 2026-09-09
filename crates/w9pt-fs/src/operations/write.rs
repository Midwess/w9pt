//! Atomic positioned write and append content publication.

use w9pt::{
    LinuxErrno,
    filesystem::{FilesystemOperation, FilesystemRequest, FilesystemResult},
};
use w9pt_fs_state::{
    CommitRequest, DataGeneration, FilesystemStateStore, InodeAttributeUpdate, InodeKind,
    MutationContext, OpenAccess, Precondition, PublishContent, ReadBatch, ReadConsistency,
    ReadOutcome, ReadQuery, ReadResult, RecordKey, StateChange, StateRecord,
};
use w9pt_fs_storage::{
    ContentRepository, FileContextScope, StorageError, TargetStore, open_committed_context,
};

use crate::{
    AccessRequirements, EngineLimits, ExecutionContext, ExportGrant, ExportPolicy,
    ExportPolicyRequest, IdentitySource, authorization_client_error, check_inode_access,
    check_mutation_allowed, encode_mutation_result, mutation_fingerprint, open_id_from_handle,
    run_mutation,
};

use super::mutation::{
    MutationOperationError, MutationPlanError, client, fingerprint_error, runner_error,
};

pub(crate) async fn execute_write<S, T, P, I>(
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
    let (open, requested_offset, data) = match &request.operation {
        FilesystemOperation::Write { open, offset, data } => (*open, *offset, data.clone()),
        _ => return Err(MutationOperationError::Internal),
    };
    let written = u32::try_from(data.len()).map_err(|_| {
        MutationOperationError::Client(w9pt::FilesystemError::new(LinuxErrno::EOVERFLOW))
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
    let context = request.context.clone();
    run_mutation(
        state,
        filesystem_id,
        mutation,
        fence,
        w9pt::filesystem::FilesystemResultKind::Written,
        limits,
        |attempt| {
            let context = context.clone();
            let data = data.clone();
            let expected_grant = initial_grant.clone();
            async move {
                let grant = policy
                    .resolve(ExportPolicyRequest::new(context))
                    .await
                    .map_err(MutationPlanError::Policy)?;
                if grant != expected_grant {
                    return Err(client(LinuxErrno::EAGAIN));
                }
                plan_write::<S, T, P::Error, I::Error>(
                    state,
                    content,
                    &grant,
                    execution,
                    mutation,
                    fence,
                    timestamp,
                    open,
                    requested_offset,
                    &data,
                    written,
                    attempt,
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
async fn plan_write<S, T, P, I>(
    state: &S,
    content_repository: &ContentRepository<T>,
    grant: &ExportGrant,
    execution: ExecutionContext,
    mutation: MutationContext,
    fence: w9pt_fs_state::WriterFence,
    timestamp: w9pt_fs_state::UnixTimestamp,
    open: w9pt::filesystem::OpenHandle,
    requested_offset: u64,
    data: &[u8],
    written: u32,
    attempt: u32,
    limits: EngineLimits,
) -> Result<CommitRequest, MutationPlanError<S::Error, T::Error, P, I>>
where
    S: FilesystemStateStore,
    T: TargetStore,
{
    let state_limits = state.contract().limits();
    let open_id = open_id_from_handle(open);
    let first = read_snapshot::<S, T::Error, P, I>(
        state,
        grant,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Filesystem, ReadQuery::Open(open_id)],
    )
    .await?;
    let open_record = writable_open(&first.results()[1], open_id, execution.client_incarnation)?;
    let inode_id = open_record.inode_id();
    let kind_snapshot = read_snapshot::<S, T::Error, P, I>(
        state,
        grant,
        ReadConsistency::AtLeast(first.revision()),
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Open(open_id),
            ReadQuery::Inode(inode_id),
        ],
    )
    .await?;
    let checked_open = writable_open(
        &kind_snapshot.results()[1],
        open_id,
        execution.client_incarnation,
    )?;
    if checked_open.inode_id() != inode_id {
        return Err(MutationPlanError::MalformedState);
    }
    let inode =
        point_inode(&kind_snapshot.results()[2])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    if inode.kind() != InodeKind::RegularFile {
        return Err(client(if inode.kind() == InodeKind::Directory {
            LinuxErrno::EISDIR
        } else {
            LinuxErrno::EOPNOTSUPP
        }));
    }

    let selected = read_snapshot::<S, T::Error, P, I>(
        state,
        grant,
        ReadConsistency::AtLeast(kind_snapshot.revision()),
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Open(open_id),
            ReadQuery::InodeWithContentMetadata(inode_id),
        ],
    )
    .await?;
    let selected_open = writable_open(
        &selected.results()[1],
        open_id,
        execution.client_incarnation,
    )?;
    if selected_open.inode_id() != inode_id {
        return Err(MutationPlanError::MalformedState);
    }
    let ReadResult::InodeWithContentMetadata {
        inode: Some(inode),
        metadata: Some(metadata),
        ..
    } = &selected.results()[2]
    else {
        return Err(MutationPlanError::MalformedState);
    };
    check_mutation_allowed(grant)
        .and_then(|()| check_inode_access(grant, inode, AccessRequirements::WRITE))
        .map_err(authorization_client_error)
        .map_err(MutationPlanError::Client)?;
    if data.is_empty() {
        let result = encode_mutation_result(&FilesystemResult::Written(0), limits, state_limits)
            .map_err(MutationPlanError::ResultCodec)?;
        let fs = grant.filesystem_id();
        return CommitRequest::new(
            fs,
            mutation,
            fence,
            vec![
                Precondition::FilesystemPolicyGeneration {
                    expected: grant.policy_generation(),
                },
                Precondition::RecordRevision {
                    key: RecordKey::Open(fs, open_id),
                    expected: selected_open.revision(),
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
            vec![StateChange::RetainResult],
            result,
            state_limits,
        )
        .map_err(MutationPlanError::MalformedCommit);
    }
    let offset = if selected_open.append() {
        inode.logical_size()
    } else {
        requested_offset
    };
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
    .map_err(|error| MutationPlanError::Target(StorageError::Representation(error)))?;
    let prepared = match inode.content() {
        Some(current) => {
            content_repository
                .prepare_write_with_context(
                    current,
                    &context,
                    mutation.mutation_id,
                    attempt,
                    offset,
                    data,
                )
                .await
        }
        None => {
            content_repository
                .prepare_write_from_new_with_context(
                    &context,
                    mutation.mutation_id,
                    attempt,
                    offset,
                    data,
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
        attributes: InodeAttributeUpdate {
            modified: Some(timestamp),
            changed: Some(timestamp),
            ..InodeAttributeUpdate::default()
        },
    };
    let result = FilesystemResult::Written(written);
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
                key: RecordKey::Open(fs, open_id),
                expected: selected_open.revision(),
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
        terminal,
        state_limits,
    )
    .map_err(MutationPlanError::MalformedCommit)
}

async fn read_snapshot<S, T, P, I>(
    state: &S,
    grant: &ExportGrant,
    consistency: ReadConsistency,
    queries: Vec<ReadQuery>,
) -> Result<w9pt_fs_state::StateSnapshot, MutationPlanError<S::Error, T, P, I>>
where
    S: FilesystemStateStore,
{
    let request = ReadBatch::new(
        grant.filesystem_id(),
        consistency,
        queries,
        state.contract().limits(),
    )
    .map_err(MutationPlanError::MalformedRead)?;
    match state
        .read(request)
        .await
        .map_err(MutationPlanError::State)?
    {
        ReadOutcome::Snapshot(snapshot) => {
            validate_filesystem(&snapshot.results()[0], grant)?;
            Ok(snapshot)
        }
        _ => Err(MutationPlanError::MalformedState),
    }
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

fn writable_open<S, T, P, I>(
    result: &ReadResult,
    open_id: w9pt_fs_state::OpenId,
    client_incarnation: w9pt_fs_state::ClientIncarnationId,
) -> Result<w9pt_fs_state::OpenRecord, MutationPlanError<S, T, P, I>> {
    let ReadResult::Point {
        record: Some(record),
        ..
    } = result
    else {
        return Err(client(LinuxErrno::EBADF));
    };
    let StateRecord::Open(open) = record.as_ref() else {
        return Err(MutationPlanError::MalformedState);
    };
    if open.open_id() != open_id || open.client_incarnation() != client_incarnation {
        return Err(client(LinuxErrno::EBADF));
    }
    if !matches!(open.access(), OpenAccess::WriteOnly | OpenAccess::ReadWrite) {
        return Err(client(if open.access() == OpenAccess::DirectoryRead {
            LinuxErrno::EISDIR
        } else {
            LinuxErrno::EBADF
        }));
    }
    Ok(open.clone())
}

fn point_inode<S, T, P, I>(
    result: &ReadResult,
) -> Result<Option<w9pt_fs_state::InodeRecord>, MutationPlanError<S, T, P, I>> {
    let ReadResult::Point { record, .. } = result else {
        return Err(MutationPlanError::MalformedState);
    };
    match record.as_deref() {
        Some(StateRecord::Inode(inode)) => Ok(Some(inode.clone())),
        None => Ok(None),
        Some(_) => Err(MutationPlanError::MalformedState),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::{convert::Infallible, future::Future};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use w9pt_fs_storage::{BLOCK_SIZE, StorageMethod, testing::block_on};

    use crate::{
        CanonicalIdentity, IdentityMappingRequest, NumericIdentity, ReverseIdentityMappingRequest,
        inode_id_from_handle, operations::execute_read, testing::TestEnvironment,
    };

    #[derive(Clone)]
    struct ChangingPolicy {
        first: ExportGrant,
        second: ExportGrant,
        calls: Arc<AtomicUsize>,
    }

    impl ExportPolicy for ChangingPolicy {
        type Error = Infallible;

        fn resolve(
            &self,
            _request: ExportPolicyRequest,
        ) -> impl Future<Output = Result<ExportGrant, Self::Error>> + Send {
            let grant = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.first.clone()
            } else {
                self.second.clone()
            };
            core::future::ready(Ok(grant))
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
            let grant = self.second.clone();
            core::future::ready(Ok(match request.identity {
                NumericIdentity::User(_) => CanonicalIdentity::Principal(grant.principal().clone()),
                NumericIdentity::Group(_) => {
                    CanonicalIdentity::Group(grant.primary_group().clone())
                }
            }))
        }
    }

    #[test]
    fn sparse_new_and_published_positioned_writes_replay_exact_counts() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::BlockSplit).await;
            let created = environment.create_file("file", 100).await;
            let offset = u64::from(BLOCK_SIZE) * 2 + 5;
            let request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Write {
                    open: created.open,
                    offset,
                    data: b"x".to_vec(),
                },
            );
            let execution = environment.execution(101);
            let first = execute_write(
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
            let replay = execute_write(
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
            assert_eq!(first, FilesystemResult::Written(1));
            assert_eq!(first, replay);
            let gap = execute_read(
                &environment.state,
                &environment.repository,
                &environment.policy,
                environment.engine_limits,
                environment.context(),
                environment.client,
                created.open,
                offset - 2,
                3,
            )
            .await
            .unwrap();
            assert_eq!(gap, [0, 0, b'x']);

            let patch = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Write {
                    open: created.open,
                    offset: 0,
                    data: b"abc".to_vec(),
                },
            );
            assert_eq!(
                execute_write(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    patch,
                    environment.execution(102),
                )
                .await
                .unwrap(),
                FilesystemResult::Written(3)
            );
        });
    }

    #[test]
    fn zero_write_is_retained_and_reused_identity_mismatches() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let created = environment.create_file("zero", 150).await;
            let execution = environment.execution(151);
            let zero = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Write {
                    open: created.open,
                    offset: 9,
                    data: Vec::new(),
                },
            );
            assert_eq!(
                execute_write(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    zero.clone(),
                    execution,
                )
                .await
                .unwrap(),
                FilesystemResult::Written(0)
            );
            assert_eq!(
                execute_write(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    zero,
                    execution,
                )
                .await
                .unwrap(),
                FilesystemResult::Written(0)
            );
            let mismatched = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Write {
                    open: created.open,
                    offset: 9,
                    data: b"x".to_vec(),
                },
            );
            assert!(matches!(
                execute_write(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    mismatched,
                    execution,
                )
                .await,
                Err(MutationOperationError::Internal)
            ));
        });
    }

    #[test]
    fn replanning_rejects_a_grant_different_from_the_fingerprint() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let created = environment.create_file("policy-race", 160).await;
            let first = environment.policy.0.clone();
            let second = ExportGrant::new(
                first.filesystem_id(),
                first.root_inode_id(),
                first.principal().clone(),
                first.primary_group().clone(),
                first.supplementary_groups().to_vec(),
                first.numeric_uid() + 1,
                first.numeric_gid(),
                first.privileged(),
                first.read_only(),
                first.policy_generation(),
                first.capability_ceiling(),
                environment.engine_limits,
            )
            .unwrap();
            let policy = ChangingPolicy {
                first,
                second,
                calls: Arc::new(AtomicUsize::new(0)),
            };
            environment.target.clear_trace().unwrap();
            assert!(matches!(
                execute_write(
                    &environment.state,
                    &environment.repository,
                    &policy,
                    &environment.identities,
                    environment.engine_limits,
                    FilesystemRequest::new(
                        environment.context(),
                        FilesystemOperation::Write {
                            open: created.open,
                            offset: 0,
                            data: b"blocked".to_vec(),
                        },
                    ),
                    environment.execution(161),
                )
                .await,
                Err(MutationOperationError::Client(error)) if error.errno == LinuxErrno::EAGAIN
            ));
            assert!(environment.target.trace().unwrap().is_empty());
        });
    }

    #[test]
    fn append_reselects_authoritative_eof_for_every_write() {
        use w9pt::protocol::OpenFlags;

        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let create = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Create {
                    directory: crate::object_handle(environment.root_id),
                    name: "append".into(),
                    flags: OpenFlags::RDWR | OpenFlags::APPEND,
                    mode: 0o660,
                    gid: 1,
                },
            );
            let FilesystemResult::Created(created) = crate::operations::execute_create(
                &environment.state,
                &environment.repository,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                create,
                environment.execution(200),
            )
            .await
            .unwrap() else {
                panic!("unexpected create result")
            };
            for (mutation, client_offset, byte) in [(201, u64::MAX, b'A'), (202, 0, b'B')] {
                let request = FilesystemRequest::new(
                    environment.context(),
                    FilesystemOperation::Write {
                        open: created.open,
                        offset: client_offset,
                        data: vec![byte],
                    },
                );
                assert_eq!(
                    execute_write(
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
                    FilesystemResult::Written(1)
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
                b"AB"
            );
        });
    }

    #[test]
    fn raw_and_block_split_writes_match_a_sparse_byte_vector_model() {
        block_on(async {
            for (ordinal, method) in [StorageMethod::Raw, StorageMethod::BlockSplit]
                .into_iter()
                .enumerate()
            {
                let environment = TestEnvironment::new(false, method).await;
                let created = environment
                    .create_file("model", 300 + ordinal as u128 * 100)
                    .await;
                let mut model = Vec::new();
                let block = u64::from(BLOCK_SIZE);
                let steps: [(u64, &[u8]); 4] = [
                    (0, b"abc"),
                    (block - 1, b"XY"),
                    (block * 2 + 7, b"sparse"),
                    (3, b"unaligned"),
                ];
                for (step, (offset, bytes)) in steps.into_iter().enumerate() {
                    let start = usize::try_from(offset).unwrap();
                    let end = start.checked_add(bytes.len()).unwrap();
                    if model.len() < end {
                        model.resize(end, 0);
                    }
                    model[start..end].copy_from_slice(bytes);
                    let request = FilesystemRequest::new(
                        environment.context(),
                        FilesystemOperation::Write {
                            open: created.open,
                            offset,
                            data: bytes.to_vec(),
                        },
                    );
                    assert_eq!(
                        execute_write(
                            &environment.state,
                            &environment.repository,
                            &environment.policy,
                            &environment.identities,
                            environment.engine_limits,
                            request,
                            environment.execution(301 + ordinal as u128 * 100 + step as u128),
                        )
                        .await
                        .unwrap(),
                        FilesystemResult::Written(u32::try_from(bytes.len()).unwrap())
                    );
                }
                let actual = execute_read(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    environment.engine_limits,
                    environment.context(),
                    environment.client,
                    created.open,
                    0,
                    u32::try_from(model.len()).unwrap(),
                )
                .await
                .unwrap();
                assert_eq!(actual, model);
                assert!(
                    execute_read(
                        &environment.state,
                        &environment.repository,
                        &environment.policy,
                        environment.engine_limits,
                        environment.context(),
                        environment.client,
                        created.open,
                        u64::try_from(model.len()).unwrap(),
                        1,
                    )
                    .await
                    .unwrap()
                    .is_empty()
                );

                let overflow = FilesystemRequest::new(
                    environment.context(),
                    FilesystemOperation::Write {
                        open: created.open,
                        offset: u64::MAX,
                        data: vec![1],
                    },
                );
                assert!(matches!(
                    execute_write(
                        &environment.state,
                        &environment.repository,
                        &environment.policy,
                        &environment.identities,
                        environment.engine_limits,
                        overflow,
                        environment.execution(399 + ordinal as u128 * 100),
                    )
                    .await,
                    Err(MutationOperationError::Target(
                        w9pt_fs_storage::StorageError::Range(_)
                    ))
                ));
            }
        });
    }

    #[test]
    fn independent_clients_serialize_overlap_disjoint_append_and_truncate() {
        use w9pt::protocol::{OpenFlags, SetAttributes, SetattrMask};
        use w9pt_fs_storage::{ContentRepository, CreationDefaults, StorageLimits};

        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::BlockSplit).await;
            let second_state = environment.authority.open_client();
            let second_repository = ContentRepository::new(
                environment.target.clone(),
                "semantic-engine-tests",
                CreationDefaults::new(StorageMethod::Raw),
                StorageLimits::default(),
            )
            .unwrap();
            let created = environment.create_file("concurrent", 500).await;
            for (state, repository, mutation, offset, data) in [
                (
                    &environment.state,
                    &environment.repository,
                    501,
                    0,
                    b"AAAA".as_slice(),
                ),
                (&second_state, &second_repository, 502, 2, b"bb".as_slice()),
                (
                    &environment.state,
                    &environment.repository,
                    503,
                    8,
                    b"Z".as_slice(),
                ),
            ] {
                let request = FilesystemRequest::new(
                    environment.context(),
                    FilesystemOperation::Write {
                        open: created.open,
                        offset,
                        data: data.to_vec(),
                    },
                );
                execute_write(
                    state,
                    repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    request,
                    environment.execution(mutation),
                )
                .await
                .unwrap();
            }
            let append_request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Open {
                    object: created.object,
                    flags: OpenFlags::RDWR | OpenFlags::APPEND,
                },
            );
            let FilesystemResult::Opened(append) = crate::operations::execute_open(
                &second_state,
                &second_repository,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                append_request,
                environment.execution(504),
            )
            .await
            .unwrap() else {
                panic!("unexpected open result")
            };
            execute_write(
                &second_state,
                &second_repository,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                FilesystemRequest::new(
                    environment.context(),
                    FilesystemOperation::Write {
                        open: append.open,
                        offset: 0,
                        data: b"!".to_vec(),
                    },
                ),
                environment.execution(505),
            )
            .await
            .unwrap();
            crate::operations::execute_setattr(
                &second_state,
                &second_repository,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                FilesystemRequest::new(
                    environment.context(),
                    FilesystemOperation::Setattr {
                        object: created.object,
                        attributes: SetAttributes {
                            valid: SetattrMask::SIZE,
                            size: 5,
                            ..SetAttributes::default()
                        },
                    },
                ),
                environment.execution(506),
            )
            .await
            .unwrap();
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
                    32,
                )
                .await
                .unwrap(),
                b"AAbb\0"
            );
        });
    }

    #[test]
    fn target_and_metadata_failures_recover_through_exact_replay() {
        use w9pt_fs_state::testing::CommitFailureTiming;
        use w9pt_fs_storage::{
            ContentRepository, CreationDefaults, StorageLimits, TargetOperation,
            testing::FailureTiming,
        };

        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let created = environment.create_file("recovery", 700).await;
            let first_request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Write {
                    open: created.open,
                    offset: 0,
                    data: b"old".to_vec(),
                },
            );
            let first_execution = environment.execution(701);
            environment
                .target
                .inject_failure(TargetOperation::PutIfAbsent, FailureTiming::Before)
                .unwrap();
            assert!(matches!(
                execute_write(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    first_request.clone(),
                    first_execution,
                )
                .await,
                Err(MutationOperationError::Target(
                    w9pt_fs_storage::StorageError::Target(_)
                ))
            ));
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
                panic!("inode unavailable")
            };
            assert!(matches!(
                &snapshot.results()[0],
                ReadResult::Point { record: Some(record), .. }
                    if matches!(record.as_ref(), StateRecord::Inode(inode) if inode.content().is_none())
            ));
            assert_eq!(
                execute_write(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    first_request,
                    first_execution,
                )
                .await
                .unwrap(),
                FilesystemResult::Written(3)
            );

            let before_commit_request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Write {
                    open: created.open,
                    offset: 0,
                    data: b"middle".to_vec(),
                },
            );
            let before_commit_execution = environment.execution(702);
            environment
                .authority
                .inject_commit_failure(CommitFailureTiming::BeforePublication)
                .unwrap();
            assert!(matches!(
                execute_write(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    before_commit_request.clone(),
                    before_commit_execution,
                )
                .await,
                Err(MutationOperationError::State(_))
            ));
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
                b"old"
            );
            assert_eq!(
                execute_write(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    before_commit_request,
                    before_commit_execution,
                )
                .await
                .unwrap(),
                FilesystemResult::Written(6)
            );

            let second_request = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Write {
                    open: created.open,
                    offset: 0,
                    data: b"newest".to_vec(),
                },
            );
            let second_execution = environment.execution(703);
            environment
                .authority
                .inject_commit_failure(CommitFailureTiming::AfterPublication)
                .unwrap();
            assert_eq!(
                execute_write(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    second_request.clone(),
                    second_execution,
                )
                .await
                .unwrap(),
                FilesystemResult::Written(6)
            );

            let reopened_state = environment.authority.open_client();
            let reopened_repository = ContentRepository::new(
                environment.target.clone(),
                "semantic-engine-tests",
                CreationDefaults::new(StorageMethod::BlockSplit),
                StorageLimits::default(),
            )
            .unwrap();
            environment.target.clear_trace().unwrap();
            assert_eq!(
                execute_write(
                    &reopened_state,
                    &reopened_repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    second_request,
                    second_execution,
                )
                .await
                .unwrap(),
                FilesystemResult::Written(6)
            );
            assert!(environment.target.trace().unwrap().is_empty());
            assert_eq!(
                execute_read(
                    &reopened_state,
                    &reopened_repository,
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
                b"newest"
            );
        });
    }

    #[test]
    fn payload_and_manifest_response_failures_preserve_exact_recovery() {
        use w9pt_fs_storage::{
            FileContextScope, MutationId, TargetOperation, open_committed_context,
            testing::FailureTiming,
        };

        block_on(async {
            let payload_environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let payload_file = payload_environment
                .create_file("payload-failure", 800)
                .await;
            payload_environment
                .target
                .inject_failure(TargetOperation::PutIfAbsent, FailureTiming::After)
                .unwrap();
            assert_eq!(
                execute_write(
                    &payload_environment.state,
                    &payload_environment.repository,
                    &payload_environment.policy,
                    &payload_environment.identities,
                    payload_environment.engine_limits,
                    FilesystemRequest::new(
                        payload_environment.context(),
                        FilesystemOperation::Write {
                            open: payload_file.open,
                            offset: 0,
                            data: b"payload".to_vec(),
                        },
                    ),
                    payload_environment.execution(801),
                )
                .await
                .unwrap(),
                FilesystemResult::Written(7)
            );

            for (ordinal, timing) in [FailureTiming::Before, FailureTiming::After]
                .into_iter()
                .enumerate()
            {
                let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
                let created = environment
                    .create_file("manifest-failure", 900 + ordinal as u128 * 10)
                    .await;
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
                    panic!("content metadata unavailable")
                };
                let ReadResult::InodeWithContentMetadata {
                    metadata: Some(metadata),
                    ..
                } = &snapshot.results()[0]
                else {
                    panic!("content metadata missing")
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
                let mutation_value = 901 + ordinal as u128 * 10;
                let mutation_id = MutationId::from_u128(mutation_value);
                let prepared = environment
                    .repository
                    .prepare_write_from_new_with_context(&context, mutation_id, 0, 0, b"manifest")
                    .await
                    .unwrap();
                let manifest = prepared.content().manifest_key().clone();
                assert!(environment.target.remove(&manifest).unwrap());
                environment
                    .target
                    .inject_failure_for(TargetOperation::PutIfAbsent, timing, manifest)
                    .unwrap();
                let request = FilesystemRequest::new(
                    environment.context(),
                    FilesystemOperation::Write {
                        open: created.open,
                        offset: 0,
                        data: b"manifest".to_vec(),
                    },
                );
                let execution = environment.execution(mutation_value);
                let result = execute_write(
                    &environment.state,
                    &environment.repository,
                    &environment.policy,
                    &environment.identities,
                    environment.engine_limits,
                    request.clone(),
                    execution,
                )
                .await;
                if timing == FailureTiming::Before {
                    assert!(matches!(
                        result,
                        Err(MutationOperationError::Target(
                            w9pt_fs_storage::StorageError::Target(_)
                        ))
                    ));
                    assert_eq!(
                        execute_write(
                            &environment.state,
                            &environment.repository,
                            &environment.policy,
                            &environment.identities,
                            environment.engine_limits,
                            request,
                            execution,
                        )
                        .await
                        .unwrap(),
                        FilesystemResult::Written(8)
                    );
                } else {
                    assert_eq!(result.unwrap(), FilesystemResult::Written(8));
                }
            }
        });
    }
}
