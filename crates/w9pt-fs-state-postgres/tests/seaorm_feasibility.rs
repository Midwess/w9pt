//! Feasibility gates for the SeaORM PostgreSQL driver facade.
//!
//! These tests intentionally use only SeaORM's public connection, transaction,
//! statement, row, result, and error APIs. Live checks run only when the caller
//! supplies `W9PT_POSTGRES_TEST_DSN`.

use std::time::Duration;

use sea_orm::{
    AccessMode, ConnectOptions, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection,
    DbErr, IsolationLevel, QueryResult, RuntimeErr, Statement, TransactionTrait, Value,
    error::{ConnAcquireErr, SqlxError},
};

const LIVE_DSN_ENV: &str = "W9PT_POSTGRES_TEST_DSN";

fn require_postgres(connection: &DatabaseConnection) -> Result<(), &'static str> {
    if matches!(
        connection,
        DatabaseConnection::SqlxPostgresPoolConnection(_)
    ) {
        Ok(())
    } else {
        Err("a PostgreSQL SeaORM connection is required")
    }
}

fn statement(sql: impl Into<String>, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
}

fn plain_statement(sql: impl Into<String>) -> Statement {
    Statement::from_string(DatabaseBackend::Postgres, sql)
}

async fn query_one(
    connection: &impl ConnectionTrait,
    sql: impl Into<String>,
    values: Vec<Value>,
) -> Result<QueryResult, DbErr> {
    connection
        .query_one(statement(sql, values))
        .await?
        .ok_or_else(|| DbErr::Custom("query returned no row".to_owned()))
}

async fn connect(dsn: &str, max_connections: u32) -> Result<DatabaseConnection, DbErr> {
    let mut options = ConnectOptions::new(dsn.to_owned());
    options
        .max_connections(max_connections)
        .min_connections(0)
        .sqlx_logging(false);
    Database::connect(options).await
}

fn native_code_and_constraint(error: &DbErr) -> Option<(String, Option<String>)> {
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

fn assert_native(error: &DbErr, code: &str, constraint: Option<&str>) {
    assert_eq!(
        native_code_and_constraint(error),
        Some((code.to_owned(), constraint.map(str::to_owned)))
    );
}

#[test]
fn disconnected_connections_are_rejected_without_backend_dispatch() {
    let disconnected = DatabaseConnection::default();
    assert_eq!(
        require_postgres(&disconnected),
        Err("a PostgreSQL SeaORM connection is required")
    );
}

#[tokio::test]
async fn caller_created_connections_are_postgres_and_independently_pooled()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var(LIVE_DSN_ENV) else {
        return Ok(());
    };
    let first = connect(&dsn, 1).await?;
    let second = connect(&dsn, 1).await?;
    require_postgres(&first)?;
    require_postgres(&second)?;

    let first_transaction = first.begin().await?;
    let second_transaction = second.begin().await?;
    let first_pid: i32 = query_one(
        &first_transaction,
        "SELECT pg_catalog.pg_backend_pid() AS backend_pid",
        Vec::new(),
    )
    .await?
    .try_get("", "backend_pid")?;
    let second_pid: i32 = query_one(
        &second_transaction,
        "SELECT pg_catalog.pg_backend_pid() AS backend_pid",
        Vec::new(),
    )
    .await?
    .try_get("", "backend_pid")?;

    assert_ne!(first_pid, second_pid, "two live pools shared one session");
    first_transaction.rollback().await?;
    second_transaction.rollback().await?;
    Ok(())
}

#[tokio::test]
async fn serializable_access_modes_are_visible_inside_transactions()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var(LIVE_DSN_ENV) else {
        return Ok(());
    };
    let database = connect(&dsn, 2).await?;

    let read = database
        .begin_with_config(
            Some(IsolationLevel::Serializable),
            Some(AccessMode::ReadOnly),
        )
        .await?;
    let read_settings = query_one(
        &read,
        r#"SELECT current_setting('transaction_isolation') AS isolation,
                  current_setting('transaction_read_only')::boolean AS read_only,
                  pg_catalog.pg_backend_pid() AS backend_pid"#,
        Vec::new(),
    )
    .await?;
    assert_eq!(
        read_settings.try_get::<String>("", "isolation")?,
        "serializable"
    );
    assert!(read_settings.try_get::<bool>("", "read_only")?);
    let read_pid = read_settings.try_get::<i32>("", "backend_pid")?;
    read.execute_unprepared("SET LOCAL statement_timeout = '1234ms'")
        .await?;
    let local_settings = query_one(
        &read,
        r#"SELECT current_setting('statement_timeout') AS statement_timeout,
                  pg_catalog.pg_backend_pid() AS backend_pid"#,
        Vec::new(),
    )
    .await?;
    assert_eq!(
        local_settings.try_get::<String>("", "statement_timeout")?,
        "1234ms"
    );
    assert_eq!(local_settings.try_get::<i32>("", "backend_pid")?, read_pid);
    read.commit().await?;

    let write = database
        .begin_with_config(
            Some(IsolationLevel::Serializable),
            Some(AccessMode::ReadWrite),
        )
        .await?;
    let write_settings = query_one(
        &write,
        r#"SELECT current_setting('transaction_isolation') AS isolation,
                  current_setting('transaction_read_only')::boolean AS read_only"#,
        Vec::new(),
    )
    .await?;
    assert_eq!(
        write_settings.try_get::<String>("", "isolation")?,
        "serializable"
    );
    assert!(!write_settings.try_get::<bool>("", "read_only")?);
    write.rollback().await?;

    let victim = database.begin().await?;
    let victim_pid = query_one(
        &victim,
        "SELECT pg_catalog.pg_backend_pid() AS backend_pid",
        Vec::new(),
    )
    .await?
    .try_get::<i32>("", "backend_pid")?;
    let terminator = connect(&dsn, 1).await?;
    let terminated = query_one(
        &terminator,
        "SELECT pg_catalog.pg_terminate_backend($1) AS terminated",
        vec![victim_pid.into()],
    )
    .await?
    .try_get::<bool>("", "terminated")?;
    assert!(terminated);
    victim
        .commit()
        .await
        .expect_err("commit on a terminated transaction must fail explicitly");

    let rollback_victim = database.begin().await?;
    let rollback_pid = query_one(
        &rollback_victim,
        "SELECT pg_catalog.pg_backend_pid() AS backend_pid",
        Vec::new(),
    )
    .await?
    .try_get::<i32>("", "backend_pid")?;
    let terminated = query_one(
        &terminator,
        "SELECT pg_catalog.pg_terminate_backend($1) AS terminated",
        vec![rollback_pid.into()],
    )
    .await?
    .try_get::<bool>("", "terminated")?;
    assert!(terminated);
    rollback_victim
        .rollback()
        .await
        .expect_err("rollback on a terminated transaction must fail explicitly");
    Ok(())
}

#[tokio::test]
async fn statement_binds_rows_and_affected_counts_cover_adapter_primitives()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var(LIVE_DSN_ENV) else {
        return Ok(());
    };
    let database = connect(&dsn, 1).await?;
    let transaction = database.begin().await?;
    let unsigned_max = u64::MAX.to_string();
    let bytes = vec![0, 1, 2, 254, 255];
    let row = query_one(
        &transaction,
        r#"SELECT $1::boolean AS bool_value,
                  $2::smallint AS small_value,
                  $3::integer AS int_value,
                  $4::bigint AS big_value,
                  $5::bytea AS bytes_value,
                  $6::text AS text_value,
                  $7::text AS nullable_some,
                  $8::text AS nullable_none,
                  $9::numeric::text AS numeric_text"#,
        vec![
            true.into(),
            i16::MIN.into(),
            i32::MAX.into(),
            i64::MIN.into(),
            bytes.clone().into(),
            "portable text".to_owned().into(),
            Some("present".to_owned()).into(),
            Option::<String>::None.into(),
            unsigned_max.clone().into(),
        ],
    )
    .await?;
    assert!(row.try_get::<bool>("", "bool_value")?);
    assert_eq!(row.try_get::<i16>("", "small_value")?, i16::MIN);
    assert_eq!(row.try_get::<i32>("", "int_value")?, i32::MAX);
    assert_eq!(row.try_get::<i64>("", "big_value")?, i64::MIN);
    assert_eq!(row.try_get::<Vec<u8>>("", "bytes_value")?, bytes);
    assert_eq!(row.try_get::<String>("", "text_value")?, "portable text");
    assert_eq!(
        row.try_get::<Option<String>>("", "nullable_some")?,
        Some("present".to_owned())
    );
    assert_eq!(row.try_get::<Option<String>>("", "nullable_none")?, None);
    assert_eq!(row.try_get::<String>("", "numeric_text")?, unsigned_max);

    transaction
        .execute_unprepared(
            "CREATE TEMP TABLE seaorm_feasibility_rows (id integer PRIMARY KEY, payload bytea) ON COMMIT DROP",
        )
        .await?;
    let inserted = transaction
        .execute(statement(
            "INSERT INTO seaorm_feasibility_rows (id, payload) VALUES ($1, $2)",
            vec![7_i32.into(), vec![9_u8, 8, 7].into()],
        ))
        .await?;
    assert_eq!(inserted.rows_affected(), 1);
    let untouched = transaction
        .execute(statement(
            "UPDATE seaorm_feasibility_rows SET payload = $1 WHERE id = $2",
            vec![vec![1_u8].into(), 8_i32.into()],
        ))
        .await?;
    assert_eq!(untouched.rows_affected(), 0);
    transaction.rollback().await?;
    Ok(())
}

#[tokio::test]
async fn native_sqlstate_and_named_constraint_are_available_through_seaorm()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var(LIVE_DSN_ENV) else {
        return Ok(());
    };
    let database = connect(&dsn, 1).await?;
    let transaction = database.begin().await?;
    transaction
        .execute_unprepared(
            r#"CREATE TEMP TABLE seaorm_feasibility_unique (
                   id integer,
                   CONSTRAINT seaorm_feasibility_unique_id UNIQUE (id)
               ) ON COMMIT DROP;
               INSERT INTO seaorm_feasibility_unique (id) VALUES (1);"#,
        )
        .await?;
    let error = transaction
        .execute(plain_statement(
            "INSERT INTO seaorm_feasibility_unique (id) VALUES (1)",
        ))
        .await
        .expect_err("duplicate insert must fail");
    assert_native(&error, "23505", Some("seaorm_feasibility_unique_id"));
    transaction.rollback().await?;

    let transaction = database.begin().await?;
    transaction
        .execute_unprepared(
            r#"CREATE TEMP TABLE seaorm_feasibility_check (
                   value integer,
                   CONSTRAINT seaorm_feasibility_value_positive CHECK (value > 0)
               ) ON COMMIT DROP;"#,
        )
        .await?;
    let error = transaction
        .execute(plain_statement(
            "INSERT INTO seaorm_feasibility_check (value) VALUES (0)",
        ))
        .await
        .expect_err("check violation must fail");
    assert_native(&error, "23514", Some("seaorm_feasibility_value_positive"));
    transaction.rollback().await?;

    let read_only = database
        .begin_with_config(
            Some(IsolationLevel::Serializable),
            Some(AccessMode::ReadOnly),
        )
        .await?;
    let error = read_only
        .execute_unprepared("CREATE TEMP TABLE seaorm_feasibility_read_only (id integer)")
        .await
        .expect_err("DDL in a read-only transaction must fail");
    assert_native(&error, "25006", None);
    read_only.rollback().await?;

    let timeout = database.begin().await?;
    timeout
        .execute_unprepared("SET LOCAL statement_timeout = '10ms'")
        .await?;
    let error = timeout
        .query_one(plain_statement("SELECT pg_catalog.pg_sleep(0.1)"))
        .await
        .expect_err("statement timeout must cancel the query");
    assert_native(&error, "57014", None);
    timeout.rollback().await?;
    Ok(())
}

#[tokio::test]
async fn serialization_and_deadlock_sqlstates_survive_the_seaorm_facade()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var(LIVE_DSN_ENV) else {
        return Ok(());
    };
    let database = connect(&dsn, 4).await?;
    database
        .execute_unprepared(
            r#"DROP TABLE IF EXISTS seaorm_feasibility_conflicts;
               CREATE TABLE seaorm_feasibility_conflicts (
                   id integer PRIMARY KEY,
                   value integer NOT NULL
               );
               INSERT INTO seaorm_feasibility_conflicts (id, value)
               VALUES (1, 0), (2, 0);"#,
        )
        .await?;

    let first = database
        .begin_with_config(
            Some(IsolationLevel::Serializable),
            Some(AccessMode::ReadWrite),
        )
        .await?;
    let second = database
        .begin_with_config(
            Some(IsolationLevel::Serializable),
            Some(AccessMode::ReadWrite),
        )
        .await?;
    query_one(
        &first,
        "SELECT value FROM seaorm_feasibility_conflicts WHERE id = 1",
        Vec::new(),
    )
    .await?;
    query_one(
        &second,
        "SELECT value FROM seaorm_feasibility_conflicts WHERE id = 1",
        Vec::new(),
    )
    .await?;
    first
        .execute(plain_statement(
            "UPDATE seaorm_feasibility_conflicts SET value = value + 1 WHERE id = 1",
        ))
        .await?;
    first.commit().await?;
    let serialization = second
        .execute(plain_statement(
            "UPDATE seaorm_feasibility_conflicts SET value = value + 1 WHERE id = 1",
        ))
        .await
        .expect_err("stale serializable writer must fail");
    assert_native(&serialization, "40001", None);
    second.rollback().await?;

    let first = database.begin().await?;
    let second = database.begin().await?;
    first
        .execute(plain_statement(
            "UPDATE seaorm_feasibility_conflicts SET value = value + 1 WHERE id = 1",
        ))
        .await?;
    second
        .execute(plain_statement(
            "UPDATE seaorm_feasibility_conflicts SET value = value + 1 WHERE id = 2",
        ))
        .await?;
    let (first_result, second_result) = tokio::join!(
        first.execute(plain_statement(
            "UPDATE seaorm_feasibility_conflicts SET value = value + 1 WHERE id = 2",
        )),
        second.execute(plain_statement(
            "UPDATE seaorm_feasibility_conflicts SET value = value + 1 WHERE id = 1",
        ))
    );
    let deadlock = match (first_result, second_result) {
        (Err(error), Ok(_)) | (Ok(_), Err(error)) => error,
        (Err(first_error), Err(second_error)) => {
            let first_code = native_code_and_constraint(&first_error);
            if first_code.as_ref().map(|(code, _)| code.as_str()) == Some("40P01") {
                first_error
            } else {
                second_error
            }
        }
        (Ok(_), Ok(_)) => panic!("deadlock cycle unexpectedly committed both statements"),
    };
    assert_native(&deadlock, "40P01", None);
    let _ = first.rollback().await;
    let _ = second.rollback().await;

    database
        .execute_unprepared("DROP TABLE seaorm_feasibility_conflicts")
        .await?;
    Ok(())
}

#[tokio::test]
async fn pool_connection_and_shutdown_failures_keep_public_error_kinds()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var(LIVE_DSN_ENV) else {
        return Ok(());
    };
    let mut options = ConnectOptions::new(dsn);
    options
        .max_connections(1)
        .min_connections(0)
        .acquire_timeout(Duration::from_millis(500))
        .sqlx_logging(false);
    let database = Database::connect(options).await?;
    let held = database.begin().await?;
    let pool_error = database
        .query_one(plain_statement("SELECT 1"))
        .await
        .expect_err("exhausted one-connection pool must time out");
    assert!(matches!(
        pool_error,
        DbErr::ConnectionAcquire(ConnAcquireErr::Timeout)
    ));
    held.rollback().await?;

    database.close_by_ref().await?;
    let closed_error = database
        .query_one(plain_statement("SELECT 1"))
        .await
        .expect_err("query through a closed pool must fail");
    assert!(matches!(
        closed_error,
        DbErr::ConnectionAcquire(ConnAcquireErr::ConnectionClosed)
            | DbErr::Query(RuntimeErr::SqlxError(SqlxError::PoolClosed))
    ));

    let mut refused =
        ConnectOptions::new("postgres://w9pt_test:w9pt_test@127.0.0.1:1/w9pt_test".to_owned());
    refused
        .max_connections(1)
        .connect_timeout(Duration::from_millis(100))
        .acquire_timeout(Duration::from_millis(100))
        .sqlx_logging(false);
    let connection_error = Database::connect(refused)
        .await
        .expect_err("refused PostgreSQL endpoint must fail connection setup");
    assert!(matches!(connection_error, DbErr::Conn(_)));
    Ok(())
}

#[tokio::test]
async fn deferred_failure_is_returned_by_the_explicit_commit_call()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var(LIVE_DSN_ENV) else {
        return Ok(());
    };
    let database = connect(&dsn, 1).await?;
    let transaction = database.begin().await?;
    transaction
        .execute_unprepared(
            r#"CREATE TEMP TABLE seaorm_feasibility_parent (
                   id integer PRIMARY KEY
               ) ON COMMIT DROP;
               CREATE TEMP TABLE seaorm_feasibility_child (
                   parent_id integer,
                   CONSTRAINT seaorm_feasibility_deferred_parent
                       FOREIGN KEY (parent_id)
                       REFERENCES seaorm_feasibility_parent (id)
                       DEFERRABLE INITIALLY DEFERRED
               ) ON COMMIT DROP;
               INSERT INTO seaorm_feasibility_child (parent_id) VALUES (99);"#,
        )
        .await?;

    let error = transaction
        .commit()
        .await
        .expect_err("deferred constraint must fail at commit");
    assert_eq!(
        native_code_and_constraint(&error),
        Some((
            "23503".to_owned(),
            Some("seaorm_feasibility_deferred_parent".to_owned())
        ))
    );
    Ok(())
}

#[tokio::test]
async fn transaction_advisory_lock_serializes_unprepared_migration_work()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var(LIVE_DSN_ENV) else {
        return Ok(());
    };
    let first = connect(&dsn, 1).await?;
    let second = connect(&dsn, 1).await?;
    let lock_key = 0x5739_5345_414f_524d_i64;

    let migration = first.begin().await?;
    migration
        .execute(statement(
            "SELECT pg_catalog.pg_advisory_xact_lock($1)",
            vec![lock_key.into()],
        ))
        .await?;
    migration
        .execute_unprepared(
            r#"CREATE TEMP TABLE seaorm_feasibility_migration (
                   version integer PRIMARY KEY,
                   checksum bytea NOT NULL
               ) ON COMMIT DROP;
               INSERT INTO seaorm_feasibility_migration (version, checksum)
               VALUES (1, decode('00010203', 'hex'));"#,
        )
        .await?;
    let count: i64 = query_one(
        &migration,
        "SELECT count(*)::bigint AS row_count FROM seaorm_feasibility_migration",
        Vec::new(),
    )
    .await?
    .try_get("", "row_count")?;
    assert_eq!(count, 1);

    let contender = second.begin().await?;
    let locked: bool = query_one(
        &contender,
        "SELECT pg_catalog.pg_try_advisory_xact_lock($1) AS locked",
        vec![lock_key.into()],
    )
    .await?
    .try_get("", "locked")?;
    assert!(!locked, "a concurrent migration acquired the held lock");

    migration.commit().await?;
    let locked_after_commit: bool = query_one(
        &contender,
        "SELECT pg_catalog.pg_try_advisory_xact_lock($1) AS locked",
        vec![lock_key.into()],
    )
    .await?
    .try_get("", "locked")?;
    assert!(
        locked_after_commit,
        "transaction commit did not release the lock"
    );
    contender.rollback().await?;
    Ok(())
}
