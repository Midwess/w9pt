//! Embedded schema steps for the current unreleased development build.

use crate::{
    MigrationError, MigrationReport, PostgresStateConfig,
    database::{
        PostgresConnection, begin_read_committed_write_transaction, query, query_scalar,
        set_transaction_local_timeout,
    },
    schema,
};

const MIGRATION_ADVISORY_LOCK: i64 = 0x7739_7074_6673_7631;
const PRODUCTION_RELATION_PREFIX: &str = "w9pt_fs_state_";
const MIGRATION_LEDGER_REGCLASS: &str = "public.w9pt_fs_state_schema_migrations";

/// Fixed production schema name used by every statement.
pub const PRODUCTION_SCHEMA: &str = "public";

/// One embedded schema step for the current build.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EmbeddedMigration {
    pub(crate) version: i32,
    pub(crate) sources: &'static [&'static str],
}

impl EmbeddedMigration {
    pub(crate) fn checksum(self) -> [u8; 32] {
        let source_length = self
            .sources
            .iter()
            .try_fold(0_usize, |length, source| length.checked_add(source.len()))
            .expect("embedded migration sources fit in addressable memory");
        let mut source = Vec::with_capacity(source_length);
        for part in self.sources {
            source.extend_from_slice(part.as_bytes());
        }
        *w9pt_fs_storage::Digest::blake3(&source).as_bytes()
    }
}

// The repository has never released a PostgreSQL schema. Earlier development
// schemas have no compatibility path: checksum drift fails closed and requires
// an explicit operator reset rather than detection, import, or upgrade logic.
pub(crate) const MIGRATIONS: &[EmbeddedMigration] = &[EmbeddedMigration {
    version: schema::INITIAL_MIGRATION_VERSION,
    sources: schema::INITIAL_MIGRATION_SOURCES,
}];

pub(crate) const MIGRATION_LEDGER_SQL: &str = r#"SELECT "version", "checksum"
FROM "public"."w9pt_fs_state_schema_migrations"
ORDER BY "version"
LIMIT $1"#;

pub(crate) fn migration_ledger_query_limit() -> Result<i64, String> {
    MIGRATIONS
        .len()
        .checked_add(1)
        .and_then(|limit| i64::try_from(limit).ok())
        .ok_or_else(|| "embedded migration lookahead limit exceeds i64".to_owned())
}

pub(crate) async fn migrate(
    database: &PostgresConnection,
    config: PostgresStateConfig,
) -> Result<MigrationReport, MigrationError> {
    let transaction = begin_read_committed_write_transaction(database)
        .await
        .map_err(MigrationError::Database)?;
    set_transaction_local_timeout(
        &transaction,
        "statement_timeout",
        config.statement_timeout(),
    )
    .await
    .map_err(MigrationError::Database)?;
    set_transaction_local_timeout(&transaction, "lock_timeout", config.lock_timeout())
        .await
        .map_err(MigrationError::Database)?;
    query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(MIGRATION_ADVISORY_LOCK)
        .execute(&transaction)
        .await
        .map_err(MigrationError::Database)?;
    reject_public_prefix_collision(&transaction).await?;
    schema::ensure_ledger(&transaction)
        .await
        .map_err(MigrationError::Database)?;

    let row_limit = migration_ledger_query_limit().map_err(MigrationError::Drift)?;
    let rows = query(MIGRATION_LEDGER_SQL)
        .bind(row_limit)
        .fetch_all(&transaction)
        .await
        .map_err(MigrationError::Database)?;

    for (index, row) in rows.iter().enumerate() {
        let version: i32 = row.try_get("version").map_err(MigrationError::Database)?;
        let checksum: Vec<u8> = row.try_get("checksum").map_err(MigrationError::Database)?;
        let expected_version = i32::try_from(index + 1)
            .map_err(|_| MigrationError::Drift("applied migration count exceeds i32".to_owned()))?;
        if version != expected_version {
            return Err(MigrationError::Drift(format!(
                "expected applied version {expected_version}, found {version}"
            )));
        }
        let Some(expected) = MIGRATIONS.get(index) else {
            return Err(MigrationError::Drift(format!(
                "database contains unknown newer migration version {version}"
            )));
        };
        if checksum.as_slice() != expected.checksum() {
            return Err(MigrationError::Drift(format!(
                "checksum differs for migration version {version}"
            )));
        }
    }

    let pending = MIGRATIONS.get(rows.len()..).ok_or_else(|| {
        MigrationError::Drift("database migration ledger is longer than embedded set".to_owned())
    })?;
    let mut applied = 0u32;
    for migration in pending {
        match migration.version {
            1 => schema::apply_initial(&transaction)
                .await
                .map_err(MigrationError::Database)?,
            version => {
                return Err(MigrationError::Drift(format!(
                    "no code-first migration implementation for version {version}"
                )));
            }
        }
        query(
            r#"INSERT INTO "public"."w9pt_fs_state_schema_migrations" ("version", "checksum")
               VALUES ($1, $2)"#,
        )
        .bind(migration.version)
        .bind(migration.checksum().to_vec())
        .execute(&transaction)
        .await
        .map_err(MigrationError::Database)?;
        applied = applied.checked_add(1).ok_or_else(|| {
            MigrationError::Drift("applied migration count overflowed u32".to_owned())
        })?;
    }

    let current_version = u32::try_from(MIGRATIONS.len())
        .map_err(|_| MigrationError::Drift("embedded migration count exceeds u32".to_owned()))?;
    transaction
        .commit()
        .await
        .map_err(MigrationError::Database)?;
    Ok(MigrationReport::new(applied, current_version))
}

async fn reject_public_prefix_collision(
    transaction: &crate::database::PostgresTransaction,
) -> Result<(), MigrationError> {
    let ledger_exists: bool = query_scalar("SELECT pg_catalog.to_regclass($1) IS NOT NULL")
        .bind(MIGRATION_LEDGER_REGCLASS)
        .fetch_one(transaction)
        .await
        .map_err(MigrationError::Database)?;
    if ledger_exists {
        return Ok(());
    }
    let collision: Option<String> = query_scalar(
        r#"SELECT class.relname::text
           FROM pg_catalog.pg_class AS class
           INNER JOIN pg_catalog.pg_namespace AS namespace
               ON namespace.oid = class.relnamespace
           WHERE namespace.nspname = 'public'
             AND pg_catalog.left(class.relname, pg_catalog.char_length($1)) = $1
           ORDER BY class.relname
           LIMIT 1"#,
    )
    .bind(PRODUCTION_RELATION_PREFIX)
    .fetch_optional(transaction)
    .await
    .map_err(MigrationError::Database)?;
    match collision {
        Some(relation) => Err(MigrationError::PublicSchemaCollision(relation)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECT_CUT_V1_CHECKSUM: [u8; 32] = [
        0x8f, 0x07, 0xd9, 0xfd, 0x1b, 0x64, 0x05, 0x81, 0x70, 0xc0, 0xed, 0x92, 0x6f, 0x0d, 0x59,
        0xa0, 0x4f, 0x37, 0x43, 0xb9, 0x08, 0x81, 0xed, 0x56, 0x86, 0x0e, 0x05, 0xb7, 0x12, 0x88,
        0x29, 0x6b,
    ];

    #[test]
    fn embedded_versions_are_gap_free_and_checksums_are_stable_width() {
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(migration.version, i32::try_from(index + 1).unwrap());
            assert_eq!(migration.checksum().len(), 32);
            assert!(
                migration
                    .sources
                    .iter()
                    .all(|source| source.ends_with('\n'))
            );
            assert!(
                migration
                    .sources
                    .iter()
                    .all(|source| !source.contains("\r\n"))
            );
        }
        assert_eq!(MIGRATIONS[0].checksum(), DIRECT_CUT_V1_CHECKSUM);
    }

    #[test]
    fn production_schema_is_fixed() {
        assert_eq!(PRODUCTION_SCHEMA, "public");
    }

    #[test]
    fn migration_ledger_read_has_one_bounded_lookahead_row() {
        assert!(MIGRATION_LEDGER_SQL.contains("LIMIT $1"));
        assert_eq!(
            migration_ledger_query_limit().unwrap(),
            i64::try_from(MIGRATIONS.len() + 1).unwrap()
        );
    }
}
