//! Paged content references cross the PostgreSQL boundary through the unchanged state API.

mod common;

use w9pt_fs_state::{
    AcquireLeaseOutcome, AcquireWriterLease, ClientIncarnationId, CommitOutcome, CommitRequest,
    ContentMetadataRecord, DataGeneration, DirectoryCookie, DirectoryEntryRecord,
    DirectoryGeneration, EntryName, FilesystemId, FilesystemRecord, FilesystemStateStore, GroupId,
    InodeAttributeUpdate, InodeData, InodeGeneration, InodeId, InodeRecord, InodeTimes,
    LeaseDuration, LeaseId, LeaseOperationId, MutationContext, MutationResult, MutationResultKind,
    MutationRetention, Precondition, PrincipalId, PublishContent, QidPath, ReadBatch,
    ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision,
    RequestFingerprint, ResultFormatVersion, StateChange, StateLimits, StateRecord, StateRevision,
    UnixTimestamp, WriterIncarnationId, WriterScopeId,
};
use w9pt_fs_state_postgres::{PostgresStateConfig, PostgresStateStore};
use w9pt_fs_storage::{
    BLOCK_SIZE, BaseContentIdentity, ContentContextId, ContentRepository, CreationDefaults,
    FileContextScope, FileId, FileStoragePolicy, MutationId, StorageLimits, StorageMethod,
    format::ManifestLayout, open_committed_context, testing::MemoryTarget,
};

use common::{cleanup_filesystems, connect, live_dsn};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn mutation(id: MutationId) -> MutationContext {
    MutationContext::new(
        id,
        RequestFingerprint::blake3(id.as_bytes()),
        ClientIncarnationId::from_u128(1),
        MutationRetention::new(100),
    )
}

fn terminal_result(bytes: &[u8], limits: StateLimits) -> TestResult<MutationResult> {
    Ok(MutationResult::new(
        MutationResultKind::new(1)?,
        ResultFormatVersion::new(1)?,
        bytes.to_vec(),
        limits,
    )?)
}

#[tokio::test]
async fn paged_content_reference_is_published_and_reopened_through_postgres() -> TestResult {
    let Some(dsn) = live_dsn() else {
        eprintln!("skipping live paged-content handoff; W9PT_POSTGRES_TEST_DSN is unset");
        return Ok(());
    };
    let writer_pool = connect(dsn.clone(), 2).await?;
    PostgresStateStore::migrate(&writer_pool).await?;
    let filesystem_id = FilesystemId::from_u128(u128::MAX - 7_050);
    cleanup_filesystems(&writer_pool, &[filesystem_id]).await?;

    // Clean the exact test filesystem on either success or a returned error.
    let result = exercise_handoff(&dsn, &writer_pool, filesystem_id).await;
    cleanup_filesystems(&writer_pool, &[filesystem_id]).await?;
    result
}

async fn exercise_handoff(
    dsn: &str,
    writer_pool: &sea_orm::DatabaseConnection,
    filesystem_id: FilesystemId,
) -> TestResult {
    let config = PostgresStateConfig::default();
    let limits = config.limits();
    let writer = PostgresStateStore::open(writer_pool.clone(), config).await?;
    let AcquireLeaseOutcome::Granted(grant) = writer
        .acquire_writer_lease(AcquireWriterLease::new(
            filesystem_id,
            LeaseOperationId::from_u128(1),
            WriterScopeId::from_u128(1),
            WriterIncarnationId::from_u128(1),
            LeaseId::from_u128(1),
            LeaseDuration::new(60_000_000)?,
            limits,
        )?)
        .await?
    else {
        return Err("test writer lease was not granted".into());
    };

    let root_id = InodeId::from_u128(1);
    let file_id = InodeId::from_u128(2);
    let content_file_id = FileId::from_u128(3);
    let context_id = ContentContextId::from_u128(4);
    let name = EntryName::new(b"paged-file".to_vec(), limits)?;
    let time = UnixTimestamp::new(1, 0)?;
    let times = InodeTimes {
        accessed: time,
        modified: time,
        changed: time,
        created: time,
    };
    let record_revision = RecordRevision::new(1)?;
    let owner = PrincipalId::new(b"owner".to_vec(), limits)?;
    let group = GroupId::new(b"group".to_vec(), limits)?;
    let records = vec![
        (
            RecordKey::Filesystem(filesystem_id),
            StateRecord::Filesystem(FilesystemRecord::new(
                filesystem_id,
                StateRevision::new(1)?,
                record_revision,
                root_id,
                QidPath::new(3)?,
                DirectoryCookie::new(2),
                1,
            )?),
        ),
        (
            RecordKey::Inode(filesystem_id, root_id),
            StateRecord::Inode(InodeRecord::new(
                root_id,
                QidPath::new(1)?,
                record_revision,
                0o755,
                owner.clone(),
                group.clone(),
                times,
                0,
                1,
                InodeGeneration::new(1)?,
                InodeData::Directory {
                    generation: DirectoryGeneration::new(1)?,
                    parent_inode_id: root_id,
                },
            )?),
        ),
        (
            RecordKey::Inode(filesystem_id, file_id),
            StateRecord::Inode(InodeRecord::new_regular(
                file_id,
                QidPath::new(2)?,
                record_revision,
                0o644,
                owner,
                group,
                times,
                0,
                1,
                InodeGeneration::new(1)?,
                context_id,
                InodeData::RegularFile {
                    content_file_id,
                    content: None,
                    data_generation: 0,
                },
            )?),
        ),
        (
            RecordKey::ContentMetadata(filesystem_id, content_file_id),
            StateRecord::ContentMetadata(ContentMetadataRecord::new(
                file_id,
                content_file_id,
                context_id,
                FileStoragePolicy::FORMAT,
                FileStoragePolicy::plain(StorageMethod::BlockSplit)
                    .to_bytes()
                    .to_vec(),
                None,
                None,
                record_revision,
                limits,
            )?),
        ),
        (
            RecordKey::DirectoryEntry(filesystem_id, root_id, name.clone()),
            StateRecord::DirectoryEntry(DirectoryEntryRecord::new(
                root_id,
                name,
                DirectoryCookie::new(1),
                file_id,
                record_revision,
            )?),
        ),
    ];
    let bootstrap = CommitRequest::new(
        filesystem_id,
        mutation(MutationId::from_u128(1)),
        grant.fence,
        records
            .iter()
            .map(|(key, _)| Precondition::RecordAbsent(key.clone()))
            .collect::<Vec<_>>(),
        records
            .into_iter()
            .map(|(key, record)| StateChange::Insert { key, record })
            .collect::<Vec<_>>(),
        terminal_result(b"created", limits)?,
        limits,
    )?;
    let CommitOutcome::Committed(created) = writer.commit(bootstrap).await? else {
        return Err("test bootstrap did not commit".into());
    };
    let context = open_committed_context(
        FileContextScope::new(
            *filesystem_id.as_bytes(),
            *file_id.as_bytes(),
            content_file_id,
            context_id,
        ),
        FileStoragePolicy::FORMAT,
        &FileStoragePolicy::plain(StorageMethod::BlockSplit).to_bytes(),
        None,
        None,
        created.revision.get(),
        None,
    )?;

    let target = MemoryTarget::new();
    let repository = ContentRepository::new(
        target.clone(),
        "postgres-paged-handoff",
        CreationDefaults::new(StorageMethod::BlockSplit),
        StorageLimits::default(),
    )?;
    let offset = u64::from(BLOCK_SIZE) * 16_384;
    let bytes = b"paged content";
    let mutation_id = MutationId::from_u128(2);
    let prepared = repository
        .prepare_write_from_new_with_context(&context, mutation_id, 0, offset, bytes)
        .await?;
    let expected = prepared.content().clone();
    let modified = UnixTimestamp::new(2, 3)?;
    let request = CommitRequest::new(
        filesystem_id,
        mutation(mutation_id),
        grant.fence,
        vec![],
        vec![StateChange::PublishContent(PublishContent {
            inode_id: file_id,
            expected_base: BaseContentIdentity::NEW_FILE,
            logical_size: expected.logical_size(),
            data_generation: DataGeneration::new(expected.generation())?,
            prepared,
            inode_generation: InodeGeneration::new(2)?,
            attributes: InodeAttributeUpdate {
                mode: Some(0o640),
                modified: Some(modified),
                changed: Some(modified),
                ..InodeAttributeUpdate::default()
            },
        })],
        terminal_result(b"published", limits)?,
        limits,
    )?;
    let CommitOutcome::Committed(committed) = writer.commit(request.clone()).await? else {
        return Err("paged content publication did not commit".into());
    };
    drop(writer);
    drop(repository);

    // The new database pool and repository retain no client-local publication state.
    let observer = PostgresStateStore::open(connect(dsn, 2).await?, config).await?;
    let CommitOutcome::AlreadyCommitted(replayed) = observer.commit(request).await? else {
        return Err("paged content publication did not replay after reopen".into());
    };
    assert_eq!(replayed, committed);
    let ReadOutcome::Snapshot(snapshot) = observer
        .read(ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Inode(file_id), ReadQuery::Mutation(mutation_id)],
            limits,
        )?)
        .await?
    else {
        return Err("reopened PostgreSQL client did not return a snapshot".into());
    };
    assert_eq!(snapshot.revision(), committed.revision);
    let ReadResult::Point {
        record: Some(record),
        ..
    } = &snapshot.results()[0]
    else {
        return Err("published file is absent".into());
    };
    let StateRecord::Inode(inode) = record.as_ref() else {
        return Err("published file record is not an inode".into());
    };
    assert_eq!(inode.content(), Some(&expected));
    assert_eq!(inode.logical_size(), offset + u64::try_from(bytes.len())?);
    assert_eq!(inode.inode_generation().get(), 2);
    assert_eq!(inode.mode(), 0o640);
    assert_eq!(inode.times().modified, modified);
    assert_eq!(inode.times().changed, modified);
    assert!(matches!(
        &snapshot.results()[1],
        ReadResult::Point { record: Some(record), .. }
            if matches!(record.as_ref(), StateRecord::Mutation(result)
                if result.result() == &committed.result
                    && result.committed_revision() == committed.revision)
    ));

    let reopened = ContentRepository::new(
        target,
        "postgres-paged-handoff",
        CreationDefaults::new(StorageMethod::Raw),
        StorageLimits::default(),
    )?;
    let content = inode.content().ok_or("published content is absent")?;
    let manifest = reopened
        .load_manifest_with_context(content, &context)
        .await?;
    assert!(matches!(manifest.layout(),
        ManifestLayout::BlockSplit { root: Some(root), .. } if root.level() == 2
    ));
    assert_eq!(
        reopened
            .read_with_context(content, &context, offset, bytes.len())
            .await?,
        bytes
    );
    assert_eq!(
        reopened
            .read_with_context(content, &context, offset - 1, 1)
            .await?,
        [0]
    );
    Ok(())
}
