//! Live empty-authority bootstrap and exact mutation replay.

mod common;

use w9pt_fs_state::{
    AcquireLeaseOutcome, AcquireWriterLease, ChangeCursor, ChangePoll, ChangePollOutcome,
    ClientIncarnationId, CommitOutcome, CommitRequest, DirectoryCookie, DirectoryGeneration,
    FilesystemId, FilesystemRecord, FilesystemStateStore, GroupId, InodeData, InodeGeneration,
    InodeId, InodeRecord, InodeTimes, LeaseDuration, LeaseId, LeaseOperationId, MutationContext,
    MutationResult, MutationResultKind, MutationRetention, Precondition, PrincipalId, QidPath,
    ReadBatch, ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision,
    RequestFingerprint, ResultFormatVersion, StateChange, StateLimitValues, StateLimits,
    StateRecord, StateRevision, UnixTimestamp, WriterIncarnationId, WriterScopeId,
};
use w9pt_fs_state_postgres::{PostgresStateConfig, PostgresStateStore};
use w9pt_fs_storage::MutationId;

use common::{cleanup_filesystems, connect, live_dsn};

fn timestamps() -> InodeTimes {
    let value = UnixTimestamp::new(1, 2).expect("valid timestamp");
    InodeTimes {
        accessed: value,
        modified: value,
        changed: value,
        created: value,
    }
}

#[tokio::test]
async fn lease_before_bootstrap_and_exact_replay_survive_reopen()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(dsn) = live_dsn() else {
        return Ok(());
    };
    let pool = connect(dsn, 6).await?;
    PostgresStateStore::migrate(&pool).await?;
    let filesystem_id = FilesystemId::from_u128(u128::MAX - 303);
    cleanup_filesystems(&pool, &[filesystem_id]).await?;
    let config = PostgresStateConfig::default();
    let store = PostgresStateStore::open(pool.clone(), config).await?;
    let limits = StateLimits::default();
    let root_inode_id = InodeId::from_u128(1);
    let scope = WriterScopeId::from_u128(2);
    let holder = WriterIncarnationId::from_u128(3);
    let acquire = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(4),
        scope,
        holder,
        LeaseId::from_u128(5),
        LeaseDuration::new(60_000_000)?,
        limits,
    )?;
    let AcquireLeaseOutcome::Granted(grant) = store.acquire_writer_lease(acquire).await? else {
        panic!("bootstrap lease was not granted");
    };

    let revision_one = StateRevision::new(1)?;
    let record_one = RecordRevision::new(1)?;
    let filesystem = StateRecord::Filesystem(FilesystemRecord::new(
        filesystem_id,
        revision_one,
        record_one,
        root_inode_id,
        QidPath::new(2)?,
        DirectoryCookie::new(1),
        1,
    )?);
    let root = StateRecord::Inode(InodeRecord::new(
        root_inode_id,
        QidPath::new(1)?,
        record_one,
        0o755,
        PrincipalId::new(b"root".to_vec(), limits)?,
        GroupId::new(b"root".to_vec(), limits)?,
        timestamps(),
        0,
        1,
        InodeGeneration::new(1)?,
        InodeData::Directory {
            generation: DirectoryGeneration::new(1)?,
            parent_inode_id: root_inode_id,
        },
    )?);
    let filesystem_key = RecordKey::Filesystem(filesystem_id);
    let root_key = RecordKey::Inode(filesystem_id, root_inode_id);
    let mutation = MutationContext::new(
        MutationId::from_u128(6),
        RequestFingerprint::blake3(b"postgres bootstrap v1"),
        ClientIncarnationId::from_u128(7),
        MutationRetention::new(8),
    );
    let result = MutationResult::new(
        MutationResultKind::new(1)?,
        ResultFormatVersion::new(1)?,
        b"bootstrapped".to_vec(),
        limits,
    )?;
    let request = CommitRequest::new(
        filesystem_id,
        mutation,
        grant.fence,
        vec![
            Precondition::RecordAbsent(filesystem_key.clone()),
            Precondition::RecordAbsent(root_key.clone()),
        ],
        vec![
            StateChange::Insert {
                key: filesystem_key.clone(),
                record: filesystem,
            },
            StateChange::Insert {
                key: root_key.clone(),
                record: root,
            },
        ],
        result,
        limits,
    )?;
    let tighter_replay = request.clone();
    let CommitOutcome::Committed(committed) = store.commit(request.clone()).await? else {
        panic!("bootstrap mutation did not commit");
    };
    assert_eq!(committed.revision.get(), 3);
    let CommitOutcome::AlreadyCommitted(replayed) = store.commit(request).await? else {
        panic!("bootstrap mutation was not replayed");
    };
    assert_eq!(replayed, committed);

    let tighter_limits = StateLimits::new(StateLimitValues {
        max_mutation_result_bytes: 1,
        ..StateLimitValues::default()
    })?;
    let default_config = PostgresStateConfig::default();
    let tighter_config = PostgresStateConfig::new(
        tighter_limits,
        default_config.statement_timeout(),
        default_config.lock_timeout(),
        default_config.definitive_abort_retries(),
        default_config.ambiguous_commit_recovery_attempts(),
        default_config.durability(),
    )?;
    let tighter_store = PostgresStateStore::open(pool.clone(), tighter_config).await?;
    let CommitOutcome::AlreadyCommitted(tighter) = tighter_store.commit(tighter_replay).await?
    else {
        panic!("tighter receiving limits rejected an exact retained replay");
    };
    assert_eq!(tighter, committed);

    let poll = ChangePoll::new(
        filesystem_id,
        ChangeCursor::after(StateRevision::new(1)?),
        10,
        10,
        limits,
    )?;
    let ChangePollOutcome::Changes(changes) = store.poll_changes(poll).await? else {
        panic!("expected bootstrap change history");
    };
    assert_eq!(changes.events().len(), 2);
    assert_eq!(changes.next().revision(), committed.revision);
    assert_eq!(changes.current_revision(), committed.revision);

    let too_small = ChangePoll::new(
        filesystem_id,
        ChangeCursor::after(StateRevision::new(2)?),
        10,
        2,
        limits,
    )?;
    assert!(matches!(
        store.poll_changes(too_small).await?,
        ChangePollOutcome::PollBoundTooSmall {
            revision,
            required_keys: 3
        } if revision == committed.revision
    ));
    let future = ChangePoll::new(
        filesystem_id,
        ChangeCursor::after(StateRevision::new(4)?),
        1,
        1,
        limits,
    )?;
    assert!(matches!(
        store.poll_changes(future).await?,
        ChangePollOutcome::RevisionUnavailable { current, .. }
            if current == committed.revision
    ));

    drop(store);
    let reopened = PostgresStateStore::open(pool, config).await?;
    let batch = ReadBatch::new(
        filesystem_id,
        ReadConsistency::LatestLinearizable,
        vec![
            ReadQuery::Filesystem,
            ReadQuery::Inode(root_inode_id),
            ReadQuery::InodeByQidPath(QidPath::new(1)?),
        ],
        limits,
    )?;
    let ReadOutcome::Snapshot(snapshot) = reopened.read(batch).await? else {
        panic!("reopened store did not return a snapshot");
    };
    assert_eq!(snapshot.revision(), committed.revision);
    assert!(matches!(
        &snapshot.results()[0],
        ReadResult::Point {
            record: Some(_),
            ..
        }
    ));
    assert!(matches!(
        &snapshot.results()[1],
        ReadResult::Point {
            record: Some(_),
            ..
        }
    ));
    assert!(matches!(
        &snapshot.results()[2],
        ReadResult::InodeByQidPath {
            qid_path,
            inode: Some(inode),
        } if *qid_path == QidPath::new(1)? && inode.inode_id() == root_inode_id
    ));
    Ok(())
}
