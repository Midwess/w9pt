//! Primary-authoritative one-snapshot record reads and bounded keyset scans.

use core::fmt;

use sea_orm::{DbErr, TryGetable};
use w9pt_fs_state::{
    AdapterFailureKind, DirectoryCookie, DirectoryEntryRecord, DirectoryPage, DirectoryPageEntry,
    EntryName, FilesystemId, InodeId, InodeKind, LockCursor, OpenPinCursor, QidPath, ReadBatch,
    ReadConsistency, ReadOutcome, ReadQuery, ReadResult, RecordKey, RecordRevision, RecordScan,
    ScanBounds, ScanPage, ScanResume, StateLimitError, StateLimitKind, StateLimits, StateRecord,
    StateRevision, StateSnapshot, StateStoreOperation, XattrCursor,
};

use crate::{
    PostgresStateConfig, PostgresStateError,
    database::{PostgresConnection, PostgresRow, PostgresTransaction, query, query_scalar},
    key_codec::SqlRecordKey,
    numeric::{decode_u64, encode_u64},
    row_codec::{
        ContentMetadataRow, DirectoryEntryRow, FilesystemRow, InodeRow, LockRow, MutationRow,
        OpenPinRow, OpenRow, OrphanRow, SqlStateRecord, WriterLeaseRow, XattrRow, XattrStagingRow,
    },
    sqlstate::SqlOperationPhase,
    transaction::{TransactionAccess, begin_transaction, commit_read_transaction},
};

const INODE_FAMILY_TAG: i16 = 2;
const DIRECTORY_ENTRY_FAMILY_TAG: i16 = 3;
const OPEN_FAMILY_TAG: i16 = 4;
const OPEN_PIN_FAMILY_TAG: i16 = 5;
const ORPHAN_FAMILY_TAG: i16 = 6;
const LOCK_FAMILY_TAG: i16 = 7;
const XATTR_FAMILY_TAG: i16 = 8;
const XATTR_STAGING_FAMILY_TAG: i16 = 9;
const MUTATION_FAMILY_TAG: i16 = 10;
const WRITER_LEASE_FAMILY_TAG: i16 = 11;
const CONTENT_METADATA_FAMILY_TAG: i16 = 12;

const FILESYSTEM_POINT_SQL: &str = r#"
SELECT "filesystem_id", "state_revision"::text AS "state_revision",
       "record_revision"::text AS "record_revision", "root_inode_id",
       "next_qid_path"::text AS "next_qid_path",
       "next_directory_cookie"::text AS "next_directory_cookie",
       "policy_generation"::text AS "policy_generation"
FROM "public"."w9pt_fs_state_filesystem_records"
WHERE "filesystem_id" = $1
"#;

const INODE_POINT_SQL: &str = r#"
SELECT "filesystem_id", "inode_id", "qid_path"::text AS "qid_path",
       "record_revision"::text AS "record_revision",
       "mode", "owner", "group_id", "accessed_seconds", "accessed_nanoseconds",
       "modified_seconds", "modified_nanoseconds", "changed_seconds",
       "changed_nanoseconds", "created_seconds", "created_nanoseconds",
       "logical_size"::text AS "logical_size", "link_count"::text AS "link_count",
       "inode_generation"::text AS "inode_generation", "kind", "content_file_id", "content_context_id",
       "data_generation"::text AS "data_generation",
       "content_generation"::text AS "content_generation",
       "content_logical_size"::text AS "content_logical_size", "content_manifest_key",
       "content_manifest_digest", "content_storage_method",
       "directory_generation"::text AS "directory_generation", "directory_parent_inode_id",
       "symlink_target",
       "device_major", "device_minor"
FROM "public"."w9pt_fs_state_inodes"
WHERE "filesystem_id" = $1 AND "inode_id" = $2
"#;

const INODE_BY_QID_PATH_SQL: &str = r#"
SELECT "filesystem_id", "inode_id", "qid_path"::text AS "qid_path",
       "record_revision"::text AS "record_revision",
       "mode", "owner", "group_id", "accessed_seconds", "accessed_nanoseconds",
       "modified_seconds", "modified_nanoseconds", "changed_seconds",
       "changed_nanoseconds", "created_seconds", "created_nanoseconds",
       "logical_size"::text AS "logical_size", "link_count"::text AS "link_count",
       "inode_generation"::text AS "inode_generation", "kind", "content_file_id", "content_context_id",
       "data_generation"::text AS "data_generation",
       "content_generation"::text AS "content_generation",
       "content_logical_size"::text AS "content_logical_size", "content_manifest_key",
       "content_manifest_digest", "content_storage_method",
       "directory_generation"::text AS "directory_generation", "directory_parent_inode_id",
       "symlink_target",
       "device_major", "device_minor"
FROM "public"."w9pt_fs_state_inodes"
WHERE "filesystem_id" = $1 AND "qid_path" = $2::numeric
"#;

const INODE_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "inode_id",
           (320 + octet_length("owner") + octet_length("group_id")
             + CASE
                   WHEN "content_manifest_key" IS NOT NULL
                       THEN octet_length("content_manifest_key")
                   WHEN "symlink_target" IS NOT NULL
                       THEN octet_length("symlink_target")
                   ELSE 0
               END)::bigint AS retained_bytes
    FROM "public"."w9pt_fs_state_inodes"
    WHERE "filesystem_id" = $1 AND ($2::bytea IS NULL OR "inode_id" > $2)
    ORDER BY "inode_id"
    LIMIT $3
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY "inode_id" ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT "inode_id", retained_bytes
FROM sized
WHERE prior_bytes <= $4::numeric
ORDER BY "inode_id"
"#;

const CONTENT_METADATA_POINT_SQL: &str = r#"
SELECT "filesystem_id", "content_file_id", "owner_inode_id", "context_id",
       "policy_format", "policy_bytes", "key_commitment", "wrapped_key_bytes",
       "record_revision"::text AS "record_revision"
FROM "public"."w9pt_fs_state_content_metadata"
WHERE "filesystem_id" = $1 AND "content_file_id" = $2
"#;

const CONTENT_METADATA_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "content_file_id",
           (160 + octet_length("policy_bytes") + COALESCE(octet_length("wrapped_key_bytes"), 0))::bigint AS retained_bytes
    FROM "public"."w9pt_fs_state_content_metadata"
    WHERE "filesystem_id" = $1 AND ($2::bytea IS NULL OR "content_file_id" > $2)
    ORDER BY "content_file_id"
    LIMIT $3
), sized AS (
    SELECT *, COALESCE(sum(retained_bytes) OVER (
        ORDER BY "content_file_id" ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
    ), 0::numeric) AS prior_bytes
    FROM candidates
)
SELECT "content_file_id", retained_bytes FROM sized
WHERE prior_bytes <= $4::numeric
ORDER BY "content_file_id"
"#;

const DIRECTORY_ENTRY_POINT_SQL: &str = r#"
SELECT "filesystem_id", "parent_inode_id", "name", "cookie"::text AS "cookie",
       "child_inode_id", "record_revision"::text AS "record_revision"
FROM "public"."w9pt_fs_state_directory_entries"
WHERE "filesystem_id" = $1 AND "parent_inode_id" = $2 AND "name" = $3
"#;

const DIRECTORY_ENTRY_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "name", "cookie"::text AS cookie_text,
           (160 + 2 * octet_length("name"))::bigint AS retained_bytes
    FROM "public"."w9pt_fs_state_directory_entries"
    WHERE "filesystem_id" = $1 AND "parent_inode_id" = $2
      AND "cookie" > $3::numeric
    ORDER BY "cookie"
    LIMIT $4
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY cookie_text::numeric
            ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT "name", cookie_text, retained_bytes
FROM sized
WHERE prior_bytes <= $5::numeric
ORDER BY cookie_text::numeric
"#;

const DIRECTORY_PAGE_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "entry"."name", "entry"."cookie"::text AS cookie_text,
           "entry"."child_inode_id",
           "entry"."record_revision"::text AS entry_record_revision,
           "child"."kind", "child"."qid_path"::text AS qid_path,
           "child"."record_revision"::text AS child_record_revision,
           (192 + 2 * octet_length("entry"."name"))::bigint AS retained_bytes
    FROM "public"."w9pt_fs_state_directory_entries" AS "entry"
    JOIN "public"."w9pt_fs_state_inodes" AS "child"
      ON "child"."filesystem_id" = "entry"."filesystem_id"
     AND "child"."inode_id" = "entry"."child_inode_id"
    WHERE "entry"."filesystem_id" = $1 AND "entry"."parent_inode_id" = $2
      AND "entry"."cookie" > $3::numeric
    ORDER BY "entry"."cookie"
    LIMIT $4
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY cookie_text::numeric
            ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT * FROM sized
WHERE prior_bytes <= $5::numeric
ORDER BY cookie_text::numeric
"#;

const OPEN_POINT_SQL: &str = r#"
SELECT "filesystem_id", "open_id", "inode_id", "client_incarnation_id", "access",
       "append", "retained_inode_generation"::text AS "retained_inode_generation",
       "record_revision"::text AS "record_revision"
FROM "public"."w9pt_fs_state_opens"
WHERE "filesystem_id" = $1 AND "open_id" = $2
"#;

const OPEN_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "open_id", 192::bigint AS retained_bytes
    FROM "public"."w9pt_fs_state_opens"
    WHERE "filesystem_id" = $1 AND ($2::bytea IS NULL OR "open_id" > $2)
    ORDER BY "open_id"
    LIMIT $3
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY "open_id" ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT "open_id", retained_bytes
FROM sized
WHERE prior_bytes <= $4::numeric
ORDER BY "open_id"
"#;

const OPEN_PIN_POINT_SQL: &str = r#"
SELECT "filesystem_id", "inode_id", "open_id",
       "record_revision"::text AS "record_revision"
FROM "public"."w9pt_fs_state_open_pins"
WHERE "filesystem_id" = $1 AND "inode_id" = $2 AND "open_id" = $3
"#;

const OPEN_PIN_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "inode_id", "open_id", 192::bigint AS retained_bytes
    FROM "public"."w9pt_fs_state_open_pins"
    WHERE "filesystem_id" = $1
      AND ($2::bytea IS NULL OR ("inode_id", "open_id") > ($2, $3))
    ORDER BY "inode_id", "open_id"
    LIMIT $4
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY "inode_id", "open_id"
            ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT "inode_id", "open_id", retained_bytes
FROM sized
WHERE prior_bytes <= $5::numeric
ORDER BY "inode_id", "open_id"
"#;

const ORPHAN_POINT_SQL: &str = r#"
SELECT "filesystem_id", "inode_id", "open_pin_count"::text AS "open_pin_count",
       "orphaned_revision"::text AS "orphaned_revision",
       "record_revision"::text AS "record_revision"
FROM "public"."w9pt_fs_state_orphans"
WHERE "filesystem_id" = $1 AND "inode_id" = $2
"#;

const ORPHAN_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "inode_id", 192::bigint AS retained_bytes
    FROM "public"."w9pt_fs_state_orphans"
    WHERE "filesystem_id" = $1 AND ($2::bytea IS NULL OR "inode_id" > $2)
    ORDER BY "inode_id"
    LIMIT $3
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY "inode_id" ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT "inode_id", retained_bytes
FROM sized
WHERE prior_bytes <= $4::numeric
ORDER BY "inode_id"
"#;

const LOCK_POINT_SQL: &str = r#"
SELECT "filesystem_id", "inode_id", "lock_id", "range_start"::text AS "range_start",
       "range_end"::text AS "range_end", "kind", "owner_client_incarnation_id",
       "owner_open_id", "lock_generation"::text AS "lock_generation",
       "record_revision"::text AS "record_revision"
FROM "public"."w9pt_fs_state_locks"
WHERE "filesystem_id" = $1 AND "inode_id" = $2 AND "lock_id" = $3
"#;

const LOCK_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "inode_id", "lock_id", 192::bigint AS retained_bytes
    FROM "public"."w9pt_fs_state_locks"
    WHERE "filesystem_id" = $1
      AND ($2::bytea IS NULL OR ("inode_id", "lock_id") > ($2, $3))
    ORDER BY "inode_id", "lock_id"
    LIMIT $4
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY "inode_id", "lock_id"
            ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT "inode_id", "lock_id", retained_bytes
FROM sized
WHERE prior_bytes <= $5::numeric
ORDER BY "inode_id", "lock_id"
"#;

const XATTR_POINT_SQL: &str = r#"
SELECT "filesystem_id", "inode_id", "name", "value",
       "record_revision"::text AS "record_revision"
FROM "public"."w9pt_fs_state_xattrs"
WHERE "filesystem_id" = $1 AND "inode_id" = $2 AND "name" = $3
"#;

const XATTR_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "inode_id", "name",
           (160 + 2 * octet_length("name") + octet_length("value"))::bigint
               AS retained_bytes
    FROM "public"."w9pt_fs_state_xattrs"
    WHERE "filesystem_id" = $1
      AND ($2::bytea IS NULL OR ("inode_id", "name") > ($2, $3))
    ORDER BY "inode_id", "name"
    LIMIT $4
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY "inode_id", "name"
            ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT "inode_id", "name", retained_bytes
FROM sized
WHERE prior_bytes <= $5::numeric
ORDER BY "inode_id", "name"
"#;

const XATTR_STAGING_POINT_SQL: &str = r#"
SELECT "filesystem_id", "staging_id", "inode_id", "name",
       "expected_size"::text AS "expected_size", "staged_bytes",
       "record_revision"::text AS "record_revision"
FROM "public"."w9pt_fs_state_xattr_staging"
WHERE "filesystem_id" = $1 AND "staging_id" = $2
"#;

const XATTR_STAGING_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "staging_id",
           (176 + octet_length("name") + octet_length("staged_bytes"))::bigint
               AS retained_bytes
    FROM "public"."w9pt_fs_state_xattr_staging"
    WHERE "filesystem_id" = $1 AND ($2::bytea IS NULL OR "staging_id" > $2)
    ORDER BY "staging_id"
    LIMIT $3
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY "staging_id" ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT "staging_id", retained_bytes
FROM sized
WHERE prior_bytes <= $4::numeric
ORDER BY "staging_id"
"#;

const MUTATION_POINT_SQL: &str = r#"
SELECT "filesystem_id", "mutation_id", "request_fingerprint", "client_incarnation_id",
       "writer_scope_id", "writer_incarnation_id", "fencing_token"::text AS "fencing_token",
       "result_kind", "result_format", "result_bytes",
       "committed_revision"::text AS "committed_revision",
       "retention_horizon"::text AS "retention_horizon",
       "record_revision"::text AS "record_revision"
FROM "public"."w9pt_fs_state_mutation_results"
WHERE "filesystem_id" = $1 AND "mutation_id" = $2
"#;

const MUTATION_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "mutation_id",
           (320 + octet_length("result_bytes"))::bigint AS retained_bytes
    FROM "public"."w9pt_fs_state_mutation_results"
    WHERE "filesystem_id" = $1 AND ($2::bytea IS NULL OR "mutation_id" > $2)
    ORDER BY "mutation_id"
    LIMIT $3
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY "mutation_id" ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT "mutation_id", retained_bytes
FROM sized
WHERE prior_bytes <= $4::numeric
ORDER BY "mutation_id"
"#;

const WRITER_LEASE_POINT_SQL: &str = r#"
SELECT "filesystem_id", "writer_scope_id",
       "greatest_fencing_token"::text AS "greatest_fencing_token", "active_holder_id",
       "active_lease_id", "active_deadline_tick"::text AS "active_deadline_tick",
       "active_record_revision"::text AS "active_record_revision"
FROM "public"."w9pt_fs_state_writer_fences"
WHERE "filesystem_id" = $1 AND "writer_scope_id" = $2
  AND "active_holder_id" IS NOT NULL
"#;

const WRITER_LEASE_SCAN_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT "writer_scope_id", 192::bigint AS retained_bytes
    FROM "public"."w9pt_fs_state_writer_fences"
    WHERE "filesystem_id" = $1 AND "active_holder_id" IS NOT NULL
      AND ($2::bytea IS NULL OR "writer_scope_id" > $2)
    ORDER BY "writer_scope_id"
    LIMIT $3
), sized AS (
    SELECT *, COALESCE(
        sum(retained_bytes) OVER (
            ORDER BY "writer_scope_id"
            ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ), 0::numeric
    ) AS prior_bytes
    FROM candidates
)
SELECT "writer_scope_id", retained_bytes
FROM sized
WHERE prior_bytes <= $4::numeric
ORDER BY "writer_scope_id"
"#;

/// Executes a bounded batch against one serializable primary snapshot.
pub(crate) async fn read_request(
    pool: &PostgresConnection,
    config: PostgresStateConfig,
    request: ReadBatch,
) -> Result<ReadOutcome, PostgresStateError> {
    let limits = config.limits();
    if let Err(error) = validate_read_request(&request, limits) {
        return Ok(ReadOutcome::MalformedRequest(error));
    }

    let mut transaction = begin_transaction(
        pool,
        config,
        TransactionAccess::ReadOnly,
        StateStoreOperation::Read,
    )
    .await?;
    let revision = read_authority_revision(&mut transaction, request.filesystem_id()).await?;
    if let ReadConsistency::AtLeast(required) = request.consistency()
        && revision < required
    {
        commit_read_transaction(transaction, StateStoreOperation::Read).await?;
        return Ok(ReadOutcome::RevisionUnavailable {
            required,
            current: revision,
        });
    }

    let mut results = Vec::with_capacity(request.queries().len());
    for (query_index, query) in request.queries().iter().enumerate() {
        match query {
            ReadQuery::DirectoryPage {
                parent_inode_id,
                after,
                bounds,
            } => match read_directory_page(
                &mut transaction,
                request.filesystem_id(),
                *parent_inode_id,
                *after,
                *bounds,
                limits,
            )
            .await?
            {
                Ok(page) => results.push(ReadResult::DirectoryPage(page)),
                Err(required_bytes) => {
                    commit_read_transaction(transaction, StateStoreOperation::Read).await?;
                    return Ok(ReadOutcome::ScanBoundTooSmall {
                        query_index,
                        required_bytes,
                    });
                }
            },
            ReadQuery::Scan(scan) => {
                match read_scan(&mut transaction, request.filesystem_id(), scan, limits).await? {
                    Ok(page) => results.push(ReadResult::Scan(page)),
                    Err(required_bytes) => {
                        commit_read_transaction(transaction, StateStoreOperation::Read).await?;
                        return Ok(ReadOutcome::ScanBoundTooSmall {
                            query_index,
                            required_bytes,
                        });
                    }
                }
            }
            _ => {
                results.push(
                    read_point(&mut transaction, request.filesystem_id(), query, limits).await?,
                );
            }
        }
    }
    let snapshot = StateSnapshot::new(revision, &request, results)
        .map_err(|error| corruption("invalid state snapshot", error))?;
    commit_read_transaction(transaction, StateStoreOperation::Read).await?;
    Ok(ReadOutcome::Snapshot(snapshot))
}

fn validate_read_request(request: &ReadBatch, limits: StateLimits) -> Result<(), StateLimitError> {
    require_count(
        StateLimitKind::ReadQueries,
        request.queries().len(),
        limits.max_read_queries(),
    )?;
    for query in request.queries() {
        if let Some(key) = query.point_key(request.filesystem_id()) {
            key.validate_against_limits(limits)?;
        }
        let bounds = match query {
            ReadQuery::Scan(scan) => Some(scan.bounds()),
            ReadQuery::DirectoryPage { bounds, .. } => Some(*bounds),
            _ => None,
        };
        let Some(bounds) = bounds else {
            continue;
        };
        require_count(
            StateLimitKind::ScanItems,
            usize::try_from(bounds.max_items()).unwrap_or(usize::MAX),
            limits.max_scan_items(),
        )?;
        require_bytes(
            StateLimitKind::ScanBytes,
            bounds.max_bytes(),
            limits.max_scan_bytes(),
        )?;
        if let ReadQuery::Scan(RecordScan::Xattrs {
            after: Some(cursor),
            ..
        }) = query
        {
            require_bytes(
                StateLimitKind::XattrName,
                cursor.name.as_bytes().len(),
                limits.max_xattr_name_bytes(),
            )?;
        }
    }
    Ok(())
}

async fn read_authority_revision(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
) -> Result<StateRevision, PostgresStateError> {
    let revision: Option<String> = query_scalar(
        r#"SELECT "current_revision"::text
           FROM "public"."w9pt_fs_state_authority_heads"
           WHERE "filesystem_id" = $1"#,
    )
    .bind(filesystem_id.as_bytes().as_slice())
    .fetch_optional(transaction)
    .await
    .map_err(execute_error)?;
    match revision {
        Some(revision) => StateRevision::new(
            decode_u64("current_revision", &revision)
                .map_err(|error| corruption("invalid authority revision", error))?,
        )
        .map_err(|error| corruption("invalid authority revision", error)),
        None => Ok(StateRevision::new(1).expect("revision one is valid")),
    }
}

async fn read_point(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
    request_query: &ReadQuery,
    limits: StateLimits,
) -> Result<ReadResult, PostgresStateError> {
    if let ReadQuery::OpenPinCount(inode_id) = request_query {
        let count: String = query_scalar(
            r#"SELECT pg_catalog.count(*)::numeric(20, 0)::text
               FROM "public"."w9pt_fs_state_open_pins"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
        )
        .bind(filesystem_id.as_bytes().as_slice())
        .bind(inode_id.as_bytes().as_slice())
        .fetch_one(transaction)
        .await
        .map_err(execute_error)?;
        let count = decode_u64("open_pin_count", &count)
            .map_err(|error| corruption("invalid open-pin count", error))?;
        return Ok(ReadResult::OpenPinCount {
            inode_id: *inode_id,
            count,
        });
    }
    if let ReadQuery::InodeByQidPath(qid_path) = request_query {
        let row = query(INODE_BY_QID_PATH_SQL)
            .bind(filesystem_id.as_bytes().as_slice())
            .bind(encode_u64(qid_path.get()))
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| inode_row(&row).map(|row| SqlStateRecord::Inode(Box::new(row))))
            .transpose()?;
        let inode = row
            .map(|row| decode_sql_record(row, limits))
            .transpose()?
            .map(|(key, record)| match (key, record) {
                (RecordKey::Inode(actual_filesystem, _), StateRecord::Inode(inode))
                    if actual_filesystem == filesystem_id && inode.qid_path() == *qid_path =>
                {
                    Ok(Box::new(inode))
                }
                (key, _) => Err(corruption(
                    "QID-path row mismatch",
                    format_args!("query {qid_path:?}, decoded {key:?}"),
                )),
            })
            .transpose()?;
        return Ok(ReadResult::InodeByQidPath {
            qid_path: *qid_path,
            inode,
        });
    }
    if let ReadQuery::InodeWithContentMetadata(inode_id) = request_query {
        let inode_result = Box::pin(read_point(
            transaction,
            filesystem_id,
            &ReadQuery::Inode(*inode_id),
            limits,
        ))
        .await?;
        let ReadResult::Point { record, .. } = inode_result else {
            unreachable!()
        };
        let inode = match record.map(|record| *record) {
            Some(StateRecord::Inode(inode)) => Some(Box::new(inode)),
            None => None,
            _ => return Err(corruption("composite inode row kind", "not inode")),
        };
        let metadata =
            if let Some(file_id) = inode.as_ref().and_then(|inode| inode.content_file_id()) {
                let result = Box::pin(read_point(
                    transaction,
                    filesystem_id,
                    &ReadQuery::ContentMetadata(file_id),
                    limits,
                ))
                .await?;
                let ReadResult::Point { record, .. } = result else {
                    unreachable!()
                };
                match record.map(|record| *record) {
                    Some(StateRecord::ContentMetadata(metadata)) => Some(Box::new(metadata)),
                    None => None,
                    _ => {
                        return Err(corruption(
                            "composite metadata row kind",
                            "not content metadata",
                        ));
                    }
                }
            } else {
                None
            };
        return Ok(ReadResult::InodeWithContentMetadata {
            inode_id: *inode_id,
            inode,
            metadata,
        });
    }
    let key = request_query.point_key(filesystem_id).ok_or_else(|| {
        PostgresStateError::new(
            StateStoreOperation::Read,
            AdapterFailureKind::Internal,
            "scan reached point-read path",
        )
    })?;
    let filesystem = filesystem_id.as_bytes().as_slice();
    let row = match request_query {
        ReadQuery::Filesystem => query(FILESYSTEM_POINT_SQL)
            .bind(filesystem)
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| filesystem_row(&row).map(SqlStateRecord::Filesystem))
            .transpose()?,
        ReadQuery::Inode(inode_id) => query(INODE_POINT_SQL)
            .bind(filesystem)
            .bind(inode_id.as_bytes().as_slice())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| inode_row(&row).map(|row| SqlStateRecord::Inode(Box::new(row))))
            .transpose()?,
        ReadQuery::ContentMetadata(file_id) => query(CONTENT_METADATA_POINT_SQL)
            .bind(filesystem)
            .bind(file_id.as_bytes().as_slice())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| content_metadata_row(&row).map(SqlStateRecord::ContentMetadata))
            .transpose()?,
        ReadQuery::InodeWithContentMetadata(_) => {
            unreachable!("composite read returned before primary-key dispatch")
        }
        ReadQuery::InodeByQidPath(_) => {
            unreachable!("QID lookup returned before primary-key dispatch")
        }
        ReadQuery::DirectoryEntry {
            parent_inode_id,
            name,
        } => query(DIRECTORY_ENTRY_POINT_SQL)
            .bind(filesystem)
            .bind(parent_inode_id.as_bytes().as_slice())
            .bind(name.as_bytes())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| directory_entry_row(&row).map(SqlStateRecord::DirectoryEntry))
            .transpose()?,
        ReadQuery::DirectoryPage { .. } => {
            unreachable!("directory page returned before primary-key dispatch")
        }
        ReadQuery::Open(open_id) => query(OPEN_POINT_SQL)
            .bind(filesystem)
            .bind(open_id.as_bytes().as_slice())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| open_row(&row).map(SqlStateRecord::Open))
            .transpose()?,
        ReadQuery::OpenPin { inode_id, open_id } => query(OPEN_PIN_POINT_SQL)
            .bind(filesystem)
            .bind(inode_id.as_bytes().as_slice())
            .bind(open_id.as_bytes().as_slice())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| open_pin_row(&row).map(SqlStateRecord::OpenPin))
            .transpose()?,
        ReadQuery::OpenPinCount(_) => {
            unreachable!("open-pin count returned before primary-key dispatch")
        }
        ReadQuery::Orphan(inode_id) => query(ORPHAN_POINT_SQL)
            .bind(filesystem)
            .bind(inode_id.as_bytes().as_slice())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| orphan_row(&row).map(SqlStateRecord::Orphan))
            .transpose()?,
        ReadQuery::Lock { inode_id, lock_id } => query(LOCK_POINT_SQL)
            .bind(filesystem)
            .bind(inode_id.as_bytes().as_slice())
            .bind(lock_id.as_bytes().as_slice())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| lock_row(&row).map(SqlStateRecord::Lock))
            .transpose()?,
        ReadQuery::Xattr { inode_id, name } => query(XATTR_POINT_SQL)
            .bind(filesystem)
            .bind(inode_id.as_bytes().as_slice())
            .bind(name.as_bytes())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| xattr_row(&row).map(SqlStateRecord::Xattr))
            .transpose()?,
        ReadQuery::XattrStaging(staging_id) => query(XATTR_STAGING_POINT_SQL)
            .bind(filesystem)
            .bind(staging_id.as_bytes().as_slice())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| xattr_staging_row(&row).map(SqlStateRecord::XattrStaging))
            .transpose()?,
        ReadQuery::Mutation(mutation_id) => query(MUTATION_POINT_SQL)
            .bind(filesystem)
            .bind(mutation_id.as_bytes().as_slice())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| mutation_row(&row).map(SqlStateRecord::Mutation))
            .transpose()?,
        ReadQuery::WriterLease(scope) => query(WRITER_LEASE_POINT_SQL)
            .bind(filesystem)
            .bind(scope.as_bytes().as_slice())
            .fetch_optional(transaction)
            .await
            .map_err(execute_error)?
            .map(|row| writer_lease_row(&row).map(SqlStateRecord::WriterLease))
            .transpose()?,
        ReadQuery::Scan(_) => unreachable!("scan was rejected before point dispatch"),
    };
    let record = row
        .map(|row| decode_sql_record(row, limits))
        .transpose()?
        .map(|(actual_key, record)| {
            if actual_key != key {
                Err(corruption(
                    "point row key mismatch",
                    format_args!("expected {key:?}, decoded {actual_key:?}"),
                ))
            } else {
                Ok(Box::new(record))
            }
        })
        .transpose()?;
    Ok(ReadResult::Point { key, record })
}

/// Fetches one public record inside an existing authoritative transaction.
///
/// Commit code can use this for precondition evaluation after acquiring its
/// deterministic locks. It never opens or completes a transaction itself.
pub(crate) async fn fetch_record(
    transaction: &mut PostgresTransaction,
    key: &RecordKey,
    limits: StateLimits,
    operation: StateStoreOperation,
) -> Result<Option<StateRecord>, PostgresStateError> {
    let query = point_query(key);
    let result = read_point(transaction, key.filesystem_id(), &query, limits)
        .await
        .map_err(|error| {
            if operation == StateStoreOperation::Read {
                error
            } else {
                error.with_operation(operation)
            }
        })?;
    match result {
        ReadResult::Point { record, .. } => Ok(record.map(|record| *record)),
        ReadResult::InodeByQidPath { .. }
        | ReadResult::InodeWithContentMetadata { .. }
        | ReadResult::DirectoryPage(_)
        | ReadResult::OpenPinCount { .. }
        | ReadResult::Scan(_) => Err(PostgresStateError::new(
            StateStoreOperation::Read,
            AdapterFailureKind::Internal,
            "point helper returned a non-point result",
        )),
    }
}

fn point_query(key: &RecordKey) -> ReadQuery {
    match key {
        RecordKey::Filesystem(_) => ReadQuery::Filesystem,
        RecordKey::Inode(_, inode_id) => ReadQuery::Inode(*inode_id),
        RecordKey::ContentMetadata(_, file_id) => ReadQuery::ContentMetadata(*file_id),
        RecordKey::DirectoryEntry(_, parent_inode_id, name) => ReadQuery::DirectoryEntry {
            parent_inode_id: *parent_inode_id,
            name: name.clone(),
        },
        RecordKey::Open(_, open_id) => ReadQuery::Open(*open_id),
        RecordKey::OpenPin(_, inode_id, open_id) => ReadQuery::OpenPin {
            inode_id: *inode_id,
            open_id: *open_id,
        },
        RecordKey::Orphan(_, inode_id) => ReadQuery::Orphan(*inode_id),
        RecordKey::Lock(_, inode_id, lock_id) => ReadQuery::Lock {
            inode_id: *inode_id,
            lock_id: *lock_id,
        },
        RecordKey::Xattr(_, inode_id, name) => ReadQuery::Xattr {
            inode_id: *inode_id,
            name: name.clone(),
        },
        RecordKey::XattrStaging(_, staging_id) => ReadQuery::XattrStaging(*staging_id),
        RecordKey::Mutation(_, mutation_id) => ReadQuery::Mutation(*mutation_id),
        RecordKey::WriterLease(_, scope) => ReadQuery::WriterLease(*scope),
    }
}

async fn read_directory_page(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
    parent_inode_id: InodeId,
    after: DirectoryCookie,
    bounds: ScanBounds,
    limits: StateLimits,
) -> Result<Result<DirectoryPage, usize>, PostgresStateError> {
    let candidate_limit = i64::from(bounds.max_items()) + 1;
    let byte_limit = encode_u64(
        u64::try_from(bounds.max_bytes())
            .map_err(|error| corruption("directory-page byte bound does not fit u64", error))?,
    );
    let rows = query(DIRECTORY_PAGE_SQL)
        .bind(filesystem_id.as_bytes().as_slice())
        .bind(parent_inode_id.as_bytes().as_slice())
        .bind(encode_u64(after.get()))
        .bind(candidate_limit)
        .bind(byte_limit)
        .fetch_all(transaction)
        .await
        .map_err(execute_error)?;
    let maximum = usize::try_from(bounds.max_items()).unwrap_or(usize::MAX);
    let mut entries = Vec::with_capacity(rows.len().min(maximum));
    let mut retained_bytes = 0usize;
    let mut stopped_early = false;
    for row in rows {
        if entries.len() == maximum {
            stopped_early = true;
            break;
        }
        let name = EntryName::new(get::<Vec<u8>>(&row, "name")?, limits)
            .map_err(|error| corruption("invalid directory-page name", error))?;
        let cookie_text: String = get(&row, "cookie_text")?;
        let cookie = DirectoryCookie::new(
            decode_u64("directory-page cookie", &cookie_text)
                .map_err(|error| corruption("invalid directory-page cookie", error))?,
        );
        if cookie == DirectoryCookie::START {
            return Err(corruption("invalid directory-page cookie", "zero"));
        }
        let child_inode_id = decode_inode_id(get(&row, "child_inode_id")?)?;
        let entry_revision_text: String = get(&row, "entry_record_revision")?;
        let entry_revision = RecordRevision::new(
            decode_u64("directory-page entry revision", &entry_revision_text)
                .map_err(|error| corruption("invalid directory-page entry revision", error))?,
        )
        .map_err(|error| corruption("invalid directory-page entry revision", error))?;
        let entry = DirectoryEntryRecord::new(
            parent_inode_id,
            name,
            cookie,
            child_inode_id,
            entry_revision,
        )
        .map_err(|error| corruption("invalid directory-page entry", error))?;
        let kind = decode_inode_kind(get(&row, "kind")?)?;
        let qid_path_text: String = get(&row, "qid_path")?;
        let qid_path = QidPath::new(
            decode_u64("directory-page QID path", &qid_path_text)
                .map_err(|error| corruption("invalid directory-page QID path", error))?,
        )
        .map_err(|error| corruption("invalid directory-page QID path", error))?;
        let child_revision_text: String = get(&row, "child_record_revision")?;
        let child_revision = RecordRevision::new(
            decode_u64("directory-page child revision", &child_revision_text)
                .map_err(|error| corruption("invalid directory-page child revision", error))?,
        )
        .map_err(|error| corruption("invalid directory-page child revision", error))?;
        let page_entry = DirectoryPageEntry::new(entry, kind, qid_path, child_revision);
        let item_bytes = page_entry
            .entry()
            .name()
            .as_bytes()
            .len()
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(192))
            .ok_or_else(|| corruption("directory-page retained-byte estimate overflowed", ""))?;
        let sql_bytes: i64 = get(&row, "retained_bytes")?;
        if usize::try_from(sql_bytes) != Ok(item_bytes) {
            return Err(corruption(
                "directory-page SQL retained-byte estimate disagrees with decoded entry",
                sql_bytes,
            ));
        }
        let next_bytes = retained_bytes
            .checked_add(item_bytes)
            .ok_or_else(|| corruption("directory-page byte total overflowed", ""))?;
        if next_bytes > bounds.max_bytes() {
            if entries.is_empty() {
                return Ok(Err(item_bytes));
            }
            stopped_early = true;
            break;
        }
        retained_bytes = next_bytes;
        entries.push(page_entry);
    }
    let resume = stopped_early
        .then(|| entries.last().map(|entry| entry.entry().cookie()))
        .flatten();
    Ok(Ok(DirectoryPage::new(entries, resume)))
}

fn decode_inode_id(bytes: Vec<u8>) -> Result<InodeId, PostgresStateError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|value: Vec<u8>| {
        corruption("invalid directory-page inode identity width", value.len())
    })?;
    Ok(InodeId::new(bytes))
}

fn decode_inode_kind(tag: i16) -> Result<InodeKind, PostgresStateError> {
    match tag {
        1 => Ok(InodeKind::RegularFile),
        2 => Ok(InodeKind::Directory),
        3 => Ok(InodeKind::Symlink),
        4 => Ok(InodeKind::CharacterDevice),
        5 => Ok(InodeKind::BlockDevice),
        6 => Ok(InodeKind::Fifo),
        7 => Ok(InodeKind::Socket),
        _ => Err(corruption("invalid directory-page inode kind", tag)),
    }
}

async fn read_scan(
    transaction: &mut PostgresTransaction,
    filesystem_id: FilesystemId,
    scan: &RecordScan,
    limits: StateLimits,
) -> Result<Result<ScanPage, usize>, PostgresStateError> {
    let filesystem = filesystem_id.as_bytes().as_slice();
    let limit = i64::from(scan.bounds().max_items()) + 1;
    let byte_limit = encode_u64(
        u64::try_from(scan.bounds().max_bytes())
            .map_err(|error| corruption("scan byte bound does not fit u64", error))?,
    );
    let rows = match scan {
        RecordScan::Inodes { after, .. } => {
            let after = after.map(|value| value.as_bytes().to_vec());
            query(INODE_SCAN_SQL)
                .bind(filesystem)
                .bind(after)
                .bind(limit)
                .bind(byte_limit.clone())
                .fetch_all(transaction)
                .await
                .map_err(execute_error)?
        }
        RecordScan::ContentMetadata { after, .. } => {
            let after = after.map(|value| value.as_bytes().to_vec());
            query(CONTENT_METADATA_SCAN_SQL)
                .bind(filesystem)
                .bind(after)
                .bind(limit)
                .bind(byte_limit.clone())
                .fetch_all(transaction)
                .await
                .map_err(execute_error)?
        }
        RecordScan::DirectoryEntries {
            parent_inode_id,
            after,
            ..
        } => query(DIRECTORY_ENTRY_SCAN_SQL)
            .bind(filesystem)
            .bind(parent_inode_id.as_bytes().as_slice())
            .bind(encode_u64(after.get()))
            .bind(limit)
            .bind(byte_limit.clone())
            .fetch_all(transaction)
            .await
            .map_err(execute_error)?,
        RecordScan::Opens { after, .. } => {
            let after = after.map(|value| value.as_bytes().to_vec());
            query(OPEN_SCAN_SQL)
                .bind(filesystem)
                .bind(after)
                .bind(limit)
                .bind(byte_limit.clone())
                .fetch_all(transaction)
                .await
                .map_err(execute_error)?
        }
        RecordScan::OpenPins { after, .. } => {
            let after_inode = after.map(|value| value.inode_id.as_bytes().to_vec());
            let after_open = after.map(|value| value.open_id.as_bytes().to_vec());
            query(OPEN_PIN_SCAN_SQL)
                .bind(filesystem)
                .bind(after_inode)
                .bind(after_open)
                .bind(limit)
                .bind(byte_limit.clone())
                .fetch_all(transaction)
                .await
                .map_err(execute_error)?
        }
        RecordScan::Orphans { after, .. } => {
            let after = after.map(|value| value.as_bytes().to_vec());
            query(ORPHAN_SCAN_SQL)
                .bind(filesystem)
                .bind(after)
                .bind(limit)
                .bind(byte_limit.clone())
                .fetch_all(transaction)
                .await
                .map_err(execute_error)?
        }
        RecordScan::Locks { after, .. } => {
            let after_inode = after.map(|value| value.inode_id.as_bytes().to_vec());
            let after_lock = after.map(|value| value.lock_id.as_bytes().to_vec());
            query(LOCK_SCAN_SQL)
                .bind(filesystem)
                .bind(after_inode)
                .bind(after_lock)
                .bind(limit)
                .bind(byte_limit.clone())
                .fetch_all(transaction)
                .await
                .map_err(execute_error)?
        }
        RecordScan::Xattrs { after, .. } => {
            let after_inode = after
                .as_ref()
                .map(|value| value.inode_id.as_bytes().to_vec());
            let after_name = after.as_ref().map(|value| value.name.as_bytes().to_vec());
            query(XATTR_SCAN_SQL)
                .bind(filesystem)
                .bind(after_inode)
                .bind(after_name)
                .bind(limit)
                .bind(byte_limit.clone())
                .fetch_all(transaction)
                .await
                .map_err(execute_error)?
        }
        RecordScan::XattrStaging { after, .. } => {
            let after = after.map(|value| value.as_bytes().to_vec());
            query(XATTR_STAGING_SCAN_SQL)
                .bind(filesystem)
                .bind(after)
                .bind(limit)
                .bind(byte_limit.clone())
                .fetch_all(transaction)
                .await
                .map_err(execute_error)?
        }
        RecordScan::Mutations { after, .. } => {
            let after = after.map(|value| value.as_bytes().to_vec());
            query(MUTATION_SCAN_SQL)
                .bind(filesystem)
                .bind(after)
                .bind(limit)
                .bind(byte_limit.clone())
                .fetch_all(transaction)
                .await
                .map_err(execute_error)?
        }
        RecordScan::WriterLeases { after, .. } => {
            let after = after.map(|value| value.as_bytes().to_vec());
            query(WRITER_LEASE_SCAN_SQL)
                .bind(filesystem)
                .bind(after)
                .bind(limit)
                .bind(byte_limit)
                .fetch_all(transaction)
                .await
                .map_err(execute_error)?
        }
    };
    let candidates = rows
        .iter()
        .map(|row| decode_scan_candidate(row, filesystem_id, scan, limits))
        .collect::<Result<Vec<_>, _>>()?;
    let (candidates, resume) = match plan_scan_candidates(scan, candidates)? {
        CandidatePlan::Selected { candidates, resume } => (candidates, resume),
        CandidatePlan::TooSmall(required_bytes) => return Ok(Err(required_bytes)),
    };

    let mut records = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let record = fetch_record(
            transaction,
            &candidate.key,
            limits,
            StateStoreOperation::Read,
        )
        .await?
        .ok_or_else(|| {
            corruption(
                "scan candidate disappeared inside one read snapshot",
                format_args!("key {:?}", candidate.key),
            )
        })?;
        let actual_bytes = retained_item_bytes(&candidate.key, &record).ok_or_else(|| {
            corruption(
                "scan retained-byte estimate overflowed",
                format_args!("key {:?}", candidate.key),
            )
        })?;
        if actual_bytes != candidate.retained_bytes {
            return Err(corruption(
                "SQL scan retained-byte preflight disagrees with decoded record",
                format_args!(
                    "key {:?}: SQL {}, decoded {}",
                    candidate.key, candidate.retained_bytes, actual_bytes
                ),
            ));
        }
        if scan_resume(&candidate.key, &record).as_ref() != Some(&candidate.cursor) {
            return Err(corruption(
                "scan candidate cursor disagrees with decoded record",
                format_args!("key {:?}", candidate.key),
            ));
        }
        records.push((candidate.key, record));
    }
    let page = ScanPage::new(scan.family(), records, resume)
        .map_err(|error| corruption("invalid scan page", error))?;
    Ok(Ok(page))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScanCandidate {
    key: RecordKey,
    cursor: ScanResume,
    retained_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CandidatePlan {
    Selected {
        candidates: Vec<ScanCandidate>,
        resume: Option<ScanResume>,
    },
    TooSmall(usize),
}

fn decode_scan_candidate(
    row: &PostgresRow,
    filesystem_id: FilesystemId,
    scan: &RecordScan,
    limits: StateLimits,
) -> Result<ScanCandidate, PostgresStateError> {
    let retained_bytes: i64 = get(row, "retained_bytes")?;
    let retained_bytes = usize::try_from(retained_bytes)
        .map_err(|_| corruption("invalid SQL scan retained-byte estimate", retained_bytes))?;
    if retained_bytes == 0 {
        return Err(corruption(
            "invalid SQL scan retained-byte estimate",
            "zero",
        ));
    }

    let (family_tag, component_a, component_b, directory_cookie) = match scan {
        RecordScan::Inodes { .. } => (INODE_FAMILY_TAG, get(row, "inode_id")?, Vec::new(), None),
        RecordScan::ContentMetadata { .. } => (
            CONTENT_METADATA_FAMILY_TAG,
            get(row, "content_file_id")?,
            Vec::new(),
            None,
        ),
        RecordScan::DirectoryEntries {
            parent_inode_id, ..
        } => {
            let cookie: String = get(row, "cookie_text")?;
            let cookie = decode_u64("directory_entries.cookie", &cookie)
                .map_err(|error| corruption("invalid directory scan cookie", error))?;
            if cookie == 0 {
                return Err(corruption("invalid directory scan cookie", "zero"));
            }
            (
                DIRECTORY_ENTRY_FAMILY_TAG,
                parent_inode_id.as_bytes().to_vec(),
                get(row, "name")?,
                Some(w9pt_fs_state::DirectoryCookie::new(cookie)),
            )
        }
        RecordScan::Opens { .. } => (OPEN_FAMILY_TAG, get(row, "open_id")?, Vec::new(), None),
        RecordScan::OpenPins { .. } => (
            OPEN_PIN_FAMILY_TAG,
            get(row, "inode_id")?,
            get(row, "open_id")?,
            None,
        ),
        RecordScan::Orphans { .. } => (ORPHAN_FAMILY_TAG, get(row, "inode_id")?, Vec::new(), None),
        RecordScan::Locks { .. } => (
            LOCK_FAMILY_TAG,
            get(row, "inode_id")?,
            get(row, "lock_id")?,
            None,
        ),
        RecordScan::Xattrs { .. } => (
            XATTR_FAMILY_TAG,
            get(row, "inode_id")?,
            get(row, "name")?,
            None,
        ),
        RecordScan::XattrStaging { .. } => (
            XATTR_STAGING_FAMILY_TAG,
            get(row, "staging_id")?,
            Vec::new(),
            None,
        ),
        RecordScan::Mutations { .. } => (
            MUTATION_FAMILY_TAG,
            get(row, "mutation_id")?,
            Vec::new(),
            None,
        ),
        RecordScan::WriterLeases { .. } => (
            WRITER_LEASE_FAMILY_TAG,
            get(row, "writer_scope_id")?,
            Vec::new(),
            None,
        ),
    };
    let key = SqlRecordKey {
        family_tag,
        filesystem_id: *filesystem_id.as_bytes(),
        component_a,
        component_b,
    }
    .decode(limits)
    .map_err(|error| corruption("invalid SQL scan candidate key", error))?;
    let cursor = match (&key, directory_cookie) {
        (RecordKey::Inode(_, inode_id), None) => ScanResume::Inode(*inode_id),
        (RecordKey::ContentMetadata(_, file_id), None) => ScanResume::ContentMetadata(*file_id),
        (RecordKey::DirectoryEntry(_, _, _), Some(cookie)) => ScanResume::DirectoryEntry(cookie),
        (RecordKey::Open(_, open_id), None) => ScanResume::Open(*open_id),
        (RecordKey::OpenPin(_, inode_id, open_id), None) => ScanResume::OpenPin(OpenPinCursor {
            inode_id: *inode_id,
            open_id: *open_id,
        }),
        (RecordKey::Orphan(_, inode_id), None) => ScanResume::Orphan(*inode_id),
        (RecordKey::Lock(_, inode_id, lock_id), None) => ScanResume::Lock(LockCursor {
            inode_id: *inode_id,
            lock_id: *lock_id,
        }),
        (RecordKey::Xattr(_, inode_id, name), None) => ScanResume::Xattr(XattrCursor {
            inode_id: *inode_id,
            name: name.clone(),
        }),
        (RecordKey::XattrStaging(_, staging_id), None) => ScanResume::XattrStaging(*staging_id),
        (RecordKey::Mutation(_, mutation_id), None) => ScanResume::Mutation(*mutation_id),
        (RecordKey::WriterLease(_, scope), None) => ScanResume::WriterLease(*scope),
        _ => {
            return Err(corruption(
                "SQL scan candidate family does not match scan",
                format_args!("key {key:?}"),
            ));
        }
    };
    if cursor.family() != scan.family() {
        return Err(corruption(
            "SQL scan candidate cursor does not match scan",
            format_args!("cursor {cursor:?}, scan {:?}", scan.family()),
        ));
    }
    Ok(ScanCandidate {
        key,
        cursor,
        retained_bytes,
    })
}

fn plan_scan_candidates(
    scan: &RecordScan,
    candidates: Vec<ScanCandidate>,
) -> Result<CandidatePlan, PostgresStateError> {
    let maximum = usize::try_from(scan.bounds().max_items()).unwrap_or(usize::MAX);
    let candidate_count = candidates.len();
    if candidate_count > maximum.saturating_add(1) {
        return Err(corruption(
            "scan preflight exceeded item lookahead bound",
            candidate_count,
        ));
    }
    let mut selected = Vec::with_capacity(candidate_count.min(maximum));
    let mut retained_bytes = 0usize;
    let mut previous = None;
    let mut stopped_early = false;
    for candidate in candidates {
        if previous
            .as_ref()
            .is_some_and(|cursor| cursor >= &candidate.cursor)
        {
            return Err(corruption(
                "scan preflight cursors are not strictly ordered",
                format_args!("previous {previous:?}, next {:?}", candidate.cursor),
            ));
        }
        previous = Some(candidate.cursor.clone());
        if selected.len() == maximum {
            stopped_early = true;
            break;
        }
        let next_bytes = retained_bytes
            .checked_add(candidate.retained_bytes)
            .ok_or_else(|| corruption("scan aggregate retained-byte estimate overflowed", ""))?;
        if next_bytes > scan.bounds().max_bytes() {
            if selected.is_empty() {
                return Ok(CandidatePlan::TooSmall(candidate.retained_bytes));
            }
            stopped_early = true;
            break;
        }
        retained_bytes = next_bytes;
        selected.push(candidate);
    }
    let resume = stopped_early
        .then(|| selected.last().map(|candidate| candidate.cursor.clone()))
        .flatten();
    Ok(CandidatePlan::Selected {
        candidates: selected,
        resume,
    })
}

fn scan_resume(key: &RecordKey, record: &StateRecord) -> Option<ScanResume> {
    match (key, record) {
        (RecordKey::Inode(_, inode_id), StateRecord::Inode(_)) => {
            Some(ScanResume::Inode(*inode_id))
        }
        (RecordKey::ContentMetadata(_, file_id), StateRecord::ContentMetadata(_)) => {
            Some(ScanResume::ContentMetadata(*file_id))
        }
        (RecordKey::DirectoryEntry(_, _, _), StateRecord::DirectoryEntry(entry)) => {
            Some(ScanResume::DirectoryEntry(entry.cookie()))
        }
        (RecordKey::Open(_, open_id), StateRecord::Open(_)) => Some(ScanResume::Open(*open_id)),
        (RecordKey::OpenPin(_, inode_id, open_id), StateRecord::OpenPin(_)) => {
            Some(ScanResume::OpenPin(OpenPinCursor {
                inode_id: *inode_id,
                open_id: *open_id,
            }))
        }
        (RecordKey::Orphan(_, inode_id), StateRecord::Orphan(_)) => {
            Some(ScanResume::Orphan(*inode_id))
        }
        (RecordKey::Lock(_, inode_id, lock_id), StateRecord::Lock(_)) => {
            Some(ScanResume::Lock(LockCursor {
                inode_id: *inode_id,
                lock_id: *lock_id,
            }))
        }
        (RecordKey::Xattr(_, inode_id, name), StateRecord::Xattr(_)) => {
            Some(ScanResume::Xattr(XattrCursor {
                inode_id: *inode_id,
                name: name.clone(),
            }))
        }
        (RecordKey::XattrStaging(_, staging_id), StateRecord::XattrStaging(_)) => {
            Some(ScanResume::XattrStaging(*staging_id))
        }
        (RecordKey::Mutation(_, mutation_id), StateRecord::Mutation(_)) => {
            Some(ScanResume::Mutation(*mutation_id))
        }
        (RecordKey::WriterLease(_, scope), StateRecord::WriterLease(_)) => {
            Some(ScanResume::WriterLease(*scope))
        }
        _ => None,
    }
}

fn retained_item_bytes(key: &RecordKey, record: &StateRecord) -> Option<usize> {
    retained_key_bytes(key)?.checked_add(retained_record_bytes(record)?)
}

fn retained_key_bytes(key: &RecordKey) -> Option<usize> {
    let variable = match key {
        RecordKey::DirectoryEntry(_, _, name) => name.as_bytes().len(),
        RecordKey::Xattr(_, _, name) => name.as_bytes().len(),
        _ => 0,
    };
    64usize.checked_add(variable)
}

fn retained_record_bytes(record: &StateRecord) -> Option<usize> {
    let (fixed, variable) = match record {
        StateRecord::Filesystem(_) => (128usize, 0usize),
        StateRecord::Inode(record) => {
            let mut variable = record.owner().as_bytes().len();
            variable = variable.checked_add(record.group().as_bytes().len())?;
            match record.data() {
                w9pt_fs_state::InodeData::RegularFile {
                    content: Some(content),
                    ..
                } => variable = variable.checked_add(content.manifest_key().as_str().len())?,
                w9pt_fs_state::InodeData::Symlink { target } => {
                    variable = variable.checked_add(target.as_bytes().len())?;
                }
                _ => {}
            }
            (256, variable)
        }
        StateRecord::ContentMetadata(record) => (
            96,
            record
                .policy_bytes()
                .len()
                .checked_add(record.wrapped_key_bytes().map_or(0, <[u8]>::len))?,
        ),
        StateRecord::DirectoryEntry(record) => (96, record.name().as_bytes().len()),
        StateRecord::Open(_)
        | StateRecord::OpenPin(_)
        | StateRecord::Orphan(_)
        | StateRecord::Lock(_) => (128, 0),
        StateRecord::Xattr(record) => (
            96,
            record
                .name()
                .as_bytes()
                .len()
                .checked_add(record.value().as_bytes().len())?,
        ),
        StateRecord::XattrStaging(record) => (
            112,
            record
                .name()
                .as_bytes()
                .len()
                .checked_add(record.bytes().as_bytes().len())?,
        ),
        StateRecord::Mutation(record) => (256, record.result().bytes().len()),
        StateRecord::WriterLease(_) => (128, 0),
    };
    fixed.checked_add(variable)
}

fn decode_sql_record(
    row: SqlStateRecord,
    limits: StateLimits,
) -> Result<(RecordKey, StateRecord), PostgresStateError> {
    row.decode(limits)
        .map_err(|error| corruption("invalid public record row", error))
}

fn filesystem_row(row: &PostgresRow) -> Result<FilesystemRow, PostgresStateError> {
    Ok(FilesystemRow {
        filesystem_id: get(row, "filesystem_id")?,
        state_revision: get(row, "state_revision")?,
        record_revision: get(row, "record_revision")?,
        root_inode_id: get(row, "root_inode_id")?,
        next_qid_path: get(row, "next_qid_path")?,
        next_directory_cookie: get(row, "next_directory_cookie")?,
        policy_generation: get(row, "policy_generation")?,
    })
}

fn inode_row(row: &PostgresRow) -> Result<InodeRow, PostgresStateError> {
    Ok(InodeRow {
        filesystem_id: get(row, "filesystem_id")?,
        inode_id: get(row, "inode_id")?,
        qid_path: get(row, "qid_path")?,
        record_revision: get(row, "record_revision")?,
        mode: get(row, "mode")?,
        owner: get(row, "owner")?,
        group_id: get(row, "group_id")?,
        accessed_seconds: get(row, "accessed_seconds")?,
        accessed_nanoseconds: get(row, "accessed_nanoseconds")?,
        modified_seconds: get(row, "modified_seconds")?,
        modified_nanoseconds: get(row, "modified_nanoseconds")?,
        changed_seconds: get(row, "changed_seconds")?,
        changed_nanoseconds: get(row, "changed_nanoseconds")?,
        created_seconds: get(row, "created_seconds")?,
        created_nanoseconds: get(row, "created_nanoseconds")?,
        logical_size: get(row, "logical_size")?,
        link_count: get(row, "link_count")?,
        inode_generation: get(row, "inode_generation")?,
        kind: get(row, "kind")?,
        content_file_id: get(row, "content_file_id")?,
        content_context_id: get(row, "content_context_id")?,
        data_generation: get(row, "data_generation")?,
        content_generation: get(row, "content_generation")?,
        content_logical_size: get(row, "content_logical_size")?,
        content_manifest_key: get(row, "content_manifest_key")?,
        content_manifest_digest: get(row, "content_manifest_digest")?,
        content_storage_method: get(row, "content_storage_method")?,
        directory_generation: get(row, "directory_generation")?,
        directory_parent_inode_id: get(row, "directory_parent_inode_id")?,
        symlink_target: get(row, "symlink_target")?,
        device_major: get(row, "device_major")?,
        device_minor: get(row, "device_minor")?,
    })
}

fn content_metadata_row(row: &PostgresRow) -> Result<ContentMetadataRow, PostgresStateError> {
    Ok(ContentMetadataRow {
        filesystem_id: get(row, "filesystem_id")?,
        content_file_id: get(row, "content_file_id")?,
        owner_inode_id: get(row, "owner_inode_id")?,
        context_id: get(row, "context_id")?,
        policy_format: get(row, "policy_format")?,
        policy_bytes: get(row, "policy_bytes")?,
        key_commitment: get(row, "key_commitment")?,
        wrapped_key_bytes: get(row, "wrapped_key_bytes")?,
        record_revision: get(row, "record_revision")?,
    })
}

fn directory_entry_row(row: &PostgresRow) -> Result<DirectoryEntryRow, PostgresStateError> {
    Ok(DirectoryEntryRow {
        filesystem_id: get(row, "filesystem_id")?,
        parent_inode_id: get(row, "parent_inode_id")?,
        name: get(row, "name")?,
        cookie: get(row, "cookie")?,
        child_inode_id: get(row, "child_inode_id")?,
        record_revision: get(row, "record_revision")?,
    })
}

fn open_row(row: &PostgresRow) -> Result<OpenRow, PostgresStateError> {
    Ok(OpenRow {
        filesystem_id: get(row, "filesystem_id")?,
        open_id: get(row, "open_id")?,
        inode_id: get(row, "inode_id")?,
        client_incarnation_id: get(row, "client_incarnation_id")?,
        access: get(row, "access")?,
        append: get(row, "append")?,
        retained_inode_generation: get(row, "retained_inode_generation")?,
        record_revision: get(row, "record_revision")?,
    })
}

fn open_pin_row(row: &PostgresRow) -> Result<OpenPinRow, PostgresStateError> {
    Ok(OpenPinRow {
        filesystem_id: get(row, "filesystem_id")?,
        inode_id: get(row, "inode_id")?,
        open_id: get(row, "open_id")?,
        record_revision: get(row, "record_revision")?,
    })
}

fn orphan_row(row: &PostgresRow) -> Result<OrphanRow, PostgresStateError> {
    Ok(OrphanRow {
        filesystem_id: get(row, "filesystem_id")?,
        inode_id: get(row, "inode_id")?,
        open_pin_count: get(row, "open_pin_count")?,
        orphaned_revision: get(row, "orphaned_revision")?,
        record_revision: get(row, "record_revision")?,
    })
}

fn lock_row(row: &PostgresRow) -> Result<LockRow, PostgresStateError> {
    Ok(LockRow {
        filesystem_id: get(row, "filesystem_id")?,
        inode_id: get(row, "inode_id")?,
        lock_id: get(row, "lock_id")?,
        range_start: get(row, "range_start")?,
        range_end: get(row, "range_end")?,
        kind: get(row, "kind")?,
        owner_client_incarnation_id: get(row, "owner_client_incarnation_id")?,
        owner_open_id: get(row, "owner_open_id")?,
        lock_generation: get(row, "lock_generation")?,
        record_revision: get(row, "record_revision")?,
    })
}

fn xattr_row(row: &PostgresRow) -> Result<XattrRow, PostgresStateError> {
    Ok(XattrRow {
        filesystem_id: get(row, "filesystem_id")?,
        inode_id: get(row, "inode_id")?,
        name: get(row, "name")?,
        value: get(row, "value")?,
        record_revision: get(row, "record_revision")?,
    })
}

fn xattr_staging_row(row: &PostgresRow) -> Result<XattrStagingRow, PostgresStateError> {
    Ok(XattrStagingRow {
        filesystem_id: get(row, "filesystem_id")?,
        staging_id: get(row, "staging_id")?,
        inode_id: get(row, "inode_id")?,
        name: get(row, "name")?,
        expected_size: get(row, "expected_size")?,
        staged_bytes: get(row, "staged_bytes")?,
        record_revision: get(row, "record_revision")?,
    })
}

fn mutation_row(row: &PostgresRow) -> Result<MutationRow, PostgresStateError> {
    Ok(MutationRow {
        filesystem_id: get(row, "filesystem_id")?,
        mutation_id: get(row, "mutation_id")?,
        request_fingerprint: get(row, "request_fingerprint")?,
        client_incarnation_id: get(row, "client_incarnation_id")?,
        writer_scope_id: get(row, "writer_scope_id")?,
        writer_incarnation_id: get(row, "writer_incarnation_id")?,
        fencing_token: get(row, "fencing_token")?,
        result_kind: get(row, "result_kind")?,
        result_format: get(row, "result_format")?,
        result_bytes: get(row, "result_bytes")?,
        committed_revision: get(row, "committed_revision")?,
        retention_horizon: get(row, "retention_horizon")?,
        record_revision: get(row, "record_revision")?,
    })
}

fn writer_lease_row(row: &PostgresRow) -> Result<WriterLeaseRow, PostgresStateError> {
    Ok(WriterLeaseRow {
        filesystem_id: get(row, "filesystem_id")?,
        writer_scope_id: get(row, "writer_scope_id")?,
        greatest_fencing_token: get(row, "greatest_fencing_token")?,
        active_holder_id: get(row, "active_holder_id")?,
        active_lease_id: get(row, "active_lease_id")?,
        active_deadline_tick: get(row, "active_deadline_tick")?,
        active_record_revision: get(row, "active_record_revision")?,
    })
}

fn get<T>(row: &PostgresRow, column: &'static str) -> Result<T, PostgresStateError>
where
    T: TryGetable,
{
    row.try_get(column).map_err(|error| {
        PostgresStateError::from_database(
            StateStoreOperation::Read,
            SqlOperationPhase::DecodeRow,
            error,
        )
    })
}

fn require_count(kind: StateLimitKind, actual: usize, maximum: u32) -> Result<(), StateLimitError> {
    let actual = u64::try_from(actual).unwrap_or(u64::MAX);
    if actual > u64::from(maximum) {
        Err(StateLimitError::new(kind, actual, u64::from(maximum)))
    } else {
        Ok(())
    }
}

fn require_bytes(
    kind: StateLimitKind,
    actual: usize,
    maximum: usize,
) -> Result<(), StateLimitError> {
    let actual = u64::try_from(actual).unwrap_or(u64::MAX);
    let maximum = u64::try_from(maximum).unwrap_or(u64::MAX);
    if actual > maximum {
        Err(StateLimitError::new(kind, actual, maximum))
    } else {
        Ok(())
    }
}

fn execute_error(error: DbErr) -> PostgresStateError {
    PostgresStateError::from_database(
        StateStoreOperation::Read,
        SqlOperationPhase::ExecuteStatement,
        error,
    )
}

fn corruption(context: &'static str, error: impl fmt::Display) -> PostgresStateError {
    PostgresStateError::new(
        StateStoreOperation::Read,
        AdapterFailureKind::Corruption,
        format!("{context}: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectOptions, Database};
    use w9pt_fs_state::{
        EntryName, GroupId, InodeData, InodeGeneration, InodeId, InodeRecord, InodeTimes, LockId,
        OpenId, PrincipalId, ReadConsistency, ScanBounds, UnixTimestamp, WriterScopeId, XattrName,
        XattrRecord, XattrStagingId, XattrValue,
    };
    use w9pt_fs_storage::MutationId;

    fn times() -> InodeTimes {
        let time = UnixTimestamp::new(0, 0).unwrap();
        InodeTimes {
            accessed: time,
            modified: time,
            changed: time,
            created: time,
        }
    }

    #[test]
    fn receiver_revalidates_batch_scan_and_cursor_limits() {
        let strict = StateLimits::new(w9pt_fs_state::StateLimitValues {
            max_read_queries: 1,
            max_scan_items: 1,
            max_scan_bytes: 128,
            max_xattr_name_bytes: 1,
            ..w9pt_fs_state::StateLimitValues::default()
        })
        .unwrap();
        let loose = StateLimits::new(w9pt_fs_state::StateLimitValues {
            max_read_queries: 2,
            max_scan_items: 2,
            max_scan_bytes: 256,
            max_xattr_name_bytes: 2,
            ..w9pt_fs_state::StateLimitValues::default()
        })
        .unwrap();
        let filesystem_id = FilesystemId::from_u128(1);
        let batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Filesystem, ReadQuery::Filesystem],
            loose,
        )
        .unwrap();
        assert!(matches!(
            validate_read_request(&batch, strict),
            Err(StateLimitError {
                kind: StateLimitKind::ReadQueries,
                ..
            })
        ));
        let bounds = ScanBounds::new(2, 1, loose).unwrap();
        let batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Scan(RecordScan::Inodes {
                after: None,
                bounds,
            })],
            loose,
        )
        .unwrap();
        assert!(matches!(
            validate_read_request(&batch, strict),
            Err(StateLimitError {
                kind: StateLimitKind::ScanItems,
                ..
            })
        ));
        let cursor = XattrCursor {
            inode_id: InodeId::from_u128(2),
            name: XattrName::new(b"xx".to_vec(), loose).unwrap(),
        };
        let batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![ReadQuery::Scan(RecordScan::Xattrs {
                after: Some(cursor),
                bounds: ScanBounds::new(1, 1, loose).unwrap(),
            })],
            loose,
        )
        .unwrap();
        assert!(matches!(
            validate_read_request(&batch, strict),
            Err(StateLimitError {
                kind: StateLimitKind::XattrName,
                ..
            })
        ));
    }

    #[test]
    fn retained_sizes_match_the_contract_formulas() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let inode_id = InodeId::from_u128(2);
        let inode = StateRecord::Inode(
            InodeRecord::new(
                inode_id,
                w9pt_fs_state::QidPath::new(1).unwrap(),
                w9pt_fs_state::RecordRevision::new(1).unwrap(),
                0o644,
                PrincipalId::new(b"owner".to_vec(), limits).unwrap(),
                GroupId::new(b"group".to_vec(), limits).unwrap(),
                times(),
                0,
                1,
                InodeGeneration::new(1).unwrap(),
                InodeData::Fifo,
            )
            .unwrap(),
        );
        assert_eq!(
            retained_item_bytes(&RecordKey::Inode(filesystem_id, inode_id), &inode),
            Some(64 + 256 + 5 + 5)
        );
        let name = XattrName::new(b"name".to_vec(), limits).unwrap();
        let xattr = StateRecord::Xattr(XattrRecord::new(
            inode_id,
            name.clone(),
            XattrValue::new(b"value".to_vec(), limits).unwrap(),
            w9pt_fs_state::RecordRevision::new(1).unwrap(),
        ));
        assert_eq!(
            retained_item_bytes(&RecordKey::Xattr(filesystem_id, inode_id, name), &xattr),
            Some((64 + 4) + (96 + 4 + 5))
        );
    }

    #[test]
    fn scan_candidate_budgeting_returns_resume_or_exact_too_small_size() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let mut pairs = Vec::new();
        for value in [2, 3] {
            let inode_id = InodeId::from_u128(value);
            let key = RecordKey::Inode(filesystem_id, inode_id);
            let record = StateRecord::Inode(
                InodeRecord::new(
                    inode_id,
                    w9pt_fs_state::QidPath::new(1).unwrap(),
                    w9pt_fs_state::RecordRevision::new(1).unwrap(),
                    0o644,
                    PrincipalId::new(b"o".to_vec(), limits).unwrap(),
                    GroupId::new(b"g".to_vec(), limits).unwrap(),
                    times(),
                    0,
                    1,
                    InodeGeneration::new(1).unwrap(),
                    InodeData::Fifo,
                )
                .unwrap(),
            );
            pairs.push((key, record));
        }
        let candidates = pairs
            .iter()
            .map(|(key, record)| ScanCandidate {
                key: key.clone(),
                cursor: scan_resume(key, record).unwrap(),
                retained_bytes: retained_item_bytes(key, record).unwrap(),
            })
            .collect();
        let scan = RecordScan::Inodes {
            after: None,
            bounds: ScanBounds::new(1, limits.max_scan_bytes(), limits).unwrap(),
        };
        let CandidatePlan::Selected { candidates, resume } =
            plan_scan_candidates(&scan, candidates).unwrap()
        else {
            panic!("expected selected candidates");
        };
        assert_eq!(candidates.len(), 1);
        assert_eq!(resume, Some(ScanResume::Inode(InodeId::from_u128(2))));

        let first_size = retained_item_bytes(&pairs[0].0, &pairs[0].1).unwrap();
        let candidates = vec![ScanCandidate {
            key: pairs[0].0.clone(),
            cursor: scan_resume(&pairs[0].0, &pairs[0].1).unwrap(),
            retained_bytes: first_size,
        }];
        let too_small = RecordScan::Inodes {
            after: None,
            bounds: ScanBounds::new(1, first_size - 1, limits).unwrap(),
        };
        assert_eq!(
            plan_scan_candidates(&too_small, candidates).unwrap(),
            CandidatePlan::TooSmall(first_size)
        );

        let first = ScanCandidate {
            key: pairs[0].0.clone(),
            cursor: scan_resume(&pairs[0].0, &pairs[0].1).unwrap(),
            retained_bytes: first_size,
        };
        let second = ScanCandidate {
            key: pairs[1].0.clone(),
            cursor: scan_resume(&pairs[1].0, &pairs[1].1).unwrap(),
            retained_bytes: limits.max_scan_bytes(),
        };
        let byte_limited = RecordScan::Inodes {
            after: None,
            bounds: ScanBounds::new(2, first_size, limits).unwrap(),
        };
        assert_eq!(
            plan_scan_candidates(&byte_limited, vec![first, second]).unwrap(),
            CandidatePlan::Selected {
                candidates: vec![ScanCandidate {
                    key: pairs[0].0.clone(),
                    cursor: scan_resume(&pairs[0].0, &pairs[0].1).unwrap(),
                    retained_bytes: first_size,
                }],
                resume: Some(ScanResume::Inode(InodeId::from_u128(2))),
            }
        );
    }

    #[test]
    fn scan_cursor_mapping_uses_exact_finalized_identities() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let parent = InodeId::from_u128(2);
        let name = EntryName::new(b"entry".to_vec(), limits).unwrap();
        let entry = w9pt_fs_state::DirectoryEntryRecord::new(
            parent,
            name.clone(),
            w9pt_fs_state::DirectoryCookie::new(9),
            InodeId::from_u128(3),
            w9pt_fs_state::RecordRevision::new(1).unwrap(),
        )
        .unwrap();
        assert_eq!(
            scan_resume(
                &RecordKey::DirectoryEntry(filesystem_id, parent, name),
                &StateRecord::DirectoryEntry(entry)
            ),
            Some(ScanResume::DirectoryEntry(
                w9pt_fs_state::DirectoryCookie::new(9)
            ))
        );
    }

    #[test]
    fn every_record_key_maps_back_to_its_exact_point_query() {
        let limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let inode_id = InodeId::from_u128(2);
        let open_id = w9pt_fs_state::OpenId::from_u128(3);
        let keys = vec![
            RecordKey::Filesystem(filesystem_id),
            RecordKey::Inode(filesystem_id, inode_id),
            RecordKey::DirectoryEntry(
                filesystem_id,
                inode_id,
                EntryName::new(b"entry".to_vec(), limits).unwrap(),
            ),
            RecordKey::Open(filesystem_id, open_id),
            RecordKey::OpenPin(filesystem_id, inode_id, open_id),
            RecordKey::Orphan(filesystem_id, inode_id),
            RecordKey::Lock(filesystem_id, inode_id, w9pt_fs_state::LockId::from_u128(4)),
            RecordKey::Xattr(
                filesystem_id,
                inode_id,
                XattrName::new(b"name".to_vec(), limits).unwrap(),
            ),
            RecordKey::XattrStaging(filesystem_id, w9pt_fs_state::XattrStagingId::from_u128(5)),
            RecordKey::Mutation(filesystem_id, w9pt_fs_storage::MutationId::from_u128(6)),
            RecordKey::WriterLease(filesystem_id, w9pt_fs_state::WriterScopeId::from_u128(7)),
        ];
        for key in keys {
            assert_eq!(point_query(&key).point_key(filesystem_id), Some(key));
        }
    }

    #[test]
    fn every_scan_query_is_static_keyset_sql_without_offset() {
        for sql in [
            INODE_SCAN_SQL,
            DIRECTORY_ENTRY_SCAN_SQL,
            DIRECTORY_PAGE_SQL,
            OPEN_SCAN_SQL,
            OPEN_PIN_SCAN_SQL,
            ORPHAN_SCAN_SQL,
            LOCK_SCAN_SQL,
            XATTR_SCAN_SQL,
            XATTR_STAGING_SCAN_SQL,
            MUTATION_SCAN_SQL,
            WRITER_LEASE_SCAN_SQL,
        ] {
            assert!(sql.contains("ORDER BY"));
            assert!(sql.contains("LIMIT"));
            assert!(sql.contains("AS MATERIALIZED"));
            assert!(sql.contains("retained_bytes"));
            assert!(sql.contains("prior_bytes"));
            assert!(!sql.contains("OFFSET"));
            assert!(sql.contains("\"public\".\"w9pt_fs_state_"));
        }
        assert!(INODE_BY_QID_PATH_SQL.contains("\"qid_path\" = $2::numeric"));
        assert!(INODE_BY_QID_PATH_SQL.contains("\"filesystem_id\" = $1"));
    }

    #[test]
    fn scan_preflight_never_projects_large_variable_payloads() {
        assert!(INODE_SCAN_SQL.contains("octet_length(\"content_manifest_key\")"));
        assert!(INODE_SCAN_SQL.contains("octet_length(\"symlink_target\")"));
        assert!(!INODE_SCAN_SQL.contains("SELECT \"filesystem_id\", \"inode_id\""));

        assert!(XATTR_SCAN_SQL.contains("octet_length(\"value\")"));
        assert!(!XATTR_SCAN_SQL.contains("SELECT \"inode_id\", \"name\", \"value\""));

        assert!(XATTR_STAGING_SCAN_SQL.contains("octet_length(\"staged_bytes\")"));
        assert!(!XATTR_STAGING_SCAN_SQL.contains("SELECT \"staging_id\", \"staged_bytes\""));

        assert!(MUTATION_SCAN_SQL.contains("octet_length(\"result_bytes\")"));
        assert!(!MUTATION_SCAN_SQL.contains("SELECT \"mutation_id\", \"result_bytes\""));
    }

    #[tokio::test]
    async fn live_empty_authority_executes_every_point_and_scan_family()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("W9PT_POSTGRES_TEST_DSN") else {
            return Ok(());
        };
        let mut options = ConnectOptions::new(dsn);
        options.max_connections(2).sqlx_logging(false);
        let database = Database::connect(options).await?;
        crate::migration::migrate(&database, PostgresStateConfig::default()).await?;
        let limits = StateLimits::default();
        let config = PostgresStateConfig::default();
        let filesystem_id = FilesystemId::from_u128(u128::MAX - 101);
        let inode_id = InodeId::from_u128(2);
        let open_id = OpenId::from_u128(3);
        let lock_id = LockId::from_u128(4);
        let staging_id = XattrStagingId::from_u128(5);
        let scope = WriterScopeId::from_u128(6);
        let name = EntryName::new(b"missing".to_vec(), limits)?;
        let xattr_name = XattrName::new(b"user.missing".to_vec(), limits)?;
        let bounds = ScanBounds::new(16, 64 * 1024, limits)?;
        let point_queries = vec![
            ReadQuery::Filesystem,
            ReadQuery::Inode(inode_id),
            ReadQuery::DirectoryEntry {
                parent_inode_id: inode_id,
                name,
            },
            ReadQuery::Open(open_id),
            ReadQuery::OpenPin { inode_id, open_id },
            ReadQuery::Orphan(inode_id),
            ReadQuery::Lock { inode_id, lock_id },
            ReadQuery::Xattr {
                inode_id,
                name: xattr_name,
            },
            ReadQuery::XattrStaging(staging_id),
            ReadQuery::Mutation(MutationId::from_u128(7)),
            ReadQuery::WriterLease(scope),
        ];
        let point_batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            point_queries,
            limits,
        )?;
        let ReadOutcome::Snapshot(points) = read_request(&database, config, point_batch).await?
        else {
            panic!("expected empty point snapshot");
        };
        assert_eq!(points.revision().get(), 1);
        assert!(
            points
                .results()
                .iter()
                .all(|result| matches!(result, ReadResult::Point { record: None, .. }))
        );

        let semantic_batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            vec![
                ReadQuery::InodeByQidPath(w9pt_fs_state::QidPath::new(1)?),
                ReadQuery::OpenPinCount(inode_id),
                ReadQuery::DirectoryPage {
                    parent_inode_id: inode_id,
                    after: DirectoryCookie::START,
                    bounds,
                },
            ],
            limits,
        )?;
        let ReadOutcome::Snapshot(semantic) =
            read_request(&database, config, semantic_batch).await?
        else {
            panic!("expected empty semantic snapshot");
        };
        assert!(matches!(
            &semantic.results()[0],
            ReadResult::InodeByQidPath { inode: None, .. }
        ));
        assert_eq!(
            semantic.results()[1],
            ReadResult::OpenPinCount { inode_id, count: 0 }
        );
        assert!(matches!(
            &semantic.results()[2],
            ReadResult::DirectoryPage(page)
                if page.entries().is_empty() && page.resume().is_none()
        ));

        let scan_queries = vec![
            RecordScan::Inodes {
                after: None,
                bounds,
            },
            RecordScan::DirectoryEntries {
                parent_inode_id: inode_id,
                after: w9pt_fs_state::DirectoryCookie::START,
                bounds,
            },
            RecordScan::Opens {
                after: None,
                bounds,
            },
            RecordScan::OpenPins {
                after: None,
                bounds,
            },
            RecordScan::Orphans {
                after: None,
                bounds,
            },
            RecordScan::Locks {
                after: None,
                bounds,
            },
            RecordScan::Xattrs {
                after: None,
                bounds,
            },
            RecordScan::XattrStaging {
                after: None,
                bounds,
            },
            RecordScan::Mutations {
                after: None,
                bounds,
            },
            RecordScan::WriterLeases {
                after: None,
                bounds,
            },
        ]
        .into_iter()
        .map(ReadQuery::Scan)
        .collect::<Vec<_>>();
        let scan_batch = ReadBatch::new(
            filesystem_id,
            ReadConsistency::LatestLinearizable,
            scan_queries,
            limits,
        )?;
        let ReadOutcome::Snapshot(scans) = read_request(&database, config, scan_batch).await?
        else {
            panic!("expected empty scan snapshot");
        };
        assert_eq!(scans.revision().get(), 1);
        assert!(scans.results().iter().all(|result| matches!(
            result,
            ReadResult::Scan(page) if page.records().is_empty() && page.resume().is_none()
        )));
        Ok(())
    }
}
