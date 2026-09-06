//! PostgreSQL-selected encrypted content reopened and rewrapped through SeaweedFS.

mod support;

use core::convert::Infallible;
use std::{env, error::Error};

use aws_sdk_s3::Client;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement,
    TransactionTrait,
};
use support::content_target;
use w9pt_fs::{
    ContentContextOrchestrationError, content_context_create_changes, generate_content_context,
    load_committed_content_context, rewrap_content_context,
};
use w9pt_fs_state::{
    AcquireLeaseOutcome, AcquireWriterLease, ClientIncarnationId, CommitOutcome, CommitRequest,
    ContentMetadataRecord, DataGeneration, DirectoryCookie, DirectoryEntryRecord,
    DirectoryGeneration, EntryName, FilesystemId, FilesystemRecord, FilesystemStateStore, GroupId,
    InodeData, InodeGeneration, InodeId, InodeRecord, InodeTimes, LeaseDuration, LeaseId,
    LeaseOperationId, MutationContext, MutationResult, MutationResultKind, MutationRetention,
    Precondition, PrincipalId, PublishContent, QidPath, RecordKey, RecordRevision,
    RequestFingerprint, ResultFormatVersion, StateChange, StateLimits, StateRecord, StateRevision,
    UnixTimestamp, WriterFence, WriterIncarnationId, WriterScopeId,
};
use w9pt_fs_state_postgres::{PostgresStateConfig, PostgresStateStore};
use w9pt_fs_storage::{
    BLOCK_SIZE, CompressionPolicy, ContentCipher, ContentContextId, ContentRepository,
    CreationDefaults, FileId, FileStoragePolicy, MasterKey, MasterKeyId, MutationId,
    RepresentationError, SecureEntropy, StorageLimits, StorageMethod,
};
use w9pt_fs_storage_s3::S3QualificationNamespace;
use w9pt_integration_test::{ensure_bucket, seaweed_client};

type TestError = Box<dyn Error + Send + Sync>;
type TestResult<T = ()> = Result<T, TestError>;

struct PublicEntropy(u8);

impl SecureEntropy for PublicEntropy {
    type Error = Infallible;

    fn fill_secure(&mut self, destination: &mut [u8]) -> Result<(), Self::Error> {
        destination.fill(self.0);
        self.0 = self.0.wrapping_add(1);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn postgres_winner_reopens_and_rewraps_encrypted_seaweedfs_content() -> TestResult {
    if env::var("W9PT_CONTENT_CONTEXT_TEST_REQUIRED").as_deref() != Ok("1") {
        eprintln!(
            "skipping content-context composition; set W9PT_CONTENT_CONTEXT_TEST_REQUIRED=1 to require it"
        );
        return Ok(());
    }
    let endpoint = required("W9PT_TEST_S3_ENDPOINT")?;
    let bucket = required("W9PT_TEST_S3_BUCKET")?;
    let prefix = required("W9PT_CONTENT_CONTEXT_S3_PREFIX")?;
    let dsn = required("W9PT_POSTGRES_TEST_DSN")?;
    let s3_client = seaweed_client(&endpoint);
    ensure_bucket(&s3_client, &bucket).await?;
    cleanup_objects(&s3_client, &bucket, &prefix).await?;

    let database = connect(&dsn).await?;
    PostgresStateStore::migrate(&database).await?;
    let filesystem_id = FilesystemId::from_u128(u128::MAX - 9_100);
    cleanup_database(&database, filesystem_id).await?;
    let result = exercise(
        &endpoint,
        &bucket,
        &prefix,
        &dsn,
        &database,
        filesystem_id,
        &s3_client,
    )
    .await;
    let object_cleanup = cleanup_objects(&s3_client, &bucket, &prefix).await;
    let database_cleanup = cleanup_database(&database, filesystem_id).await;
    result?;
    object_cleanup?;
    database_cleanup?;
    Ok(())
}

async fn exercise(
    endpoint: &str,
    bucket: &str,
    prefix: &str,
    dsn: &str,
    database: &DatabaseConnection,
    filesystem_id: FilesystemId,
    s3_client: &Client,
) -> TestResult {
    let namespace = S3QualificationNamespace::new(prefix.to_owned())?;
    let (first_candidate, second_candidate) = content_target::candidates(endpoint, bucket)?;
    content_target::ensure_unqualified(&first_candidate, &second_candidate)?;
    let (first_target, second_target) =
        content_target::probe_and_wrap(&first_candidate, &second_candidate, &namespace).await?;
    content_target::ensure_unqualified(&first_candidate, &second_candidate)?;

    let config = PostgresStateConfig::default();
    let limits = config.limits();
    let writer = PostgresStateStore::open(database.clone(), config).await?;
    let observer = PostgresStateStore::open(connect(dsn).await?, config).await?;
    let fence = acquire_fence(&writer, filesystem_id, limits).await?;
    let root_id = InodeId::from_u128(1);
    let inode_id = InodeId::from_u128(2);
    let file_id = FileId::from_u128(3);
    let winning_context_id = ContentContextId::from_u128(4);
    let losing_context_id = ContentContextId::from_u128(5);
    let old_master = MasterKey::new(MasterKeyId::new([0x11; 16]), [0x12; 32]);
    let new_master = MasterKey::new(MasterKeyId::new([0x13; 16]), [0x14; 32]);
    let policy = FileStoragePolicy::new(
        StorageMethod::BlockSplit,
        CompressionPolicy::Lz4BlockV1,
        ContentCipher::Aes256SivV1,
    );
    let revision = RecordRevision::new(1)?;
    let mut winning_entropy = PublicEntropy(0x21);
    let winning = generate_content_context(
        filesystem_id,
        inode_id,
        file_id,
        winning_context_id,
        policy,
        Some(&old_master),
        &mut winning_entropy,
        revision,
        limits,
    )?;
    let mut losing_entropy = PublicEntropy(0x22);
    let losing = generate_content_context(
        filesystem_id,
        inode_id,
        file_id,
        losing_context_id,
        policy,
        Some(&old_master),
        &mut losing_entropy,
        revision,
        limits,
    )?;
    assert_ne!(winning.key_commitment(), losing.key_commitment());

    let create_mutation = mutation(1, b"encrypted-create");
    let create = creation_request(
        filesystem_id,
        root_id,
        inode_id,
        winning.clone(),
        create_mutation,
        fence,
        limits,
    )?;
    let committed = match writer.commit(create).await? {
        CommitOutcome::Committed(committed) => committed,
        outcome => return Err(format!("encrypted create returned {outcome:?}").into()),
    };
    let losing_retry = creation_request(
        filesystem_id,
        root_id,
        inode_id,
        losing,
        create_mutation,
        fence,
        limits,
    )?;
    assert!(matches!(
        observer.commit(losing_retry).await?,
        CommitOutcome::AlreadyCommitted(replayed) if replayed == committed
    ));

    let (inode, metadata, context) = load_committed_content_context(
        &observer,
        filesystem_id,
        inode_id,
        Some(&old_master),
    )
    .await?;
    assert_eq!(metadata.context_id(), winning_context_id);
    assert_eq!(context.binding().key_commitment(), winning.key_commitment());

    let object_prefix = format!("{prefix}/objects");
    let repository = ContentRepository::new(
        first_target,
        object_prefix.clone(),
        CreationDefaults::new(StorageMethod::Raw),
        StorageLimits::default(),
    )?;
    let offset = u64::from(BLOCK_SIZE)
        .checked_mul(16_384)
        .and_then(|value| value.checked_add(7))
        .ok_or("composition offset overflowed")?;
    let bytes = vec![b'Z'; 1_025];
    let content_mutation = MutationId::from_u128(2);
    let prepared = repository
        .prepare_write_from_new_with_context(&context, content_mutation, 0, offset, &bytes)
        .await?;
    let content = prepared.content().clone();
    let publish = CommitRequest::new(
        filesystem_id,
        mutation(2, b"encrypted-publish"),
        fence,
        vec![
            Precondition::RecordRevision {
                key: RecordKey::Inode(filesystem_id, inode_id),
                expected: inode.revision(),
            },
            Precondition::RecordRevision {
                key: RecordKey::ContentMetadata(filesystem_id, file_id),
                expected: metadata.revision(),
            },
        ],
        vec![StateChange::PublishContent(PublishContent {
            inode_id,
            expected_base: w9pt_fs_storage::BaseContentIdentity::NEW_FILE,
            logical_size: content.logical_size(),
            data_generation: DataGeneration::new(content.generation())?,
            prepared,
            inode_generation: InodeGeneration::new(2)?,
            attributes: w9pt_fs_state::InodeAttributeUpdate::default(),
        })],
        terminal_result(b"published", limits)?,
        limits,
    )?;
    assert!(matches!(writer.commit(publish).await?, CommitOutcome::Committed(_)));
    drop(context);
    drop(repository);

    let (published_inode, published_metadata, reopened_context) =
        load_committed_content_context(
            &observer,
            filesystem_id,
            inode_id,
            Some(&old_master),
        )
        .await?;
    assert_eq!(published_inode.content(), Some(&content));
    let reader = ContentRepository::new(
        second_target,
        object_prefix.clone(),
        CreationDefaults::new(StorageMethod::Raw),
        StorageLimits::default(),
    )?;
    assert_eq!(
        reader
            .read_with_context(&content, &reopened_context, offset, bytes.len())
            .await?,
        bytes
    );
    assert_eq!(
        reader
            .read_with_context(&content, &reopened_context, offset - 1, 1)
            .await?,
        [0]
    );
    let objects_before = snapshot_objects(s3_client, bucket, &object_prefix).await?;

    let (metadata_key, rewrap) = rewrap_content_context::<Infallible>(
        filesystem_id,
        &published_metadata,
        &old_master,
        &new_master,
    )?;
    let rewrap_request = CommitRequest::new(
        filesystem_id,
        mutation(3, b"encrypted-rewrap"),
        fence,
        vec![Precondition::RecordRevision {
            key: metadata_key,
            expected: published_metadata.revision(),
        }],
        vec![rewrap],
        terminal_result(b"rewrapped", limits)?,
        limits,
    )?;
    assert!(matches!(
        writer.commit(rewrap_request.clone()).await?,
        CommitOutcome::Committed(_)
    ));
    assert!(matches!(
        observer.commit(rewrap_request).await?,
        CommitOutcome::AlreadyCommitted(_)
    ));
    assert!(matches!(
        load_committed_content_context(
            &observer,
            filesystem_id,
            inode_id,
            Some(&old_master),
        )
        .await,
        Err(ContentContextOrchestrationError::Representation(
            RepresentationError::WrongMaster
        ))
    ));
    let (rewrapped_inode, _, rewrapped_context) = load_committed_content_context(
        &observer,
        filesystem_id,
        inode_id,
        Some(&new_master),
    )
    .await?;
    assert_eq!(rewrapped_inode.content(), Some(&content));
    assert_eq!(objects_before, snapshot_objects(s3_client, bucket, &object_prefix).await?);
    assert_eq!(
        reader
            .read_with_context(&content, &rewrapped_context, offset, bytes.len())
            .await?,
        bytes
    );
    content_target::ensure_unqualified(&first_candidate, &second_candidate)?;
    Ok(())
}

fn creation_request(
    filesystem_id: FilesystemId,
    root_id: InodeId,
    inode_id: InodeId,
    metadata: ContentMetadataRecord,
    mutation: MutationContext,
    fence: WriterFence,
    limits: StateLimits,
) -> TestResult<CommitRequest> {
    let timestamp = UnixTimestamp::new(1, 0)?;
    let times = InodeTimes {
        accessed: timestamp,
        modified: timestamp,
        changed: timestamp,
        created: timestamp,
    };
    let revision = RecordRevision::new(1)?;
    let owner = PrincipalId::new(b"owner".to_vec(), limits)?;
    let group = GroupId::new(b"group".to_vec(), limits)?;
    let root = InodeRecord::new(
        root_id,
        QidPath::new(1)?,
        revision,
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
    )?;
    let inode = InodeRecord::new_regular(
        inode_id,
        QidPath::new(2)?,
        revision,
        0o600,
        owner,
        group,
        times,
        0,
        1,
        InodeGeneration::new(1)?,
        metadata.context_id(),
        InodeData::RegularFile {
            content_file_id: metadata.content_file_id(),
            content: None,
            data_generation: 0,
        },
    )?
    ;
    let mut changes = vec![
        StateChange::Insert {
            key: RecordKey::Filesystem(filesystem_id),
            record: StateRecord::Filesystem(FilesystemRecord::new(
                filesystem_id,
                StateRevision::new(1)?,
                revision,
                root_id,
                QidPath::new(3)?,
                DirectoryCookie::new(2),
                1,
            )?),
        },
        StateChange::Insert {
            key: RecordKey::Inode(filesystem_id, root_id),
            record: StateRecord::Inode(root),
        },
    ];
    changes.extend(content_context_create_changes(filesystem_id, inode, metadata)?);
    let name = EntryName::new(b"encrypted".to_vec(), limits)?;
    changes.push(StateChange::Insert {
        key: RecordKey::DirectoryEntry(filesystem_id, root_id, name.clone()),
        record: StateRecord::DirectoryEntry(DirectoryEntryRecord::new(
            root_id,
            name,
            DirectoryCookie::new(1),
            inode_id,
            revision,
        )?),
    });
    let preconditions: Vec<_> = changes
        .iter()
        .filter_map(|change| match change {
            StateChange::Insert { key, .. } => Some(Precondition::RecordAbsent(key.clone())),
            _ => None,
        })
        .collect();
    Ok(CommitRequest::new(
        filesystem_id,
        mutation,
        fence,
        preconditions,
        changes,
        terminal_result(b"created", limits)?,
        limits,
    )?)
}

async fn acquire_fence(
    store: &PostgresStateStore,
    filesystem_id: FilesystemId,
    limits: StateLimits,
) -> TestResult<WriterFence> {
    let request = AcquireWriterLease::new(
        filesystem_id,
        LeaseOperationId::from_u128(1),
        WriterScopeId::from_u128(1),
        WriterIncarnationId::from_u128(1),
        LeaseId::from_u128(1),
        LeaseDuration::new(60_000_000)?,
        limits,
    )?;
    match store.acquire_writer_lease(request).await? {
        AcquireLeaseOutcome::Granted(grant) => Ok(grant.fence),
        outcome => Err(format!("composition lease returned {outcome:?}").into()),
    }
}

fn mutation(id: u128, fingerprint: &[u8]) -> MutationContext {
    MutationContext::new(
        MutationId::from_u128(id),
        RequestFingerprint::blake3(fingerprint),
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

async fn connect(dsn: &str) -> Result<DatabaseConnection, sea_orm::DbErr> {
    let mut options = ConnectOptions::new(dsn.to_owned());
    options
        .max_connections(4)
        .min_connections(0)
        .sqlx_logging(false);
    Database::connect(options).await
}

async fn cleanup_database(
    database: &DatabaseConnection,
    filesystem_id: FilesystemId,
) -> Result<(), sea_orm::DbErr> {
    const TABLES: &[&str] = &[
        "w9pt_fs_state_change_keys",
        "w9pt_fs_state_change_commits",
        "w9pt_fs_state_directory_entries",
        "w9pt_fs_state_inodes",
        "w9pt_fs_state_content_metadata",
        "w9pt_fs_state_filesystem_records",
        "w9pt_fs_state_mutation_results",
        "w9pt_fs_state_writer_lease_operations",
        "w9pt_fs_state_writer_fences",
        "w9pt_fs_state_authority_heads",
    ];
    let transaction = database.begin().await?;
    for table in TABLES {
        transaction
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(r#"DELETE FROM "public"."{table}" WHERE "filesystem_id" = $1"#),
                vec![filesystem_id.as_bytes().to_vec().into()],
            ))
            .await?;
    }
    transaction.commit().await
}

async fn snapshot_objects(
    client: &Client,
    bucket: &str,
    prefix: &str,
) -> TestResult<Vec<(String, Vec<u8>)>> {
    let exact = format!("{prefix}/");
    let mut continuation = None;
    let mut objects = Vec::new();
    for _ in 0..100_u32 {
        let output = client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(&exact)
            .set_continuation_token(continuation)
            .max_keys(1_000)
            .send()
            .await?;
        for object in output.contents() {
            let key = object.key().ok_or("listed object omitted key")?.to_owned();
            if !key.starts_with(&exact) {
                return Err("object listing escaped the composition prefix".into());
            }
            let bytes = client
                .get_object()
                .bucket(bucket)
                .key(&key)
                .send()
                .await?
                .body
                .collect()
                .await?
                .into_bytes()
                .to_vec();
            objects.push((key, bytes));
        }
        if !output.is_truncated().unwrap_or(false) {
            objects.sort_by(|left, right| left.0.cmp(&right.0));
            return Ok(objects);
        }
        continuation = Some(
            output
                .next_continuation_token()
                .ok_or("truncated object listing omitted continuation token")?
                .to_owned(),
        );
    }
    Err("composition object listing exceeded 100 pages".into())
}

async fn cleanup_objects(client: &Client, bucket: &str, prefix: &str) -> TestResult {
    for (key, _) in snapshot_objects(client, bucket, prefix).await? {
        client.delete_object().bucket(bucket).key(key).send().await?;
    }
    Ok(())
}

fn required(name: &str) -> TestResult<String> {
    env::var(name).map_err(|_| format!("{name} is required for composition integration").into())
}
