//! Environment-driven migration integration tests.

mod common;

use std::time::{Duration, Instant};

use sea_orm::{ConnectionTrait, TransactionTrait};
use w9pt_fs_state::{AuthorityClass, WriterTopology};
use w9pt_fs_state_postgres::{
    MigrationError, PostgresOpenError, PostgresStateConfig, PostgresStateStore,
};

use common::{connect, live_dsn, native_code_and_constraint, statement};

const MIGRATION_LOCK_KEY: i64 = 0x7739_7074_6673_7631;
const DIRECT_CUT_V1_COLUMN_COUNT: i64 = 130;
const DIRECT_CUT_V1_CONSTRAINT_COUNT: i64 = 172;
const DIRECT_CUT_V1_INDEX_COUNT: i64 = 23;
const DIRECT_CUT_V1_COLUMN_CATALOG_MD5: &str = "7f3d74bc3cd8d0767905fc84e79e048f";
const DIRECT_CUT_V1_CONSTRAINT_CATALOG_MD5: &str = "a494e9a593ea21357ccb274387a60f84";
const DIRECT_CUT_V1_INDEX_CATALOG_MD5: &str = "2d7113a6c7bf648a4c7d8ac0f69151e9";
const COLUMN_CATALOG_COUNT_SQL: &str = r#"
SELECT pg_catalog.count(*)::bigint AS catalog_count
FROM information_schema.columns
WHERE table_schema = 'public'
  AND pg_catalog.left(table_name, 14) = 'w9pt_fs_state_'
"#;
const CONSTRAINT_CATALOG_COUNT_SQL: &str = r#"
SELECT pg_catalog.count(*)::bigint AS catalog_count
FROM pg_catalog.pg_constraint AS constraint_row
INNER JOIN pg_catalog.pg_class AS class
    ON class.oid = constraint_row.conrelid
INNER JOIN pg_catalog.pg_namespace AS namespace
    ON namespace.oid = class.relnamespace
WHERE namespace.nspname = 'public'
  AND pg_catalog.left(class.relname, 14) = 'w9pt_fs_state_'
  AND constraint_row.contype <> 'n'
"#;
const INDEX_CATALOG_COUNT_SQL: &str = r#"
SELECT pg_catalog.count(*)::bigint AS catalog_count
FROM pg_catalog.pg_indexes
WHERE schemaname = 'public'
  AND pg_catalog.left(tablename, 14) = 'w9pt_fs_state_'
"#;
const COLUMN_CATALOG_MD5_SQL: &str = r#"
WITH rows AS (
    SELECT substring(table_name FROM 15) AS table_name,
           ordinal_position, column_name, data_type, udt_name, is_nullable,
           coalesce(numeric_precision::text, '') AS precision_text,
           coalesce(numeric_scale::text, '') AS scale_text,
           coalesce(column_default, '') AS default_text
    FROM information_schema.columns
    WHERE table_schema = 'public'
      AND pg_catalog.left(table_name, 14) = 'w9pt_fs_state_'
)
SELECT md5(string_agg(
    concat_ws(E'\x1f', table_name, ordinal_position, column_name, data_type,
              udt_name, is_nullable, precision_text, scale_text, default_text),
    E'\x1e' ORDER BY table_name, ordinal_position
)) AS catalog_md5
FROM rows
"#;
const CONSTRAINT_CATALOG_MD5_SQL: &str = r#"
WITH rows AS (
    SELECT substring(class.relname FROM 15) AS table_name,
           substring(constraint_row.conname FROM 15) AS constraint_name,
           constraint_row.contype, constraint_row.condeferrable,
           constraint_row.condeferred,
           replace(pg_get_constraintdef(constraint_row.oid, true),
                   'w9pt_fs_state_', '') AS definition
    FROM pg_catalog.pg_constraint AS constraint_row
    INNER JOIN pg_catalog.pg_class AS class
        ON class.oid = constraint_row.conrelid
    INNER JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = class.relnamespace
    WHERE namespace.nspname = 'public'
      AND pg_catalog.left(class.relname, 14) = 'w9pt_fs_state_'
      AND constraint_row.contype <> 'n'
)
SELECT md5(string_agg(
    concat_ws(E'\x1f', table_name, constraint_name, contype,
              condeferrable, condeferred, definition),
    E'\x1e' ORDER BY table_name, constraint_name
)) AS catalog_md5
FROM rows
"#;
const INDEX_CATALOG_MD5_SQL: &str = r#"
WITH rows AS (
    SELECT substring(tablename FROM 15) AS table_name,
           substring(indexname FROM 15) AS index_name,
           replace(replace(indexdef, 'public.w9pt_fs_state_', ''),
                   'w9pt_fs_state_', '') AS definition
    FROM pg_catalog.pg_indexes
    WHERE schemaname = 'public'
      AND pg_catalog.left(tablename, 14) = 'w9pt_fs_state_'
)
SELECT md5(string_agg(
    concat_ws(E'\x1f', table_name, index_name, definition),
    E'\x1e' ORDER BY table_name, index_name
)) AS catalog_md5
FROM rows
"#;
const RESET_PUBLIC_STATE_TABLES: &str = r#"
DO $migration_reset$
DECLARE
    relation_name text;
BEGIN
    FOR relation_name IN
        SELECT class.relname
        FROM pg_catalog.pg_class AS class
        INNER JOIN pg_catalog.pg_namespace AS namespace
            ON namespace.oid = class.relnamespace
        WHERE namespace.nspname = 'public'
          AND class.relkind IN ('r', 'p')
          AND pg_catalog.left(class.relname, 14) = 'w9pt_fs_state_'
    LOOP
        EXECUTE format('DROP TABLE IF EXISTS public.%I CASCADE', relation_name);
    END LOOP;
END
$migration_reset$;
"#;

fn with_connection_options(dsn: &str, encoded_options: &str) -> String {
    let separator = if dsn.contains('?') { '&' } else { '?' };
    format!("{dsn}{separator}options={encoded_options}")
}

async fn catalog_md5(
    database: &sea_orm::DatabaseConnection,
    sql: &str,
) -> Result<String, sea_orm::DbErr> {
    database
        .query_one(statement(sql, Vec::new()))
        .await?
        .ok_or_else(|| sea_orm::DbErr::RecordNotFound("catalog hash row is absent".to_owned()))?
        .try_get("", "catalog_md5")
}

async fn catalog_count(
    database: &sea_orm::DatabaseConnection,
    sql: &str,
) -> Result<i64, sea_orm::DbErr> {
    database
        .query_one(statement(sql, Vec::new()))
        .await?
        .ok_or_else(|| sea_orm::DbErr::RecordNotFound("catalog count row is absent".to_owned()))?
        .try_get("", "catalog_count")
}

#[tokio::test]
async fn explicit_migration_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let Some(dsn) = live_dsn() else {
        return Ok(());
    };
    let pool = connect(dsn.clone(), 4).await?;
    pool.execute_unprepared(RESET_PUBLIC_STATE_TABLES).await?;

    pool.execute_unprepared(
        r#"CREATE TABLE "public"."w9pt_fs_state_authority_heads" (invalid integer);"#,
    )
    .await?;
    let rollback_error = PostgresStateStore::migrate(&pool)
        .await
        .expect_err("incompatible preexisting relation must abort migration");
    assert!(matches!(
        rollback_error,
        PostgresOpenError::Migration(MigrationError::PublicSchemaCollision(relation))
            if relation == "w9pt_fs_state_authority_heads"
    ));
    let ledger_exists = pool
        .query_one(statement(
            r#"SELECT pg_catalog.to_regclass(
                   'public.w9pt_fs_state_schema_migrations'
               ) IS NOT NULL AS present"#,
            Vec::new(),
        ))
        .await?
        .expect("catalog query returns one row")
        .try_get::<bool>("", "present")?;
    assert!(
        !ledger_exists,
        "failed migration leaked its bootstrap ledger"
    );
    pool.execute_unprepared(RESET_PUBLIC_STATE_TABLES).await?;

    let repeatable_read_dsn = with_connection_options(
        &dsn,
        "-c%20default_transaction_isolation%3Drepeatable%5C%20read",
    );
    let first_runner = connect(repeatable_read_dsn.clone(), 2).await?;
    let contender = connect(repeatable_read_dsn, 2).await?;
    let (first, concurrent) = tokio::join!(
        PostgresStateStore::migrate(&first_runner),
        PostgresStateStore::migrate(&contender)
    );
    let first = first?;
    let concurrent = concurrent?;
    assert_eq!(first.applied() + concurrent.applied(), 1);
    assert_eq!(first.current_version(), 1);
    assert_eq!(concurrent.current_version(), 1);
    let second = PostgresStateStore::migrate(&pool).await?;
    assert_eq!(second.current_version(), 1);
    assert_eq!(second.applied(), 0);
    let default_read_only = connect(
        with_connection_options(&dsn, "-c%20default_transaction_read_only%3Don"),
        1,
    )
    .await?;
    assert_eq!(
        PostgresStateStore::migrate(&default_read_only)
            .await?
            .applied(),
        0
    );

    let lock_holder = pool.begin().await?;
    lock_holder
        .execute(statement(
            "SELECT pg_catalog.pg_advisory_xact_lock($1)",
            vec![MIGRATION_LOCK_KEY.into()],
        ))
        .await?;
    let default = PostgresStateConfig::default();
    let bounded = PostgresStateConfig::new(
        default.limits(),
        Duration::from_millis(250),
        Duration::from_millis(100),
        default.definitive_abort_retries(),
        default.ambiguous_commit_recovery_attempts(),
        default.durability(),
    )?;
    let started = Instant::now();
    assert!(matches!(
        PostgresStateStore::migrate_with_config(&contender, bounded).await,
        Err(PostgresOpenError::Migration(MigrationError::Database(_)))
    ));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded migration lock wait exceeded two seconds"
    );
    lock_holder.rollback().await?;
    let store = PostgresStateStore::open(pool.clone(), PostgresStateConfig::default()).await?;
    assert_eq!(
        store.contract().writer_topology(),
        WriterTopology::SerializableMultiWriter
    );
    assert_eq!(
        store.contract().authority_class(),
        AuthorityClass::Production
    );

    pool.execute_unprepared(
        r#"UPDATE "public"."w9pt_fs_state_schema_migrations"
           SET "version" = 2 WHERE "version" = 1"#,
    )
    .await?;
    assert!(matches!(
        PostgresStateStore::migrate(&pool).await,
        Err(PostgresOpenError::Migration(MigrationError::Drift(_)))
    ));
    pool.execute_unprepared(
        r#"UPDATE "public"."w9pt_fs_state_schema_migrations"
           SET "version" = 1 WHERE "version" = 2"#,
    )
    .await?;
    pool.execute(statement(
        r#"INSERT INTO "public"."w9pt_fs_state_schema_migrations"
               ("version", "checksum") VALUES ($1, $2)"#,
        vec![2_i32.into(), vec![0_u8; 32].into()],
    ))
    .await?;
    assert!(matches!(
        PostgresStateStore::migrate(&pool).await,
        Err(PostgresOpenError::Migration(MigrationError::Drift(_)))
    ));
    pool.execute_unprepared(
        r#"DELETE FROM "public"."w9pt_fs_state_schema_migrations" WHERE "version" = 2;
           UPDATE "public"."w9pt_fs_state_schema_migrations"
           SET "checksum" = decode(
               '0000000000000000000000000000000000000000000000000000000000000000',
               'hex'
           ) WHERE "version" = 1;"#,
    )
    .await?;
    assert!(matches!(
        PostgresStateStore::migrate(&pool).await,
        Err(PostgresOpenError::Migration(MigrationError::Drift(_)))
    ));

    pool.execute_unprepared(RESET_PUBLIC_STATE_TABLES).await?;
    PostgresStateStore::migrate(&pool).await?;
    assert_eq!(
        catalog_count(&pool, COLUMN_CATALOG_COUNT_SQL).await?,
        DIRECT_CUT_V1_COLUMN_COUNT
    );
    assert_eq!(
        catalog_count(&pool, CONSTRAINT_CATALOG_COUNT_SQL).await?,
        DIRECT_CUT_V1_CONSTRAINT_COUNT
    );
    assert_eq!(
        catalog_count(&pool, INDEX_CATALOG_COUNT_SQL).await?,
        DIRECT_CUT_V1_INDEX_COUNT
    );
    assert_eq!(
        catalog_md5(&pool, COLUMN_CATALOG_MD5_SQL).await?,
        DIRECT_CUT_V1_COLUMN_CATALOG_MD5
    );
    assert_eq!(
        catalog_md5(&pool, CONSTRAINT_CATALOG_MD5_SQL).await?,
        DIRECT_CUT_V1_CONSTRAINT_CATALOG_MD5
    );
    assert_eq!(
        catalog_md5(&pool, INDEX_CATALOG_MD5_SQL).await?,
        DIRECT_CUT_V1_INDEX_CATALOG_MD5
    );

    let invalid = pool
        .execute(statement(
            r#"INSERT INTO "public"."w9pt_fs_state_writer_lease_operations" (
               "filesystem_id", "lease_operation_id", "operation_kind",
               "request_fingerprint", "outcome_tag"
           ) VALUES ($1, $2, 3, $3, 5)"#,
            vec![
                vec![0xab_u8; 16].into(),
                vec![0xcd_u8; 16].into(),
                vec![0xef_u8; 32].into(),
            ],
        ))
        .await
        .expect_err("release outcome tag 5 must be rejected");
    assert_eq!(
        native_code_and_constraint(&invalid),
        Some((
            "23514".to_owned(),
            Some("w9pt_fs_state_writer_lease_operations_outcome_tag".to_owned())
        ))
    );
    Ok(())
}
