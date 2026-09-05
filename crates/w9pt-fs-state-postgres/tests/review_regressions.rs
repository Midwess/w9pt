//! Live regressions for edge cases found during code review.

mod common;

use core::num::NonZeroU64;

use w9pt_fs_state::{
    AcquireLeaseOutcome, AcquireWriterLease, ChangeCursor, ChangePoll, ChangePollOutcome,
    ClientIncarnationId, CommitConflictKind, CommitOutcome, CommitRequest, DirectoryCookie,
    DirectoryEntryRecord, DirectoryGeneration, EntryName, FilesystemId, FilesystemRecord,
    FilesystemStateStore, GroupId, InodeData, InodeGeneration, InodeId, InodeRecord, InodeTimes,
    LeaseDuration, LeaseId, LeaseOperationId, LockGeneration, LockId, LockKind, LockOwner,
    LockRange, LockRecord, MalformedCommit, MutationContext, MutationResult, MutationResultKind,
    MutationRetention, OpenAccess, OpenId, OpenRecord, PrincipalId, QidPath, ReadBatch,
    ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision,
    RecordValidationError, RequestFingerprint, ResultFormatVersion, StateChange, StateLimitValues,
    StateLimits, StateRecord, StateRevision, UnixTimestamp, WriterFence, WriterIncarnationId,
    WriterScopeId,
};
use w9pt_fs_state_postgres::{PostgresStateConfig, PostgresStateStore};
use w9pt_fs_storage::MutationId;

use common::{cleanup_filesystems, connect, live_dsn};

fn times() -> InodeTimes {
    let value = UnixTimestamp::new(1, 0).expect("valid timestamp");
    InodeTimes {
        accessed: value,
        modified: value,
        changed: value,
        created: value,
    }
}

fn inode(
    inode_id: InodeId,
    qid_path: QidPath,
    data: InodeData,
    links: u64,
    limits: StateLimits,
) -> StateRecord {
    StateRecord::Inode(
        InodeRecord::new(
            inode_id,
            qid_path,
            RecordRevision::new(1).expect("one is nonzero"),
            0o755,
            PrincipalId::new(b"owner".to_vec(), limits).expect("bounded owner"),
            GroupId::new(b"group".to_vec(), limits).expect("bounded group"),
            times(),
            0,
            links,
            InodeGeneration::new(1).expect("one is nonzero"),
            data,
        )
        .expect("valid test inode"),
    )
}

fn result(limits: StateLimits) -> MutationResult {
    MutationResult::new(
        MutationResultKind::new(1).expect("nonzero result kind"),
        ResultFormatVersion::new(1).expect("nonzero result format"),
        b"ok".to_vec(),
        limits,
    )
    .expect("bounded result")
}

fn request(
    filesystem_id: FilesystemId,
    mutation: u128,
    fence: WriterFence,
    changes: Vec<StateChange>,
    limits: StateLimits,
) -> CommitRequest {
    CommitRequest::new(
        filesystem_id,
        MutationContext::new(
            MutationId::from_u128(mutation),
            RequestFingerprint::blake3(&mutation.to_be_bytes()),
            ClientIncarnationId::from_u128(mutation + 10_000),
            MutationRetention::new(1),
        ),
        fence,
        vec![],
        changes,
        result(limits),
        limits,
    )
    .expect("valid review regression request")
}

async fn acquire(
    store: &PostgresStateStore,
    filesystem_id: FilesystemId,
    identity: u128,
    limits: StateLimits,
) -> Result<WriterFence, Box<dyn std::error::Error>> {
    let request = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(identity),
        WriterScopeId::from_u128(identity + 1),
        WriterIncarnationId::from_u128(identity + 2),
        LeaseId::from_u128(identity + 3),
        LeaseDuration::new(60_000_000)?,
        limits,
    )?;
    let AcquireLeaseOutcome::Granted(grant) = store.acquire_writer_lease(request).await? else {
        return Err("writer lease was not granted".into());
    };
    Ok(grant.fence)
}

async fn bootstrap(
    store: &PostgresStateStore,
    filesystem_id: FilesystemId,
    root_id: InodeId,
    fence: WriterFence,
    mutation: u128,
    limits: StateLimits,
) -> Result<(), Box<dyn std::error::Error>> {
    let filesystem = StateRecord::Filesystem(FilesystemRecord::new(
        filesystem_id,
        StateRevision::new(1)?,
        RecordRevision::new(1)?,
        root_id,
        QidPath::new(2)?,
        DirectoryCookie::new(1),
        1,
    )?);
    let root = inode(
        root_id,
        QidPath::new(1)?,
        InodeData::Directory {
            generation: DirectoryGeneration::new(1)?,
            parent_inode_id: root_id,
        },
        1,
        limits,
    );
    let outcome = store
        .commit(request(
            filesystem_id,
            mutation,
            fence,
            vec![
                StateChange::Insert {
                    key: RecordKey::Filesystem(filesystem_id),
                    record: filesystem,
                },
                StateChange::Insert {
                    key: RecordKey::Inode(filesystem_id, root_id),
                    record: root,
                },
            ],
            limits,
        ))
        .await?;
    if !matches!(outcome, CommitOutcome::Committed(_)) {
        return Err(format!("bootstrap failed: {outcome:?}").into());
    }
    Ok(())
}

fn directory_chain_bootstrap(
    filesystem_id: FilesystemId,
    root_number: u128,
    depth: u32,
    mutation: u128,
    fence: WriterFence,
    limits: StateLimits,
) -> Result<CommitRequest, Box<dyn std::error::Error>> {
    let root_id = InodeId::from_u128(root_number);
    let next_qid_path = u64::from(depth)
        .checked_add(2)
        .ok_or("test QID path overflow")?;
    let next_cookie = u64::from(depth)
        .checked_add(1)
        .ok_or("test directory cookie overflow")?;
    let mut changes = vec![StateChange::Insert {
        key: RecordKey::Filesystem(filesystem_id),
        record: StateRecord::Filesystem(FilesystemRecord::new(
            filesystem_id,
            StateRevision::new(1)?,
            RecordRevision::new(1)?,
            root_id,
            QidPath::new(next_qid_path)?,
            DirectoryCookie::new(next_cookie),
            1,
        )?),
    }];
    let mut parent = root_id;
    for level in 0..=depth {
        let inode_id = if level == 0 {
            root_id
        } else {
            InodeId::from_u128(
                root_number
                    .checked_add(u128::from(level))
                    .ok_or("test inode identity overflow")?,
            )
        };
        changes.push(StateChange::Insert {
            key: RecordKey::Inode(filesystem_id, inode_id),
            record: inode(
                inode_id,
                QidPath::new(u64::from(level) + 1)?,
                InodeData::Directory {
                    generation: DirectoryGeneration::new(1)?,
                    parent_inode_id: if level == 0 { root_id } else { parent },
                },
                1,
                limits,
            ),
        });
        if level != 0 {
            let name = EntryName::new(format!("depth-{level}").into_bytes(), limits)?;
            changes.push(StateChange::Insert {
                key: RecordKey::DirectoryEntry(filesystem_id, parent, name.clone()),
                record: StateRecord::DirectoryEntry(DirectoryEntryRecord::new(
                    parent,
                    name,
                    DirectoryCookie::new(u64::from(level)),
                    inode_id,
                    RecordRevision::new(1)?,
                )?),
            });
        }
        parent = inode_id;
    }
    Ok(request(filesystem_id, mutation, fence, changes, limits))
}

#[tokio::test]
async fn reviewed_commit_edges_preserve_semantic_outcomes() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(dsn) = live_dsn() else {
        return Ok(());
    };
    let pool = connect(dsn, 8).await?;
    PostgresStateStore::migrate(&pool).await?;
    let regression_filesystems = [
        FilesystemId::from_u128(u128::MAX - 520),
        FilesystemId::from_u128(u128::MAX - 521),
        FilesystemId::from_u128(u128::MAX - 522),
    ];
    cleanup_filesystems(&pool, &regression_filesystems).await?;
    let config = PostgresStateConfig::default();
    let store = PostgresStateStore::open(pool.clone(), config).await?;
    let limits = config.limits();

    let invalid_fs = FilesystemId::from_u128(u128::MAX - 520);
    let invalid_root = InodeId::from_u128(1);
    let invalid_child = InodeId::from_u128(2);
    let invalid_fence = acquire(&store, invalid_fs, 10, limits).await?;
    let name = EntryName::new(b"child".to_vec(), limits)?;
    let invalid_bootstrap = request(
        invalid_fs,
        20,
        invalid_fence,
        vec![
            StateChange::Insert {
                key: RecordKey::Filesystem(invalid_fs),
                record: StateRecord::Filesystem(FilesystemRecord::new(
                    invalid_fs,
                    StateRevision::new(1)?,
                    RecordRevision::new(1)?,
                    invalid_root,
                    QidPath::new(3)?,
                    DirectoryCookie::new(1),
                    1,
                )?),
            },
            StateChange::Insert {
                key: RecordKey::Inode(invalid_fs, invalid_root),
                record: inode(
                    invalid_root,
                    QidPath::new(1)?,
                    InodeData::Directory {
                        generation: DirectoryGeneration::new(1)?,
                        parent_inode_id: invalid_root,
                    },
                    1,
                    limits,
                ),
            },
            StateChange::Insert {
                key: RecordKey::Inode(invalid_fs, invalid_child),
                record: inode(invalid_child, QidPath::new(2)?, InodeData::Fifo, 1, limits),
            },
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(invalid_fs, invalid_root, name.clone()),
                record: StateRecord::DirectoryEntry(DirectoryEntryRecord::new(
                    invalid_root,
                    name,
                    DirectoryCookie::new(1),
                    invalid_child,
                    RecordRevision::new(1)?,
                )?),
            },
        ],
        limits,
    );
    assert!(matches!(
        store.commit(invalid_bootstrap).await?,
        CommitOutcome::MalformedRequest(MalformedCommit::InvalidRecord(_))
    ));
    let absent = ReadBatch::new(
        invalid_fs,
        ReadConsistency::LatestLinearizable,
        vec![ReadQuery::Filesystem],
        limits,
    )?;
    assert!(matches!(
        store.read(absent).await?,
        ReadOutcome::Snapshot(snapshot)
            if matches!(snapshot.results()[0], ReadResult::Point { record: None, .. })
    ));

    let filesystem_id = FilesystemId::from_u128(u128::MAX - 521);
    let root_id = InodeId::from_u128(100);
    let file_id = InodeId::from_u128(101);
    let fence = acquire(&store, filesystem_id, 1000, limits).await?;
    bootstrap(&store, filesystem_id, root_id, fence, 1010, limits).await?;

    let missing_open = OpenId::from_u128(102);
    let missing_dependency = request(
        filesystem_id,
        1020,
        fence,
        vec![StateChange::Insert {
            key: RecordKey::Open(filesystem_id, missing_open),
            record: StateRecord::Open(OpenRecord::new(
                missing_open,
                InodeId::from_u128(999),
                ClientIncarnationId::from_u128(103),
                OpenAccess::ReadOnly,
                false,
                InodeGeneration::new(1)?,
                RecordRevision::new(1)?,
            )),
        }],
        limits,
    );
    assert!(matches!(
        store.commit(missing_dependency).await?,
        CommitOutcome::MalformedRequest(MalformedCommit::InvalidRecord(_))
    ));

    let entry_name = EntryName::new(b"file".to_vec(), limits)?;
    let create_file = request(
        filesystem_id,
        1030,
        fence,
        vec![
            StateChange::Insert {
                key: RecordKey::Inode(filesystem_id, file_id),
                record: inode(file_id, QidPath::new(2)?, InodeData::Fifo, 1, limits),
            },
            StateChange::Insert {
                key: RecordKey::DirectoryEntry(filesystem_id, root_id, entry_name.clone()),
                record: StateRecord::DirectoryEntry(DirectoryEntryRecord::new(
                    root_id,
                    entry_name,
                    DirectoryCookie::new(1),
                    file_id,
                    RecordRevision::new(1)?,
                )?),
            },
            StateChange::AdvanceDirectoryCookie {
                count: NonZeroU64::new(1).expect("one is nonzero"),
            },
            StateChange::AdvanceQidPath {
                count: NonZeroU64::new(1).expect("one is nonzero"),
            },
            StateChange::BumpDirectoryGeneration(root_id),
        ],
        limits,
    );
    assert!(matches!(
        store.commit(create_file).await?,
        CommitOutcome::Committed(_)
    ));

    let first_open = OpenId::from_u128(104);
    let second_open = OpenId::from_u128(105);
    let first_client = ClientIncarnationId::from_u128(106);
    let second_client = ClientIncarnationId::from_u128(107);
    let opens = request(
        filesystem_id,
        1040,
        fence,
        vec![
            StateChange::Insert {
                key: RecordKey::Open(filesystem_id, first_open),
                record: StateRecord::Open(OpenRecord::new(
                    first_open,
                    file_id,
                    first_client,
                    OpenAccess::ReadWrite,
                    false,
                    InodeGeneration::new(1)?,
                    RecordRevision::new(1)?,
                )),
            },
            StateChange::Insert {
                key: RecordKey::Open(filesystem_id, second_open),
                record: StateRecord::Open(OpenRecord::new(
                    second_open,
                    file_id,
                    second_client,
                    OpenAccess::ReadWrite,
                    false,
                    InodeGeneration::new(1)?,
                    RecordRevision::new(1)?,
                )),
            },
        ],
        limits,
    );
    assert!(matches!(
        store.commit(opens).await?,
        CommitOutcome::Committed(_)
    ));

    let first_lock = LockId::from_u128(108);
    let second_lock = LockId::from_u128(109);
    let lock_record = |lock_id, range, client, open| {
        StateRecord::Lock(LockRecord::new(
            lock_id,
            file_id,
            range,
            LockKind::Exclusive,
            LockOwner::new(client, open),
            LockGeneration::new(1).expect("one is nonzero"),
            RecordRevision::new(1).expect("one is nonzero"),
        ))
    };
    let locks = request(
        filesystem_id,
        1050,
        fence,
        vec![
            StateChange::Insert {
                key: RecordKey::Lock(filesystem_id, file_id, first_lock),
                record: lock_record(
                    first_lock,
                    LockRange::finite(0, 10)?,
                    first_client,
                    first_open,
                ),
            },
            StateChange::Insert {
                key: RecordKey::Lock(filesystem_id, file_id, second_lock),
                record: lock_record(
                    second_lock,
                    LockRange::finite(20, 30)?,
                    second_client,
                    second_open,
                ),
            },
        ],
        limits,
    );
    assert!(matches!(
        store.commit(locks).await?,
        CommitOutcome::Committed(_)
    ));

    let overlapping = StateRecord::Lock(LockRecord::new(
        second_lock,
        file_id,
        LockRange::finite(5, 15)?,
        LockKind::Exclusive,
        LockOwner::new(second_client, second_open),
        LockGeneration::new(2)?,
        RecordRevision::new(1)?,
    ));
    let replace = request(
        filesystem_id,
        1060,
        fence,
        vec![StateChange::Replace {
            key: RecordKey::Lock(filesystem_id, file_id, second_lock),
            record: overlapping.clone(),
        }],
        limits,
    );
    assert!(matches!(
        store.commit(replace).await?,
        CommitOutcome::Conflict(conflict)
            if matches!(conflict.kind, CommitConflictKind::LockConflict { existing }
                if existing == first_lock)
    ));
    let atomic_transition = request(
        filesystem_id,
        1070,
        fence,
        vec![
            StateChange::Delete(RecordKey::Lock(filesystem_id, file_id, first_lock)),
            StateChange::Replace {
                key: RecordKey::Lock(filesystem_id, file_id, second_lock),
                record: overlapping,
            },
        ],
        limits,
    );
    assert!(matches!(
        store.commit(atomic_transition).await?,
        CommitOutcome::Committed(_)
    ));

    let history_limits = StateLimits::new(StateLimitValues {
        max_change_history_commits: 1,
        ..StateLimitValues::default()
    })?;
    let small_config = PostgresStateConfig::new(
        history_limits,
        config.statement_timeout(),
        config.lock_timeout(),
        config.definitive_abort_retries(),
        config.ambiguous_commit_recovery_attempts(),
        config.durability(),
    )?;
    let small_store = PostgresStateStore::open(pool.clone(), small_config).await?;
    let history_fs = FilesystemId::from_u128(u128::MAX - 522);
    let history_root = InodeId::from_u128(200);
    let history_fence = acquire(&small_store, history_fs, 2000, history_limits).await?;
    bootstrap(
        &small_store,
        history_fs,
        history_root,
        history_fence,
        2010,
        history_limits,
    )
    .await?;
    let large_store = PostgresStateStore::open(pool, config).await?;
    let bump = request(
        history_fs,
        2020,
        history_fence,
        vec![StateChange::BumpInodeGeneration(history_root)],
        limits,
    );
    assert!(matches!(
        large_store.commit(bump).await?,
        CommitOutcome::Committed(_)
    ));
    let poll = ChangePoll::new(
        history_fs,
        ChangeCursor::after(StateRevision::new(1)?),
        10,
        limits.max_change_keys(),
        limits,
    )?;
    assert!(matches!(
        large_store.poll_changes(poll).await?,
        ChangePollOutcome::RevisionCompacted {
            oldest_available,
            ..
        } if oldest_available.get() == 2
    ));
    Ok(())
}

#[tokio::test]
async fn postgres_ancestry_accepts_exact_bound_and_rejects_one_edge_beyond()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(dsn) = live_dsn() else {
        return Ok(());
    };
    let pool = connect(dsn, 4).await?;
    PostgresStateStore::migrate(&pool).await?;
    let exact_fs = FilesystemId::from_u128(u128::MAX - 523);
    let beyond_fs = FilesystemId::from_u128(u128::MAX - 524);
    cleanup_filesystems(&pool, &[exact_fs, beyond_fs]).await?;

    let default = PostgresStateConfig::default();
    let limits = StateLimits::new(StateLimitValues {
        max_directory_ancestor_depth: 2,
        ..StateLimitValues::default()
    })?;
    let config = PostgresStateConfig::new(
        limits,
        default.statement_timeout(),
        default.lock_timeout(),
        default.definitive_abort_retries(),
        default.ambiguous_commit_recovery_attempts(),
        default.durability(),
    )?;
    let store = PostgresStateStore::open(pool, config).await?;

    let exact_fence = acquire(&store, exact_fs, 3_000, limits).await?;
    let exact = directory_chain_bootstrap(exact_fs, 10_000, 2, 3_010, exact_fence, limits)?;
    assert!(matches!(
        store.commit(exact).await?,
        CommitOutcome::Committed(_)
    ));

    let beyond_fence = acquire(&store, beyond_fs, 4_000, limits).await?;
    let beyond = directory_chain_bootstrap(beyond_fs, 20_000, 3, 4_010, beyond_fence, limits)?;
    assert!(matches!(
        store.commit(beyond).await?,
        CommitOutcome::MalformedRequest(MalformedCommit::InvalidRecord(
            RecordValidationError::DirectoryAncestorLimit { maximum: 2 }
        ))
    ));
    Ok(())
}
