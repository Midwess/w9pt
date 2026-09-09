//! Portable open release and open-unlinked retirement.

use w9pt::{
    LinuxErrno,
    filesystem::{FilesystemOperation, FilesystemRequest, FilesystemResult},
};
use w9pt_fs_state::{
    CommitRequest, CounterAdjustment, FilesystemStateStore, MutationContext, Precondition,
    ReadBatch, ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey, StateChange,
    StateRecord,
};
use w9pt_fs_storage::TargetStore;

use crate::{
    EngineLimits, ExecutionContext, ExportGrant, ExportPolicy, ExportPolicyRequest, IdentitySource,
    encode_mutation_result, inode_id_from_handle, mutation_fingerprint, open_id_from_handle,
    run_mutation,
};

use super::mutation::{
    MutationOperationError, MutationPlanError, client, fingerprint_error, runner_error,
};

pub(crate) async fn execute_release<S, T, P, I>(
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
    let (object, open, xattr) = match request.operation {
        FilesystemOperation::Release {
            object,
            open,
            xattr,
        } => (object, open, xattr),
        _ => return Err(MutationOperationError::Internal),
    };
    if xattr.is_some() {
        return Err(MutationOperationError::Client(w9pt::FilesystemError::new(
            LinuxErrno::EOPNOTSUPP,
        )));
    }
    let Some(open) = open else {
        return Ok(FilesystemResult::Released);
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
        w9pt::filesystem::FilesystemResultKind::Released,
        limits,
        |_| {
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
                plan_release::<S, T::Error, P::Error, I::Error>(
                    state, &grant, execution, mutation, fence, object, open, limits,
                )
                .await
            }
        },
    )
    .await
    .map_err(runner_error)
}

#[allow(clippy::too_many_arguments)]
async fn plan_release<S, T, P, I>(
    state: &S,
    grant: &ExportGrant,
    execution: ExecutionContext,
    mutation: MutationContext,
    fence: w9pt_fs_state::WriterFence,
    object: w9pt::filesystem::ObjectHandle,
    open: w9pt::filesystem::OpenHandle,
    limits: EngineLimits,
) -> Result<CommitRequest, MutationPlanError<S::Error, T, P, I>>
where
    S: FilesystemStateStore,
{
    let state_limits = state.contract().limits();
    let open_id = open_id_from_handle(open);
    let first = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Filesystem, ReadQuery::Open(open_id)],
        state_limits,
    )
    .map_err(MutationPlanError::MalformedRead)?;
    let ReadOutcome::Snapshot(first) = state.read(first).await.map_err(MutationPlanError::State)?
    else {
        return Err(MutationPlanError::MalformedState);
    };
    validate_filesystem(&first.results()[0], grant)?;
    let open_record = point_open(&first.results()[1])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    if open_record.client_incarnation() != execution.client_incarnation
        || open_record.inode_id() != inode_id_from_handle(object)
    {
        return Err(client(LinuxErrno::EBADF));
    }
    let inode_id = open_record.inode_id();
    let read = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::AtLeast(first.revision()),
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Open(open_id),
            ReadQuery::Inode(inode_id),
            ReadQuery::OpenPin { inode_id, open_id },
            ReadQuery::OpenPinCount(inode_id),
            ReadQuery::Orphan(inode_id),
        ],
        state_limits,
    )
    .map_err(MutationPlanError::MalformedRead)?;
    let ReadOutcome::Snapshot(snapshot) =
        state.read(read).await.map_err(MutationPlanError::State)?
    else {
        return Err(MutationPlanError::MalformedState);
    };
    validate_filesystem(&snapshot.results()[0], grant)?;
    let current_open =
        point_open(&snapshot.results()[1])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    if current_open.open_id() != open_id
        || current_open.inode_id() != inode_id
        || current_open.client_incarnation() != execution.client_incarnation
    {
        return Err(client(LinuxErrno::EBADF));
    }
    let inode = point_record(&snapshot.results()[2], |record| match record {
        StateRecord::Inode(value) => Some(value.clone()),
        _ => None,
    })?
    .ok_or(MutationPlanError::MalformedState)?;
    let pin = point_record(&snapshot.results()[3], |record| match record {
        StateRecord::OpenPin(value) => Some(value.clone()),
        _ => None,
    })?
    .ok_or(MutationPlanError::MalformedState)?;
    if pin.inode_id() != inode_id || pin.open_id() != open_id {
        return Err(MutationPlanError::MalformedState);
    }
    let ReadResult::OpenPinCount {
        inode_id: counted_inode,
        count,
    } = &snapshot.results()[4]
    else {
        return Err(MutationPlanError::MalformedState);
    };
    if *counted_inode != inode_id || *count == 0 {
        return Err(MutationPlanError::MalformedState);
    }
    let orphan = point_record(&snapshot.results()[5], |record| match record {
        StateRecord::Orphan(value) => Some(value.clone()),
        _ => None,
    })?;
    let fs = grant.filesystem_id();
    let mut preconditions = vec![
        Precondition::FilesystemPolicyGeneration {
            expected: grant.policy_generation(),
        },
        Precondition::RecordRevision {
            key: RecordKey::Open(fs, open_id),
            expected: current_open.revision(),
        },
        Precondition::RecordRevision {
            key: RecordKey::OpenPin(fs, inode_id, open_id),
            expected: pin.revision(),
        },
        Precondition::RecordRevision {
            key: RecordKey::Inode(fs, inode_id),
            expected: inode.revision(),
        },
        Precondition::OpenPinCount {
            inode_id,
            expected: *count,
        },
    ];
    let mut changes = vec![
        StateChange::Delete(RecordKey::Open(fs, open_id)),
        StateChange::Delete(RecordKey::OpenPin(fs, inode_id, open_id)),
    ];
    match (inode.link_count(), orphan) {
        (links, None) if links > 0 => {}
        (0, Some(orphan)) if *count > 1 => {
            if orphan.open_pin_count() != *count {
                return Err(MutationPlanError::MalformedState);
            }
            preconditions.push(Precondition::RecordRevision {
                key: RecordKey::Orphan(fs, inode_id),
                expected: orphan.revision(),
            });
            changes.push(StateChange::AdjustOpenPinCount {
                inode_id,
                adjustment: CounterAdjustment::decrease(1).expect("one is a nonzero adjustment"),
            });
        }
        (0, Some(orphan)) if *count == 1 => {
            if orphan.open_pin_count() != 1 {
                return Err(MutationPlanError::MalformedState);
            }
            preconditions.push(Precondition::RecordRevision {
                key: RecordKey::Orphan(fs, inode_id),
                expected: orphan.revision(),
            });
            changes.push(StateChange::Delete(RecordKey::Orphan(fs, inode_id)));
            changes.push(StateChange::Delete(RecordKey::Inode(fs, inode_id)));
        }
        _ => return Err(MutationPlanError::MalformedState),
    }
    let terminal = encode_mutation_result(&FilesystemResult::Released, limits, state_limits)
        .map_err(MutationPlanError::ResultCodec)?;
    CommitRequest::new(
        fs,
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

fn point_open<S, T, P, I>(
    result: &ReadResult,
) -> Result<Option<w9pt_fs_state::OpenRecord>, MutationPlanError<S, T, P, I>> {
    point_record(result, |record| match record {
        StateRecord::Open(value) => Some(value.clone()),
        _ => None,
    })
}

fn point_record<S, T, P, I, R>(
    result: &ReadResult,
    convert: impl FnOnce(&StateRecord) -> Option<R>,
) -> Result<Option<R>, MutationPlanError<S, T, P, I>> {
    let ReadResult::Point { record, .. } = result else {
        return Err(MutationPlanError::MalformedState);
    };
    match record.as_deref() {
        Some(record) => convert(record)
            .map(Some)
            .ok_or(MutationPlanError::MalformedState),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt::protocol::OpenFlags;
    use w9pt_fs_storage::{StorageMethod, testing::block_on};

    use crate::{object_handle, operations::execute_create, testing::TestEnvironment};

    #[test]
    fn release_removes_owned_open_and_pin_and_replays() {
        block_on(async {
            let environment = TestEnvironment::new(false, StorageMethod::Raw).await;
            let create = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Create {
                    directory: object_handle(environment.root_id),
                    name: "file".into(),
                    flags: OpenFlags::RDWR,
                    mode: 0o660,
                    gid: 1,
                },
            );
            let FilesystemResult::Created(created) = execute_create(
                &environment.state,
                &environment.repository,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                create,
                environment.execution(100),
            )
            .await
            .unwrap() else {
                panic!("unexpected create result")
            };
            let release = FilesystemRequest::new(
                environment.context(),
                FilesystemOperation::Release {
                    object: created.object,
                    open: Some(created.open),
                    xattr: None,
                },
            );
            let execution = environment.execution(101);
            let first = execute_release::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                &environment.state,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                release.clone(),
                execution,
            )
            .await
            .unwrap();
            let replay = execute_release::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                &environment.state,
                &environment.policy,
                &environment.identities,
                environment.engine_limits,
                release,
                execution,
            )
            .await
            .unwrap();
            assert_eq!(first, FilesystemResult::Released);
            assert_eq!(first, replay);
            let inode_id = inode_id_from_handle(created.object);
            let read = ReadBatch::new(
                environment.filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![
                    ReadQuery::Open(open_id_from_handle(created.open)),
                    ReadQuery::OpenPinCount(inode_id),
                    ReadQuery::Inode(inode_id),
                ],
                environment.state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(snapshot) = environment.state.read(read).await.unwrap()
            else {
                panic!("release state unavailable")
            };
            assert!(matches!(
                &snapshot.results()[0],
                ReadResult::Point { record: None, .. }
            ));
            assert!(matches!(
                &snapshot.results()[1],
                ReadResult::OpenPinCount { count: 0, .. }
            ));
            assert!(matches!(
                &snapshot.results()[2],
                ReadResult::Point {
                    record: Some(_),
                    ..
                }
            ));
        });
    }
}
