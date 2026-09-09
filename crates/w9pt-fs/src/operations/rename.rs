//! Atomic directory-relative rename with cookie transfer and replacement handling.

use w9pt::{
    LinuxErrno,
    filesystem::{FilesystemOperation, FilesystemRequest, FilesystemResult},
};
use w9pt_fs_state::{
    CommitRequest, DirectoryCookie, DirectoryEntryRecord, EntryName, FilesystemStateStore,
    InodeData, InodeKind, InodeRecord, MutationContext, OrphanRecord, Precondition, ReadBatch,
    ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision, ScanBounds,
    StateChange, StateRecord,
};
use w9pt_fs_storage::TargetStore;

use crate::{
    EngineLimits, ExecutionContext, ExportGrant, ExportPolicy, ExportPolicyRequest, IdentitySource,
    authorization_client_error, check_directory_mutation, check_sticky_directory,
    encode_mutation_result, inode_id_from_handle, mutation_fingerprint, run_mutation,
};

use super::mutation::{
    MutationOperationError, MutationPlanError, client, fingerprint_error, runner_error,
};

pub(crate) async fn execute_rename<S, T, P, I>(
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
    let (old_directory, old_name, new_directory, new_name) = match &request.operation {
        FilesystemOperation::RenameAt {
            old_directory,
            old_name,
            new_directory,
            new_name,
        } => (
            *old_directory,
            old_name.clone(),
            *new_directory,
            new_name.clone(),
        ),
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
        w9pt::filesystem::FilesystemResultKind::RenamedAt,
        limits,
        |_| {
            let context = context.clone();
            let old_name = old_name.clone();
            let new_name = new_name.clone();
            async move {
                let grant = policy
                    .resolve(ExportPolicyRequest::new(context))
                    .await
                    .map_err(MutationPlanError::Policy)?;
                if grant.filesystem_id() != filesystem_id || grant.root_inode_id() != root_inode_id
                {
                    return Err(client(LinuxErrno::EAGAIN));
                }
                plan_rename::<S, T::Error, P::Error, I::Error>(
                    state,
                    &grant,
                    mutation,
                    fence,
                    now,
                    old_directory,
                    old_name,
                    new_directory,
                    new_name,
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
async fn plan_rename<S, T, P, I>(
    state: &S,
    grant: &ExportGrant,
    mutation: MutationContext,
    fence: w9pt_fs_state::WriterFence,
    now: w9pt_fs_state::UnixTimestamp,
    old_directory: w9pt::filesystem::ObjectHandle,
    old_name: String,
    new_directory: w9pt::filesystem::ObjectHandle,
    new_name: String,
    limits: EngineLimits,
) -> Result<CommitRequest, MutationPlanError<S::Error, T, P, I>>
where
    S: FilesystemStateStore,
{
    let state_limits = state.contract().limits();
    if old_name.len() > state_limits.max_entry_name_bytes()
        || new_name.len() > state_limits.max_entry_name_bytes()
    {
        return Err(client(LinuxErrno::ENAMETOOLONG));
    }
    let old_name = EntryName::new(old_name.into_bytes(), state_limits)
        .map_err(|_| client(LinuxErrno::EINVAL))?;
    let new_name = EntryName::new(new_name.into_bytes(), state_limits)
        .map_err(|_| client(LinuxErrno::EINVAL))?;
    let old_parent_id = inode_id_from_handle(old_directory);
    let new_parent_id = inode_id_from_handle(new_directory);
    let first = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::LatestLinearizable,
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Inode(old_parent_id),
            ReadQuery::Inode(new_parent_id),
            ReadQuery::DirectoryEntry {
                parent_inode_id: old_parent_id,
                name: old_name.clone(),
            },
            ReadQuery::DirectoryEntry {
                parent_inode_id: new_parent_id,
                name: new_name.clone(),
            },
        ],
        state_limits,
    )
    .map_err(MutationPlanError::MalformedRead)?;
    let ReadOutcome::Snapshot(first) = state.read(first).await.map_err(MutationPlanError::State)?
    else {
        return Err(MutationPlanError::MalformedState);
    };
    validate_filesystem(&first.results()[0], grant)?;
    let source_entry =
        directory_entry(&first.results()[3])?.ok_or_else(|| client(LinuxErrno::ENOENT))?;
    let destination_entry = directory_entry(&first.results()[4])?;
    let source_id = source_entry.child_inode_id();
    let destination_id = destination_entry
        .as_ref()
        .map(DirectoryEntryRecord::child_inode_id);
    let mut queries = vec![
        ReadQuery::Filesystem,
        ReadQuery::Inode(old_parent_id),
        ReadQuery::Inode(new_parent_id),
        ReadQuery::DirectoryEntry {
            parent_inode_id: old_parent_id,
            name: old_name.clone(),
        },
        ReadQuery::DirectoryEntry {
            parent_inode_id: new_parent_id,
            name: new_name.clone(),
        },
        ReadQuery::Inode(source_id),
    ];
    if let Some(destination_id) = destination_id {
        queries.extend([
            ReadQuery::Inode(destination_id),
            ReadQuery::OpenPinCount(destination_id),
            ReadQuery::Orphan(destination_id),
        ]);
    }
    let read = ReadBatch::new(
        grant.filesystem_id(),
        ReadConsistency::AtLeast(first.revision()),
        queries,
        state_limits,
    )
    .map_err(MutationPlanError::MalformedRead)?;
    let ReadOutcome::Snapshot(snapshot) =
        state.read(read).await.map_err(MutationPlanError::State)?
    else {
        return Err(MutationPlanError::MalformedState);
    };
    validate_filesystem(&snapshot.results()[0], grant)?;
    let old_parent =
        inode_record(&snapshot.results()[1])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    let new_parent =
        inode_record(&snapshot.results()[2])?.ok_or_else(|| client(LinuxErrno::EBADF))?;
    let source_entry =
        directory_entry(&snapshot.results()[3])?.ok_or_else(|| client(LinuxErrno::ENOENT))?;
    if source_entry.child_inode_id() != source_id {
        return Err(client(LinuxErrno::EAGAIN));
    }
    let current_destination = directory_entry(&snapshot.results()[4])?;
    if current_destination
        .as_ref()
        .map(DirectoryEntryRecord::child_inode_id)
        != destination_id
    {
        return Err(client(LinuxErrno::EAGAIN));
    }
    let source = inode_record(&snapshot.results()[5])?.ok_or(MutationPlanError::MalformedState)?;
    check_directory_mutation(grant, &old_parent)
        .and_then(|()| check_directory_mutation(grant, &new_parent))
        .and_then(|()| check_sticky_directory(grant, &old_parent, &source))
        .map_err(authorization_client_error)
        .map_err(MutationPlanError::Client)?;
    let fs = grant.filesystem_id();
    let terminal = encode_mutation_result(&FilesystemResult::RenamedAt, limits, state_limits)
        .map_err(MutationPlanError::ResultCodec)?;
    if old_parent_id == new_parent_id && old_name == new_name || destination_id == Some(source_id) {
        return CommitRequest::new(
            fs,
            mutation,
            fence,
            vec![
                Precondition::FilesystemPolicyGeneration {
                    expected: grant.policy_generation(),
                },
                Precondition::RecordRevision {
                    key: RecordKey::DirectoryEntry(fs, old_parent_id, old_name),
                    expected: source_entry.revision(),
                },
            ],
            vec![StateChange::RetainResult],
            terminal,
            state_limits,
        )
        .map_err(MutationPlanError::MalformedCommit);
    }
    let ancestor_conditions = if source.kind() == InodeKind::Directory {
        ancestor_preconditions::<S, T, P, I>(state, grant, new_parent_id, source_id, limits).await?
    } else {
        Vec::new()
    };
    let mut destination = None;
    let mut pin_count = 0u64;
    let mut destination_orphan = None;
    if let Some(destination_id) = destination_id {
        destination =
            Some(inode_record(&snapshot.results()[6])?.ok_or(MutationPlanError::MalformedState)?);
        let ReadResult::OpenPinCount { inode_id, count } = &snapshot.results()[7] else {
            return Err(MutationPlanError::MalformedState);
        };
        if *inode_id != destination_id {
            return Err(MutationPlanError::MalformedState);
        }
        pin_count = *count;
        destination_orphan = orphan_record(&snapshot.results()[8])?;
    }
    if let Some(destination) = &destination {
        if (source.kind() == InodeKind::Directory) != (destination.kind() == InodeKind::Directory) {
            return Err(client(if source.kind() == InodeKind::Directory {
                LinuxErrno::ENOTDIR
            } else {
                LinuxErrno::EISDIR
            }));
        }
        check_sticky_directory(grant, &new_parent, destination)
            .map_err(authorization_client_error)
            .map_err(MutationPlanError::Client)?;
        if destination.kind() == InodeKind::Directory {
            if pin_count != 0 {
                return Err(client(LinuxErrno::EBUSY));
            }
            let page = ReadBatch::new(
                fs,
                ReadConsistency::LatestLinearizable,
                vec![ReadQuery::DirectoryPage {
                    parent_inode_id: destination.inode_id(),
                    after: DirectoryCookie::START,
                    bounds: ScanBounds::new(1, state_limits.max_scan_bytes(), state_limits)
                        .map_err(|_| MutationPlanError::MalformedState)?,
                }],
                state_limits,
            )
            .map_err(MutationPlanError::MalformedRead)?;
            let ReadOutcome::Snapshot(page) =
                state.read(page).await.map_err(MutationPlanError::State)?
            else {
                return Err(MutationPlanError::MalformedState);
            };
            let ReadResult::DirectoryPage(page) = &page.results()[0] else {
                return Err(MutationPlanError::MalformedState);
            };
            if !page.entries().is_empty() {
                return Err(client(LinuxErrno::ENOTEMPTY));
            }
        }
        if destination_orphan.is_some() || destination.link_count() == 0 {
            return Err(MutationPlanError::MalformedState);
        }
    }
    let moved = DirectoryEntryRecord::new(
        new_parent_id,
        new_name.clone(),
        source_entry.cookie(),
        source_id,
        RecordRevision::new(1).expect("one is a valid placeholder revision"),
    )
    .map_err(|_| MutationPlanError::MalformedState)?;
    let mut changes = vec![StateChange::MoveDirectoryEntry {
        source_parent_inode_id: old_parent_id,
        source_name: old_name.clone(),
        destination: moved,
    }];
    if old_parent_id == new_parent_id {
        changes.push(StateChange::Replace {
            key: RecordKey::Inode(fs, old_parent_id),
            record: StateRecord::Inode(updated_parent(&old_parent, now)?),
        });
    } else {
        changes.push(StateChange::Replace {
            key: RecordKey::Inode(fs, old_parent_id),
            record: StateRecord::Inode(updated_parent(&old_parent, now)?),
        });
        changes.push(StateChange::Replace {
            key: RecordKey::Inode(fs, new_parent_id),
            record: StateRecord::Inode(updated_parent(&new_parent, now)?),
        });
        if source.kind() == InodeKind::Directory {
            changes.push(StateChange::Replace {
                key: RecordKey::Inode(fs, source_id),
                record: StateRecord::Inode(moved_directory(&source, new_parent_id, now)?),
            });
        }
    }
    if let Some(destination) = &destination {
        let links = destination
            .link_count()
            .checked_sub(1)
            .ok_or_else(|| client(LinuxErrno::EOVERFLOW))?;
        if links > 0 {
            changes.push(StateChange::Replace {
                key: RecordKey::Inode(fs, destination.inode_id()),
                record: StateRecord::Inode(updated_target(destination, links, now)?),
            });
        } else if pin_count > 0 {
            changes.push(StateChange::Replace {
                key: RecordKey::Inode(fs, destination.inode_id()),
                record: StateRecord::Inode(updated_target(destination, 0, now)?),
            });
            changes.push(StateChange::Insert {
                key: RecordKey::Orphan(fs, destination.inode_id()),
                record: StateRecord::Orphan(
                    OrphanRecord::new(
                        destination.inode_id(),
                        pin_count,
                        snapshot.revision(),
                        RecordRevision::new(1).expect("one is a valid placeholder revision"),
                    )
                    .map_err(|_| MutationPlanError::MalformedState)?,
                ),
            });
        } else {
            changes.push(StateChange::Delete(RecordKey::Inode(
                fs,
                destination.inode_id(),
            )));
        }
    }
    let mut preconditions = vec![
        Precondition::FilesystemPolicyGeneration {
            expected: grant.policy_generation(),
        },
        Precondition::RecordRevision {
            key: RecordKey::Inode(fs, old_parent_id),
            expected: old_parent.revision(),
        },
        Precondition::DirectoryGeneration {
            inode_id: old_parent_id,
            expected: old_parent
                .directory_generation()
                .ok_or(MutationPlanError::MalformedState)?,
        },
        Precondition::RecordRevision {
            key: RecordKey::DirectoryEntry(fs, old_parent_id, old_name),
            expected: source_entry.revision(),
        },
        Precondition::RecordRevision {
            key: RecordKey::Inode(fs, source_id),
            expected: source.revision(),
        },
    ];
    preconditions.extend(ancestor_conditions);
    if old_parent_id != new_parent_id {
        preconditions.extend([
            Precondition::RecordRevision {
                key: RecordKey::Inode(fs, new_parent_id),
                expected: new_parent.revision(),
            },
            Precondition::DirectoryGeneration {
                inode_id: new_parent_id,
                expected: new_parent
                    .directory_generation()
                    .ok_or(MutationPlanError::MalformedState)?,
            },
        ]);
    }
    match (&current_destination, &destination) {
        (Some(entry), Some(inode)) => {
            preconditions.extend([
                Precondition::RecordRevision {
                    key: RecordKey::DirectoryEntry(fs, new_parent_id, new_name),
                    expected: entry.revision(),
                },
                Precondition::RecordRevision {
                    key: RecordKey::Inode(fs, inode.inode_id()),
                    expected: inode.revision(),
                },
                Precondition::LinkCount {
                    inode_id: inode.inode_id(),
                    expected: inode.link_count(),
                },
                Precondition::OpenPinCount {
                    inode_id: inode.inode_id(),
                    expected: pin_count,
                },
                Precondition::RecordAbsent(RecordKey::Orphan(fs, inode.inode_id())),
            ]);
        }
        (None, None) => preconditions.push(Precondition::RecordAbsent(RecordKey::DirectoryEntry(
            fs,
            new_parent_id,
            new_name,
        ))),
        _ => return Err(MutationPlanError::MalformedState),
    }
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

async fn ancestor_preconditions<S, T, P, I>(
    state: &S,
    grant: &ExportGrant,
    mut current: w9pt_fs_state::InodeId,
    moved: w9pt_fs_state::InodeId,
    limits: EngineLimits,
) -> Result<Vec<Precondition>, MutationPlanError<S::Error, T, P, I>>
where
    S: FilesystemStateStore,
{
    let mut result = Vec::new();
    for depth in 0..=limits.max_ancestor_depth() {
        if current == moved {
            return Err(client(LinuxErrno::ELOOP));
        }
        if current == grant.root_inode_id() {
            return Ok(result);
        }
        if depth == limits.max_ancestor_depth() {
            return Err(client(LinuxErrno::ELOOP));
        }
        let request = ReadBatch::new(
            grant.filesystem_id(),
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Inode(current)],
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
        let inode =
            inode_record(&snapshot.results()[0])?.ok_or(MutationPlanError::MalformedState)?;
        if inode.kind() != InodeKind::Directory {
            return Err(MutationPlanError::MalformedState);
        }
        result.push(Precondition::RecordRevision {
            key: RecordKey::Inode(grant.filesystem_id(), current),
            expected: inode.revision(),
        });
        current = inode
            .directory_parent()
            .ok_or(MutationPlanError::MalformedState)?;
    }
    Err(client(LinuxErrno::ELOOP))
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
fn inode_record<S, T, P, I>(
    result: &ReadResult,
) -> Result<Option<InodeRecord>, MutationPlanError<S, T, P, I>> {
    point(result, |r| match r {
        StateRecord::Inode(v) => Some(v.clone()),
        _ => None,
    })
}
fn directory_entry<S, T, P, I>(
    result: &ReadResult,
) -> Result<Option<DirectoryEntryRecord>, MutationPlanError<S, T, P, I>> {
    point(result, |r| match r {
        StateRecord::DirectoryEntry(v) => Some(v.clone()),
        _ => None,
    })
}
fn orphan_record<S, T, P, I>(
    result: &ReadResult,
) -> Result<Option<OrphanRecord>, MutationPlanError<S, T, P, I>> {
    point(result, |r| match r {
        StateRecord::Orphan(v) => Some(v.clone()),
        _ => None,
    })
}
fn point<S, T, P, I, R>(
    result: &ReadResult,
    convert: impl FnOnce(&StateRecord) -> Option<R>,
) -> Result<Option<R>, MutationPlanError<S, T, P, I>> {
    let ReadResult::Point { record, .. } = result else {
        return Err(MutationPlanError::MalformedState);
    };
    match record.as_deref() {
        Some(r) => convert(r)
            .map(Some)
            .ok_or(MutationPlanError::MalformedState),
        None => Ok(None),
    }
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
    let parent = inode
        .directory_parent()
        .ok_or(MutationPlanError::MalformedState)?;
    rebuild(
        inode,
        inode.link_count(),
        now,
        InodeData::Directory {
            generation,
            parent_inode_id: parent,
        },
    )
}
fn moved_directory<S, T, P, I>(
    inode: &InodeRecord,
    parent: w9pt_fs_state::InodeId,
    now: w9pt_fs_state::UnixTimestamp,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    let generation = inode
        .directory_generation()
        .ok_or(MutationPlanError::MalformedState)?;
    rebuild(
        inode,
        inode.link_count(),
        now,
        InodeData::Directory {
            generation,
            parent_inode_id: parent,
        },
    )
}
fn updated_target<S, T, P, I>(
    inode: &InodeRecord,
    links: u64,
    now: w9pt_fs_state::UnixTimestamp,
) -> Result<InodeRecord, MutationPlanError<S, T, P, I>> {
    rebuild(inode, links, now, inode.data().clone())
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
    use crate::{operations::execute_mkdir, testing::TestEnvironment};
    use core::convert::Infallible;
    use w9pt_fs_storage::{StorageMethod, testing::block_on};
    #[test]
    fn replacement_preserves_source_cookie_and_orphans_open_destination() {
        block_on(async {
            let e = TestEnvironment::new(false, StorageMethod::Raw).await;
            let source = e.create_file("source", 100).await;
            let replaced = e.create_file("replaced", 101).await;
            let name = EntryName::new(b"source".to_vec(), e.state_limits).unwrap();
            let read = ReadBatch::new(
                e.filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![ReadQuery::DirectoryEntry {
                    parent_inode_id: e.root_id,
                    name,
                }],
                e.state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(before) = e.state.read(read).await.unwrap() else {
                panic!()
            };
            let cookie = directory_entry::<Infallible, Infallible, Infallible, Infallible>(
                &before.results()[0],
            )
            .unwrap()
            .unwrap()
            .cookie();
            let request = FilesystemRequest::new(
                e.context(),
                FilesystemOperation::RenameAt {
                    old_directory: crate::object_handle(e.root_id),
                    old_name: "source".into(),
                    new_directory: crate::object_handle(e.root_id),
                    new_name: "replaced".into(),
                },
            );
            assert_eq!(
                execute_rename::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                    &e.state,
                    &e.policy,
                    &e.identities,
                    e.engine_limits,
                    request,
                    e.execution(102)
                )
                .await
                .unwrap(),
                FilesystemResult::RenamedAt
            );
            let new_name = EntryName::new(b"replaced".to_vec(), e.state_limits).unwrap();
            let old_name = EntryName::new(b"source".to_vec(), e.state_limits).unwrap();
            let read = ReadBatch::new(
                e.filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![
                    ReadQuery::DirectoryEntry {
                        parent_inode_id: e.root_id,
                        name: old_name,
                    },
                    ReadQuery::DirectoryEntry {
                        parent_inode_id: e.root_id,
                        name: new_name,
                    },
                    ReadQuery::Orphan(inode_id_from_handle(replaced.object)),
                ],
                e.state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(after) = e.state.read(read).await.unwrap() else {
                panic!()
            };
            assert!(matches!(
                &after.results()[0],
                ReadResult::Point { record: None, .. }
            ));
            assert_eq!(
                directory_entry::<Infallible, Infallible, Infallible, Infallible>(
                    &after.results()[1],
                )
                .unwrap()
                .unwrap()
                .cookie(),
                cookie
            );
            assert_eq!(
                directory_entry::<Infallible, Infallible, Infallible, Infallible>(
                    &after.results()[1],
                )
                .unwrap()
                .unwrap()
                .child_inode_id(),
                inode_id_from_handle(source.object)
            );
            assert!(matches!(
                &after.results()[2],
                ReadResult::Point {
                    record: Some(_),
                    ..
                }
            ));
        });
    }

    #[test]
    fn directory_cycle_is_rejected_and_cross_parent_move_updates_ancestry() {
        block_on(async {
            let e = TestEnvironment::new(false, StorageMethod::Raw).await;
            let mkdir = |directory, name: &str| {
                FilesystemRequest::new(
                    e.context(),
                    FilesystemOperation::Mkdir {
                        directory,
                        name: name.into(),
                        mode: 0o750,
                        gid: 1,
                    },
                )
            };
            let FilesystemResult::DirectoryCreated(a) =
                execute_mkdir::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                    &e.state,
                    &e.policy,
                    &e.identities,
                    e.engine_limits,
                    mkdir(crate::object_handle(e.root_id), "a"),
                    e.execution(200),
                )
                .await
                .unwrap()
            else {
                panic!("unexpected mkdir result")
            };
            let FilesystemResult::DirectoryCreated(b) =
                execute_mkdir::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                    &e.state,
                    &e.policy,
                    &e.identities,
                    e.engine_limits,
                    mkdir(a.object, "b"),
                    e.execution(201),
                )
                .await
                .unwrap()
            else {
                panic!("unexpected mkdir result")
            };
            let cycle = FilesystemRequest::new(
                e.context(),
                FilesystemOperation::RenameAt {
                    old_directory: crate::object_handle(e.root_id),
                    old_name: "a".into(),
                    new_directory: b.object,
                    new_name: "a".into(),
                },
            );
            assert!(matches!(
                execute_rename::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                    &e.state,
                    &e.policy,
                    &e.identities,
                    e.engine_limits,
                    cycle,
                    e.execution(202),
                )
                .await,
                Err(MutationOperationError::Client(error)) if error.errno == LinuxErrno::ELOOP
            ));

            let move_b = FilesystemRequest::new(
                e.context(),
                FilesystemOperation::RenameAt {
                    old_directory: a.object,
                    old_name: "b".into(),
                    new_directory: crate::object_handle(e.root_id),
                    new_name: "b".into(),
                },
            );
            assert_eq!(
                execute_rename::<_, w9pt_fs_storage::testing::MemoryTarget, _, _>(
                    &e.state,
                    &e.policy,
                    &e.identities,
                    e.engine_limits,
                    move_b,
                    e.execution(203),
                )
                .await
                .unwrap(),
                FilesystemResult::RenamedAt
            );
            let b_id = inode_id_from_handle(b.object);
            let read = ReadBatch::new(
                e.filesystem_id,
                ReadConsistency::LatestLinearizable,
                vec![ReadQuery::Inode(b_id)],
                e.state_limits,
            )
            .unwrap();
            let ReadOutcome::Snapshot(snapshot) = e.state.read(read).await.unwrap() else {
                panic!("moved directory unavailable")
            };
            assert!(matches!(
                &snapshot.results()[0],
                ReadResult::Point { record: Some(record), .. }
                    if matches!(record.as_ref(), StateRecord::Inode(inode)
                        if inode.directory_parent() == Some(e.root_id))
            ));
        });
    }
}
