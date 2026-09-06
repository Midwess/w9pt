//! Caller-side SeaORM connection helpers for live adapter tests.

#![allow(dead_code)]

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, DbErr,
    RuntimeErr, Statement, TransactionTrait, Value, error::SqlxError,
};
use w9pt_fs_state::FilesystemId;

pub(crate) const LIVE_DSN_ENV: &str = "W9PT_POSTGRES_TEST_DSN";

pub(crate) fn live_dsn() -> Option<String> {
    std::env::var(LIVE_DSN_ENV).ok()
}

pub(crate) async fn connect(
    dsn: impl Into<String>,
    max_connections: u32,
) -> Result<DatabaseConnection, DbErr> {
    let mut options = ConnectOptions::new(dsn.into());
    options
        .max_connections(max_connections)
        .min_connections(0)
        .sqlx_logging(false);
    Database::connect(options).await
}

pub(crate) fn statement(sql: impl Into<String>, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
}

pub(crate) fn native_code_and_constraint(error: &DbErr) -> Option<(String, Option<String>)> {
    let runtime = match error {
        DbErr::Conn(runtime) | DbErr::Exec(runtime) | DbErr::Query(runtime) => runtime,
        _ => return None,
    };
    let RuntimeErr::SqlxError(SqlxError::Database(database)) = runtime else {
        return None;
    };
    Some((
        database.code()?.into_owned(),
        database.constraint().map(str::to_owned),
    ))
}

pub(crate) async fn cleanup_filesystems(
    database: &DatabaseConnection,
    filesystem_ids: &[FilesystemId],
) -> Result<(), DbErr> {
    const DELETIONS: &[&str] = &[
        r#"DELETE FROM "public"."w9pt_fs_state_change_keys" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_change_commits" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_locks" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_open_pins" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_orphans" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_xattrs" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_xattr_staging" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_directory_entries" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_opens" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_filesystem_records" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_inodes" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_content_metadata" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_mutation_results" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_writer_lease_operations" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_writer_fences" WHERE "filesystem_id" = $1"#,
        r#"DELETE FROM "public"."w9pt_fs_state_authority_heads" WHERE "filesystem_id" = $1"#,
    ];
    let transaction = database.begin().await?;
    for filesystem_id in filesystem_ids {
        let filesystem = filesystem_id.as_bytes().to_vec();
        for sql in DELETIONS {
            transaction
                .execute(statement(*sql, vec![filesystem.clone().into()]))
                .await?;
        }
    }
    transaction.commit().await
}
