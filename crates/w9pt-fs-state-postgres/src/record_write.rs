//! Bounded table-specific locking and writes for semantic state records.

use core::fmt;

use sea_orm::{DbErr, ExecResult};
use w9pt_fs_state::{
    AdapterFailureKind, MutationRecord, RecordKey, StateRecord, StateRevision, StateStoreOperation,
};

use crate::{
    PostgresStateError,
    database::{PostgresTransaction, query, query_scalar, validate_rows_affected},
    numeric::encode_u64,
    row_codec::{
        ContentMetadataRow, DirectoryEntryRow, FilesystemRow, InodeRow, LockRow, MutationRow,
        OpenPinRow, OpenRow, OrphanRow, SqlStateRecord, XattrRow, XattrStagingRow,
    },
    sqlstate::SqlOperationPhase,
};

/// Whether a point lock or mutation found its exact semantic row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RowPresence {
    /// No row with the semantic key existed.
    Absent,
    /// Exactly one row with the semantic key existed or was inserted.
    Present,
}

/// Acquires a row lock for one existing semantic key and reports its presence.
pub(crate) async fn lock_existing_record(
    transaction: &mut PostgresTransaction,
    key: &RecordKey,
) -> Result<RowPresence, PostgresStateError> {
    let found = match key {
        RecordKey::Filesystem(filesystem_id) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_filesystem_records"
               WHERE "filesystem_id" = $1 FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::Inode(filesystem_id, inode_id) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_inodes"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2 FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::ContentMetadata(filesystem_id, file_id) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_content_metadata"
                   WHERE "filesystem_id" = $1 AND "content_file_id" = $2 FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(file_id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::DirectoryEntry(filesystem_id, parent_inode_id, name) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_directory_entries"
                   WHERE "filesystem_id" = $1 AND "parent_inode_id" = $2 AND "name" = $3
                   FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(parent_inode_id.as_bytes().to_vec())
            .bind(name.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::Open(filesystem_id, open_id) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_opens"
               WHERE "filesystem_id" = $1 AND "open_id" = $2 FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(open_id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::OpenPin(filesystem_id, inode_id, open_id) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_open_pins"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2 AND "open_id" = $3
               FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .bind(open_id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::Orphan(filesystem_id, inode_id) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_orphans"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2 FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::Lock(filesystem_id, inode_id, lock_id) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_locks"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2 AND "lock_id" = $3
               FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .bind(lock_id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::Xattr(filesystem_id, inode_id, name) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_xattrs"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2 AND "name" = $3 FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .bind(name.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::XattrStaging(filesystem_id, staging_id) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_xattr_staging"
               WHERE "filesystem_id" = $1 AND "staging_id" = $2 FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(staging_id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::Mutation(filesystem_id, mutation_id) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_mutation_results"
               WHERE "filesystem_id" = $1 AND "mutation_id" = $2 FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(mutation_id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
        RecordKey::WriterLease(filesystem_id, writer_scope_id) => {
            query_scalar::<i32>(
                r#"SELECT 1 FROM "public"."w9pt_fs_state_writer_fences"
               WHERE "filesystem_id" = $1 AND "writer_scope_id" = $2
               FOR UPDATE"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(writer_scope_id.as_bytes().to_vec())
            .fetch_optional(transaction)
            .await
        }
    }
    .map_err(execute_error)?;
    Ok(if found.is_some() {
        RowPresence::Present
    } else {
        RowPresence::Absent
    })
}

/// Inserts one caller-changeable public record without upsert behavior.
pub(crate) async fn insert_record(
    transaction: &mut PostgresTransaction,
    key: &RecordKey,
    record: &StateRecord,
    revision: StateRevision,
) -> Result<RowPresence, PostgresStateError> {
    let row = prepare_public_record(key, record, revision).map_err(prepare_error)?;
    let result = insert_encoded(transaction, row).await?;
    affected_presence(result)
}

/// Replaces one existing caller-changeable public record by exact key.
pub(crate) async fn replace_record(
    transaction: &mut PostgresTransaction,
    key: &RecordKey,
    record: &StateRecord,
    revision: StateRevision,
) -> Result<RowPresence, PostgresStateError> {
    let row = prepare_public_record(key, record, revision).map_err(prepare_error)?;
    let result = replace_encoded(transaction, row).await?;
    affected_presence(result)
}

/// Deletes one caller-changeable public record by exact key.
pub(crate) async fn delete_record(
    transaction: &mut PostgresTransaction,
    key: &RecordKey,
) -> Result<RowPresence, PostgresStateError> {
    if matches!(key, RecordKey::Mutation(..) | RecordKey::WriterLease(..)) {
        return Err(prepare_error(
            RecordWritePrepareError::ProtectedRecordFamily,
        ));
    }
    let result = delete_by_key(transaction, key).await?;
    affected_presence(result)
}

/// Inserts the immutable terminal mutation ledger row at the allocated revision.
pub(crate) async fn insert_mutation_record(
    transaction: &mut PostgresTransaction,
    key: &RecordKey,
    record: &MutationRecord,
    revision: StateRevision,
) -> Result<RowPresence, PostgresStateError> {
    let state_record = StateRecord::Mutation(record.clone());
    let encoded = SqlStateRecord::encode(key, &state_record)
        .map_err(|error| prepare_error(RecordWritePrepareError::Codec(error.to_string())))?;
    let SqlStateRecord::Mutation(mut row) = encoded else {
        return Err(prepare_error(RecordWritePrepareError::MutationKeyMismatch));
    };
    let allocated = encode_u64(revision.get());
    row.committed_revision.clone_from(&allocated);
    row.record_revision = allocated;
    let result = insert_mutation(transaction, row).await?;
    affected_presence(result)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RecordWritePrepareError {
    ProtectedRecordFamily,
    MutationKeyMismatch,
    Codec(String),
}

impl fmt::Display for RecordWritePrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProtectedRecordFamily => formatter.write_str(
                "mutation and writer-lease records require their dedicated persistence protocol",
            ),
            Self::MutationKeyMismatch => {
                formatter.write_str("mutation record did not encode as a mutation row")
            }
            Self::Codec(detail) => write!(formatter, "record codec rejected write: {detail}"),
        }
    }
}

fn prepare_public_record(
    key: &RecordKey,
    record: &StateRecord,
    revision: StateRevision,
) -> Result<SqlStateRecord, RecordWritePrepareError> {
    let mut row = SqlStateRecord::encode(key, record)
        .map_err(|error| RecordWritePrepareError::Codec(error.to_string()))?;
    let allocated = encode_u64(revision.get());
    match &mut row {
        SqlStateRecord::Filesystem(row) => {
            row.state_revision.clone_from(&allocated);
            row.record_revision.clone_from(&allocated);
        }
        SqlStateRecord::Inode(row) => row.record_revision.clone_from(&allocated),
        SqlStateRecord::ContentMetadata(row) => row.record_revision.clone_from(&allocated),
        SqlStateRecord::DirectoryEntry(row) => row.record_revision.clone_from(&allocated),
        SqlStateRecord::Open(row) => row.record_revision.clone_from(&allocated),
        SqlStateRecord::OpenPin(row) => row.record_revision.clone_from(&allocated),
        SqlStateRecord::Orphan(row) => row.record_revision.clone_from(&allocated),
        SqlStateRecord::Lock(row) => row.record_revision.clone_from(&allocated),
        SqlStateRecord::Xattr(row) => row.record_revision.clone_from(&allocated),
        SqlStateRecord::XattrStaging(row) => row.record_revision.clone_from(&allocated),
        SqlStateRecord::Mutation(_) | SqlStateRecord::WriterLease(_) => {
            return Err(RecordWritePrepareError::ProtectedRecordFamily);
        }
    }
    Ok(row)
}

async fn insert_encoded(
    transaction: &mut PostgresTransaction,
    row: SqlStateRecord,
) -> Result<ExecResult, PostgresStateError> {
    match row {
        SqlStateRecord::Filesystem(row) => insert_filesystem(transaction, row).await,
        SqlStateRecord::Inode(row) => insert_inode(transaction, *row).await,
        SqlStateRecord::ContentMetadata(row) => insert_content_metadata(transaction, row).await,
        SqlStateRecord::DirectoryEntry(row) => insert_directory_entry(transaction, row).await,
        SqlStateRecord::Open(row) => insert_open(transaction, row).await,
        SqlStateRecord::OpenPin(row) => insert_open_pin(transaction, row).await,
        SqlStateRecord::Orphan(row) => insert_orphan(transaction, row).await,
        SqlStateRecord::Lock(row) => insert_lock(transaction, row).await,
        SqlStateRecord::Xattr(row) => insert_xattr(transaction, row).await,
        SqlStateRecord::XattrStaging(row) => insert_xattr_staging(transaction, row).await,
        SqlStateRecord::Mutation(_) | SqlStateRecord::WriterLease(_) => Err(prepare_error(
            RecordWritePrepareError::ProtectedRecordFamily,
        )),
    }
}

async fn replace_encoded(
    transaction: &mut PostgresTransaction,
    row: SqlStateRecord,
) -> Result<ExecResult, PostgresStateError> {
    match row {
        SqlStateRecord::Filesystem(row) => replace_filesystem(transaction, row).await,
        SqlStateRecord::Inode(row) => replace_inode(transaction, *row).await,
        SqlStateRecord::ContentMetadata(row) => replace_content_metadata(transaction, row).await,
        SqlStateRecord::DirectoryEntry(row) => replace_directory_entry(transaction, row).await,
        SqlStateRecord::Open(row) => replace_open(transaction, row).await,
        SqlStateRecord::OpenPin(row) => replace_open_pin(transaction, row).await,
        SqlStateRecord::Orphan(row) => replace_orphan(transaction, row).await,
        SqlStateRecord::Lock(row) => replace_lock(transaction, row).await,
        SqlStateRecord::Xattr(row) => replace_xattr(transaction, row).await,
        SqlStateRecord::XattrStaging(row) => replace_xattr_staging(transaction, row).await,
        SqlStateRecord::Mutation(_) | SqlStateRecord::WriterLease(_) => Err(prepare_error(
            RecordWritePrepareError::ProtectedRecordFamily,
        )),
    }
}

async fn insert_filesystem(
    transaction: &mut PostgresTransaction,
    row: FilesystemRow,
) -> Result<ExecResult, PostgresStateError> {
    query(
        r#"INSERT INTO "public"."w9pt_fs_state_filesystem_records"
           ("filesystem_id", "state_revision", "record_revision", "root_inode_id",
            "next_qid_path", "next_directory_cookie", "policy_generation")
           VALUES ($1, $2::numeric, $3::numeric, $4, $5::numeric, $6::numeric,
                   $7::numeric)"#,
    )
    .bind(row.filesystem_id)
    .bind(row.state_revision)
    .bind(row.record_revision)
    .bind(row.root_inode_id)
    .bind(row.next_qid_path)
    .bind(row.next_directory_cookie)
    .bind(row.policy_generation)
    .execute(transaction)
    .await
    .map_err(execute_error)
}

async fn replace_filesystem(
    transaction: &mut PostgresTransaction,
    row: FilesystemRow,
) -> Result<ExecResult, PostgresStateError> {
    query(
        r#"UPDATE "public"."w9pt_fs_state_filesystem_records"
           SET "state_revision" = $2::numeric, "record_revision" = $3::numeric,
               "root_inode_id" = $4, "next_qid_path" = $5::numeric,
               "next_directory_cookie" = $6::numeric,
               "policy_generation" = $7::numeric
           WHERE "filesystem_id" = $1"#,
    )
    .bind(row.filesystem_id)
    .bind(row.state_revision)
    .bind(row.record_revision)
    .bind(row.root_inode_id)
    .bind(row.next_qid_path)
    .bind(row.next_directory_cookie)
    .bind(row.policy_generation)
    .execute(transaction)
    .await
    .map_err(execute_error)
}

async fn insert_inode(
    transaction: &mut PostgresTransaction,
    row: InodeRow,
) -> Result<ExecResult, PostgresStateError> {
    query(
        r#"INSERT INTO "public"."w9pt_fs_state_inodes"
           ("filesystem_id", "inode_id", "qid_path", "record_revision", "mode", "owner", "group_id",
            "accessed_seconds", "accessed_nanoseconds", "modified_seconds",
            "modified_nanoseconds", "changed_seconds", "changed_nanoseconds",
            "created_seconds", "created_nanoseconds", "logical_size", "link_count",
            "inode_generation", "kind", "content_file_id", "content_context_id", "data_generation",
            "content_generation", "content_logical_size", "content_manifest_key",
            "content_manifest_digest", "content_storage_method", "directory_generation",
            "symlink_target", "device_major", "device_minor", "directory_parent_inode_id")
           VALUES ($1, $2, $3::numeric, $4::numeric, $5, $6, $7, $8, $9, $10,
                   $11, $12, $13, $14, $15, $16::numeric, $17::numeric,
                   $18::numeric, $19, $20, $21, $22::numeric, $23::numeric,
                   $24::numeric, $25, $26, $27, $28::numeric, $29, $30, $31, $32)"#,
    )
    .bind(row.filesystem_id)
    .bind(row.inode_id)
    .bind(row.qid_path)
    .bind(row.record_revision)
    .bind(row.mode)
    .bind(row.owner)
    .bind(row.group_id)
    .bind(row.accessed_seconds)
    .bind(row.accessed_nanoseconds)
    .bind(row.modified_seconds)
    .bind(row.modified_nanoseconds)
    .bind(row.changed_seconds)
    .bind(row.changed_nanoseconds)
    .bind(row.created_seconds)
    .bind(row.created_nanoseconds)
    .bind(row.logical_size)
    .bind(row.link_count)
    .bind(row.inode_generation)
    .bind(row.kind)
    .bind(row.content_file_id)
    .bind(row.content_context_id)
    .bind(row.data_generation)
    .bind(row.content_generation)
    .bind(row.content_logical_size)
    .bind(row.content_manifest_key)
    .bind(row.content_manifest_digest)
    .bind(row.content_storage_method)
    .bind(row.directory_generation)
    .bind(row.symlink_target)
    .bind(row.device_major)
    .bind(row.device_minor)
    .bind(row.directory_parent_inode_id)
    .execute(transaction)
    .await
    .map_err(execute_error)
}

async fn replace_inode(
    transaction: &mut PostgresTransaction,
    row: InodeRow,
) -> Result<ExecResult, PostgresStateError> {
    query(
        r#"UPDATE "public"."w9pt_fs_state_inodes" SET
           "qid_path" = $3::numeric, "record_revision" = $4::numeric,
           "mode" = $5, "owner" = $6, "group_id" = $7,
           "accessed_seconds" = $8, "accessed_nanoseconds" = $9,
           "modified_seconds" = $10, "modified_nanoseconds" = $11,
           "changed_seconds" = $12, "changed_nanoseconds" = $13,
           "created_seconds" = $14, "created_nanoseconds" = $15,
           "logical_size" = $16::numeric, "link_count" = $17::numeric,
           "inode_generation" = $18::numeric, "kind" = $19, "content_file_id" = $20,
           "content_context_id" = $21, "data_generation" = $22::numeric,
           "content_generation" = $23::numeric, "content_logical_size" = $24::numeric,
           "content_manifest_key" = $25, "content_manifest_digest" = $26,
           "content_storage_method" = $27, "directory_generation" = $28::numeric,
           "symlink_target" = $29, "device_major" = $30, "device_minor" = $31,
           "directory_parent_inode_id" = $32
           WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
    )
    .bind(row.filesystem_id)
    .bind(row.inode_id)
    .bind(row.qid_path)
    .bind(row.record_revision)
    .bind(row.mode)
    .bind(row.owner)
    .bind(row.group_id)
    .bind(row.accessed_seconds)
    .bind(row.accessed_nanoseconds)
    .bind(row.modified_seconds)
    .bind(row.modified_nanoseconds)
    .bind(row.changed_seconds)
    .bind(row.changed_nanoseconds)
    .bind(row.created_seconds)
    .bind(row.created_nanoseconds)
    .bind(row.logical_size)
    .bind(row.link_count)
    .bind(row.inode_generation)
    .bind(row.kind)
    .bind(row.content_file_id)
    .bind(row.content_context_id)
    .bind(row.data_generation)
    .bind(row.content_generation)
    .bind(row.content_logical_size)
    .bind(row.content_manifest_key)
    .bind(row.content_manifest_digest)
    .bind(row.content_storage_method)
    .bind(row.directory_generation)
    .bind(row.symlink_target)
    .bind(row.device_major)
    .bind(row.device_minor)
    .bind(row.directory_parent_inode_id)
    .execute(transaction)
    .await
    .map_err(execute_error)
}

macro_rules! simple_insert_replace {
    (
        $insert_fn:ident, $replace_fn:ident, $row:ty, $table:literal,
        [$($column:literal => $field:ident $(as $cast:literal)?),+ $(,)?],
        keys = $key_count:literal
    ) => {
        async fn $insert_fn(
            transaction: &mut PostgresTransaction,
            row: $row,
        ) -> Result<ExecResult, PostgresStateError> {
            let sql = insert_sql($table, &[$($column),+], &[$(cast_name!($($cast)?)),+]);
            let mut query = query(&sql);
            $(query = query.bind(row.$field);)+
            query.execute(transaction).await.map_err(execute_error)
        }

        async fn $replace_fn(
            transaction: &mut PostgresTransaction,
            row: $row,
        ) -> Result<ExecResult, PostgresStateError> {
            let sql = replace_sql(
                $table,
                &[$($column),+],
                &[$(cast_name!($($cast)?)),+],
                $key_count,
            );
            let mut query = query(&sql);
            $(query = query.bind(row.$field);)+
            query.execute(transaction).await.map_err(execute_error)
        }
    };
}

// Macro calls use `as "numeric"` only for decimal-text fields. The helper
// overload below represents all primitive fields without a cast.
macro_rules! cast_name {
    () => {
        None
    };
    ($cast:literal) => {
        Some($cast)
    };
}

simple_insert_replace!(
    insert_content_metadata,
    replace_content_metadata,
    ContentMetadataRow,
    "content_metadata",
    [
        "filesystem_id" => filesystem_id,
        "content_file_id" => content_file_id,
        "owner_inode_id" => owner_inode_id,
        "context_id" => context_id,
        "policy_format" => policy_format,
        "policy_bytes" => policy_bytes,
        "key_commitment" => key_commitment,
        "wrapped_key_bytes" => wrapped_key_bytes,
        "record_revision" => record_revision as "numeric",
    ],
    keys = 2
);

simple_insert_replace!(
    insert_directory_entry,
    replace_directory_entry,
    DirectoryEntryRow,
    "directory_entries",
    [
        "filesystem_id" => filesystem_id,
        "parent_inode_id" => parent_inode_id,
        "name" => name,
        "cookie" => cookie as "numeric",
        "child_inode_id" => child_inode_id,
        "record_revision" => record_revision as "numeric",
    ],
    keys = 3
);

simple_insert_replace!(
    insert_open,
    replace_open,
    OpenRow,
    "opens",
    [
        "filesystem_id" => filesystem_id,
        "open_id" => open_id,
        "inode_id" => inode_id,
        "client_incarnation_id" => client_incarnation_id,
        "access" => access,
        "append" => append,
        "retained_inode_generation" => retained_inode_generation as "numeric",
        "record_revision" => record_revision as "numeric",
    ],
    keys = 2
);

simple_insert_replace!(
    insert_open_pin,
    replace_open_pin,
    OpenPinRow,
    "open_pins",
    [
        "filesystem_id" => filesystem_id,
        "inode_id" => inode_id,
        "open_id" => open_id,
        "record_revision" => record_revision as "numeric",
    ],
    keys = 3
);

simple_insert_replace!(
    insert_orphan,
    replace_orphan,
    OrphanRow,
    "orphans",
    [
        "filesystem_id" => filesystem_id,
        "inode_id" => inode_id,
        "open_pin_count" => open_pin_count as "numeric",
        "orphaned_revision" => orphaned_revision as "numeric",
        "record_revision" => record_revision as "numeric",
    ],
    keys = 2
);

simple_insert_replace!(
    insert_lock,
    replace_lock,
    LockRow,
    "locks",
    [
        "filesystem_id" => filesystem_id,
        "inode_id" => inode_id,
        "lock_id" => lock_id,
        "range_start" => range_start as "numeric",
        "range_end" => range_end as "numeric",
        "kind" => kind,
        "owner_client_incarnation_id" => owner_client_incarnation_id,
        "owner_open_id" => owner_open_id,
        "lock_generation" => lock_generation as "numeric",
        "record_revision" => record_revision as "numeric",
    ],
    keys = 3
);

simple_insert_replace!(
    insert_xattr,
    replace_xattr,
    XattrRow,
    "xattrs",
    [
        "filesystem_id" => filesystem_id,
        "inode_id" => inode_id,
        "name" => name,
        "value" => value,
        "record_revision" => record_revision as "numeric",
    ],
    keys = 3
);

simple_insert_replace!(
    insert_xattr_staging,
    replace_xattr_staging,
    XattrStagingRow,
    "xattr_staging",
    [
        "filesystem_id" => filesystem_id,
        "staging_id" => staging_id,
        "inode_id" => inode_id,
        "name" => name,
        "expected_size" => expected_size as "numeric",
        "staged_bytes" => staged_bytes,
        "record_revision" => record_revision as "numeric",
    ],
    keys = 2
);

async fn insert_mutation(
    transaction: &mut PostgresTransaction,
    row: MutationRow,
) -> Result<ExecResult, PostgresStateError> {
    query(
        r#"INSERT INTO "public"."w9pt_fs_state_mutation_results"
           ("filesystem_id", "mutation_id", "request_fingerprint", "client_incarnation_id",
            "writer_scope_id", "writer_incarnation_id", "fencing_token", "result_kind",
            "result_format", "result_bytes", "committed_revision", "retention_horizon",
            "record_revision")
           VALUES ($1, $2, $3, $4, $5, $6, $7::numeric, $8, $9, $10,
                   $11::numeric, $12::numeric, $13::numeric)"#,
    )
    .bind(row.filesystem_id)
    .bind(row.mutation_id)
    .bind(row.request_fingerprint)
    .bind(row.client_incarnation_id)
    .bind(row.writer_scope_id)
    .bind(row.writer_incarnation_id)
    .bind(row.fencing_token)
    .bind(row.result_kind)
    .bind(row.result_format)
    .bind(row.result_bytes)
    .bind(row.committed_revision)
    .bind(row.retention_horizon)
    .bind(row.record_revision)
    .execute(transaction)
    .await
    .map_err(execute_error)
}

async fn delete_by_key(
    transaction: &mut PostgresTransaction,
    key: &RecordKey,
) -> Result<ExecResult, PostgresStateError> {
    let result = match key {
        RecordKey::Filesystem(filesystem_id) => {
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_filesystem_records"
               WHERE "filesystem_id" = $1"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .execute(transaction)
            .await
        }
        RecordKey::Inode(filesystem_id, inode_id) => {
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_inodes"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .execute(transaction)
            .await
        }
        RecordKey::ContentMetadata(filesystem_id, file_id) => {
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_content_metadata"
                   WHERE "filesystem_id" = $1 AND "content_file_id" = $2"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(file_id.as_bytes().to_vec())
            .execute(transaction)
            .await
        }
        RecordKey::DirectoryEntry(filesystem_id, parent_inode_id, name) => {
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_directory_entries"
               WHERE "filesystem_id" = $1 AND "parent_inode_id" = $2 AND "name" = $3"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(parent_inode_id.as_bytes().to_vec())
            .bind(name.as_bytes().to_vec())
            .execute(transaction)
            .await
        }
        RecordKey::Open(filesystem_id, open_id) => {
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_opens"
               WHERE "filesystem_id" = $1 AND "open_id" = $2"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(open_id.as_bytes().to_vec())
            .execute(transaction)
            .await
        }
        RecordKey::OpenPin(filesystem_id, inode_id, open_id) => {
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_open_pins"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2 AND "open_id" = $3"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .bind(open_id.as_bytes().to_vec())
            .execute(transaction)
            .await
        }
        RecordKey::Orphan(filesystem_id, inode_id) => {
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_orphans"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .execute(transaction)
            .await
        }
        RecordKey::Lock(filesystem_id, inode_id, lock_id) => {
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_locks"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2 AND "lock_id" = $3"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .bind(lock_id.as_bytes().to_vec())
            .execute(transaction)
            .await
        }
        RecordKey::Xattr(filesystem_id, inode_id, name) => {
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_xattrs"
               WHERE "filesystem_id" = $1 AND "inode_id" = $2 AND "name" = $3"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(inode_id.as_bytes().to_vec())
            .bind(name.as_bytes().to_vec())
            .execute(transaction)
            .await
        }
        RecordKey::XattrStaging(filesystem_id, staging_id) => {
            query(
                r#"DELETE FROM "public"."w9pt_fs_state_xattr_staging"
               WHERE "filesystem_id" = $1 AND "staging_id" = $2"#,
            )
            .bind(filesystem_id.as_bytes().to_vec())
            .bind(staging_id.as_bytes().to_vec())
            .execute(transaction)
            .await
        }
        RecordKey::Mutation(..) | RecordKey::WriterLease(..) => {
            return Err(prepare_error(
                RecordWritePrepareError::ProtectedRecordFamily,
            ));
        }
    };
    result.map_err(execute_error)
}

fn insert_sql(table: &str, columns: &[&str], casts: &[Option<&str>]) -> String {
    let quoted = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let values = casts
        .iter()
        .enumerate()
        .map(|(index, cast)| match cast {
            Some(cast) => format!("${}::{cast}", index + 1),
            None => format!("${}", index + 1),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("INSERT INTO \"public\".\"w9pt_fs_state_{table}\" ({quoted}) VALUES ({values})")
}

fn replace_sql(table: &str, columns: &[&str], casts: &[Option<&str>], key_count: usize) -> String {
    let assignments = columns
        .iter()
        .zip(casts)
        .enumerate()
        .skip(key_count)
        .map(|(index, (column, cast))| match cast {
            Some(cast) => format!("\"{column}\" = ${}::{cast}", index + 1),
            None => format!("\"{column}\" = ${}", index + 1),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let predicates = columns
        .iter()
        .take(key_count)
        .enumerate()
        .map(|(index, column)| format!("\"{column}\" = ${}", index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    format!("UPDATE \"public\".\"w9pt_fs_state_{table}\" SET {assignments} WHERE {predicates}")
}

fn affected_presence(result: ExecResult) -> Result<RowPresence, PostgresStateError> {
    match validate_rows_affected(&result, 0..=1) {
        Ok(0) => Ok(RowPresence::Absent),
        Ok(1) => Ok(RowPresence::Present),
        Ok(_) => unreachable!("validated range permits only zero or one"),
        Err(rows) => Err(PostgresStateError::new(
            StateStoreOperation::Commit,
            AdapterFailureKind::Corruption,
            format!("point record write affected {rows} rows"),
        )),
    }
}

fn execute_error(error: DbErr) -> PostgresStateError {
    PostgresStateError::from_database(
        StateStoreOperation::Commit,
        SqlOperationPhase::ExecuteStatement,
        error,
    )
}

fn prepare_error(error: RecordWritePrepareError) -> PostgresStateError {
    PostgresStateError::new(
        StateStoreOperation::Commit,
        AdapterFailureKind::Internal,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt_fs_state::{
        DirectoryCookie, FencingToken, FilesystemId, FilesystemRecord, InodeId, LeaseDeadline,
        LeaseId, RecordRevision, StateRevision, WriterIncarnationId, WriterLeaseRecord,
        WriterScopeId,
    };

    #[test]
    fn allocated_revision_replaces_both_filesystem_revision_fields() {
        let filesystem_id = FilesystemId::from_u128(1);
        let record = StateRecord::Filesystem(
            FilesystemRecord::new(
                filesystem_id,
                StateRevision::new(2).unwrap(),
                RecordRevision::new(2).unwrap(),
                InodeId::from_u128(3),
                w9pt_fs_state::QidPath::new(4).unwrap(),
                DirectoryCookie::new(4),
                5,
            )
            .unwrap(),
        );
        let encoded = prepare_public_record(
            &RecordKey::Filesystem(filesystem_id),
            &record,
            StateRevision::new(9).unwrap(),
        )
        .unwrap();
        let SqlStateRecord::Filesystem(row) = encoded else {
            panic!("expected filesystem row");
        };
        assert_eq!(row.state_revision, "9");
        assert_eq!(row.record_revision, "9");
    }

    #[test]
    fn direct_writer_lease_writes_are_rejected() {
        let filesystem_id = FilesystemId::from_u128(1);
        let scope = WriterScopeId::from_u128(2);
        let lease = WriterLeaseRecord::new(
            filesystem_id,
            scope,
            WriterIncarnationId::from_u128(3),
            LeaseId::from_u128(4),
            LeaseDeadline::new(5),
            FencingToken::new(6).unwrap(),
            RecordRevision::new(7).unwrap(),
        );
        assert_eq!(
            prepare_public_record(
                &RecordKey::WriterLease(filesystem_id, scope),
                &StateRecord::WriterLease(lease),
                StateRevision::new(8).unwrap(),
            ),
            Err(RecordWritePrepareError::ProtectedRecordFamily)
        );
    }

    #[test]
    fn generated_statement_shapes_are_point_bounded_and_distinguish_operations() {
        let insert = insert_sql(
            "orphans",
            &["filesystem_id", "inode_id", "record_revision"],
            &[None, None, Some("numeric")],
        );
        let replace = replace_sql(
            "orphans",
            &["filesystem_id", "inode_id", "record_revision"],
            &[None, None, Some("numeric")],
            2,
        );
        for sql in [&insert, &replace] {
            assert!(sql.contains("\"public\".\"w9pt_fs_state_orphans\""));
            assert!(!sql.contains(&["ON", "CONFLICT"].join(" ")));
            assert!(!sql.contains(&["CAS", "CADE"].concat()));
        }
        assert!(insert.starts_with("INSERT INTO"));
        assert!(insert.contains("$3::numeric"));
        assert!(replace.starts_with("UPDATE"));
        assert!(replace.contains("WHERE \"filesystem_id\" = $1 AND \"inode_id\" = $2"));
    }

    #[test]
    fn source_contains_only_fully_qualified_bounded_record_statements() {
        let source = include_str!("record_write.rs");
        assert!(!source.contains(&["ON", "CONFLICT"].join(" ")));
        assert!(!source.contains(&["DELETE", "CASCADE"].join(" ")));
        assert!(!source.contains(&["", "OFFSET", ""].join(" ")));
        for table in [
            "filesystem_records",
            "inodes",
            "directory_entries",
            "opens",
            "open_pins",
            "orphans",
            "locks",
            "xattrs",
            "xattr_staging",
            "mutation_results",
            "writer_fences",
        ] {
            assert!(
                source.contains(&format!("\"public\".\"w9pt_fs_state_{table}\"")),
                "missing fully qualified SQL for {table}"
            );
        }
        assert!(source.matches("FOR UPDATE").count() >= 11);
    }
}
