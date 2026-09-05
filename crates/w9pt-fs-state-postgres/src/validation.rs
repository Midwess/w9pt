//! Writable-primary, schema, privilege, and durability validation at open.

use core::fmt;
use std::collections::BTreeMap;

use sea_orm::DbErr;
use w9pt_fs_state::{StateLimitValues, StateLimits};

use crate::{
    MigrationError, PostgresOpenError, PostgresStateConfig, PrimaryWalDurability, SchemaLimit,
    config::SCHEMA_LIMITS,
    database::{
        PostgresConnection, PostgresTransaction, TransactionAccess, begin_serializable_transaction,
        query, query_scalar, set_transaction_local_timeout,
    },
    migration::{
        MIGRATION_LEDGER_SQL, MIGRATIONS, PRODUCTION_SCHEMA, migration_ledger_query_limit,
    },
    schema::TABLE_NAMES,
};

const MINIMUM_POSTGRES_MAJOR: i64 = 15;
const MAXIMUM_POSTGRES_MAJOR: i64 = 18;

const PRODUCTION_TABLES: &[&str] = TABLE_NAMES;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RuntimePrivilege {
    Select,
    Insert,
    Update,
    Delete,
}

impl RuntimePrivilege {
    const fn as_sql(self) -> &'static str {
        match self {
            Self::Select => "SELECT",
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }
}

impl fmt::Display for RuntimePrivilege {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_sql())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PrivilegeRequirement {
    relation: &'static str,
    privilege: RuntimePrivilege,
}

const fn privilege(relation: &'static str, privilege: RuntimePrivilege) -> PrivilegeRequirement {
    PrivilegeRequirement {
        relation,
        privilege,
    }
}

// This is the least privilege set required by the version-1 runtime. The
// migration ledger is read-only to the runtime role. Immutable ledgers are
// inserted but not updated, and retained change history is deleted only by
// bounded compaction.
const RUNTIME_PRIVILEGES: &[PrivilegeRequirement] = &[
    privilege("w9pt_fs_state_schema_migrations", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_authority_heads", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_authority_heads", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_authority_heads", RuntimePrivilege::Update),
    privilege("w9pt_fs_state_filesystem_records", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_filesystem_records", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_filesystem_records", RuntimePrivilege::Update),
    privilege("w9pt_fs_state_filesystem_records", RuntimePrivilege::Delete),
    privilege("w9pt_fs_state_inodes", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_inodes", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_inodes", RuntimePrivilege::Update),
    privilege("w9pt_fs_state_inodes", RuntimePrivilege::Delete),
    privilege("w9pt_fs_state_directory_entries", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_directory_entries", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_directory_entries", RuntimePrivilege::Update),
    privilege("w9pt_fs_state_directory_entries", RuntimePrivilege::Delete),
    privilege("w9pt_fs_state_opens", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_opens", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_opens", RuntimePrivilege::Update),
    privilege("w9pt_fs_state_opens", RuntimePrivilege::Delete),
    privilege("w9pt_fs_state_open_pins", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_open_pins", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_open_pins", RuntimePrivilege::Update),
    privilege("w9pt_fs_state_open_pins", RuntimePrivilege::Delete),
    privilege("w9pt_fs_state_orphans", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_orphans", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_orphans", RuntimePrivilege::Update),
    privilege("w9pt_fs_state_orphans", RuntimePrivilege::Delete),
    privilege("w9pt_fs_state_locks", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_locks", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_locks", RuntimePrivilege::Update),
    privilege("w9pt_fs_state_locks", RuntimePrivilege::Delete),
    privilege("w9pt_fs_state_xattrs", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_xattrs", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_xattrs", RuntimePrivilege::Update),
    privilege("w9pt_fs_state_xattrs", RuntimePrivilege::Delete),
    privilege("w9pt_fs_state_xattr_staging", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_xattr_staging", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_xattr_staging", RuntimePrivilege::Update),
    privilege("w9pt_fs_state_xattr_staging", RuntimePrivilege::Delete),
    privilege("w9pt_fs_state_mutation_results", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_mutation_results", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_writer_fences", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_writer_fences", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_writer_fences", RuntimePrivilege::Update),
    privilege(
        "w9pt_fs_state_writer_lease_operations",
        RuntimePrivilege::Select,
    ),
    privilege(
        "w9pt_fs_state_writer_lease_operations",
        RuntimePrivilege::Insert,
    ),
    privilege("w9pt_fs_state_change_commits", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_change_commits", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_change_commits", RuntimePrivilege::Delete),
    privilege("w9pt_fs_state_change_keys", RuntimePrivilege::Select),
    privilege("w9pt_fs_state_change_keys", RuntimePrivilege::Insert),
    privilege("w9pt_fs_state_change_keys", RuntimePrivilege::Delete),
];

/// Typed reason that a PostgreSQL pool cannot back the production contract.
#[derive(Debug)]
pub(crate) enum OpenValidationError {
    /// A catalog or settings query failed.
    Database {
        /// Stable validation phase.
        step: &'static str,
        /// Native PostgreSQL/driver failure.
        source: DbErr,
    },
    /// The server major version is outside the supported range.
    UnsupportedServerVersion {
        /// PostgreSQL's integer `server_version_num`.
        version_number: i64,
        /// Major version decoded from the number.
        major: i64,
    },
    /// The selected connection is routed to a standby.
    RecoveryRoute,
    /// The selected connection defaults transactions to read-only.
    DefaultReadOnlyRoute,
    /// A requested read-write serializable transaction remained read-only.
    TransactionReadOnly,
    /// The transaction did not enter serializable isolation.
    UnexpectedTransactionIsolation {
        /// Isolation setting returned by PostgreSQL.
        actual: String,
    },
    /// A durability-critical server setting is disabled.
    DurabilitySettingDisabled {
        /// PostgreSQL setting name.
        setting: &'static str,
    },
    /// Transaction-local synchronous commit could not be selected.
    CannotEnforceSynchronousCommit {
        /// Native failure returned by `SET LOCAL`.
        source: DbErr,
    },
    /// PostgreSQL did not retain the transaction-local setting exactly.
    UnexpectedSynchronousCommit {
        /// Setting value observed after `SET LOCAL`.
        actual: String,
    },
    /// A configured portable-state limit exceeds the compiled schema bound.
    ConfiguredLimitExceedsSchema {
        /// Stable state limit category.
        limit: SchemaLimit,
        /// Requested adapter bound.
        actual: u64,
        /// Immutable version-1 schema maximum.
        maximum: u64,
    },
    /// The fixed production schema is absent.
    MissingSchema,
    /// A required production table is absent.
    MissingTable {
        /// Unqualified fixed table name.
        table: &'static str,
    },
    /// A required table is not an ordinary table as migrated.
    UnexpectedRelationKind {
        /// Unqualified fixed table name.
        table: &'static str,
        /// PostgreSQL `pg_class.relkind` value.
        actual: String,
    },
    /// A required table is temporary or unlogged.
    NonPermanentTable {
        /// Unqualified fixed table name.
        table: &'static str,
        /// PostgreSQL `pg_class.relpersistence` value.
        actual: String,
    },
    /// The runtime role cannot use the fixed schema.
    MissingSchemaUsage,
    /// The runtime role lacks one exact DML privilege.
    MissingTablePrivilege {
        /// Unqualified fixed table name.
        table: String,
        /// PostgreSQL privilege name.
        privilege: String,
    },
    /// The embedded migration set cannot form a bounded PostgreSQL query limit.
    InvalidEmbeddedMigrationSet {
        /// Checked conversion failure.
        detail: String,
    },
    /// The applied migration ledger has a missing or reordered version.
    MigrationVersionMismatch {
        /// Version required at this ledger position.
        expected: i32,
        /// Version stored by PostgreSQL, or `None` when pending.
        actual: Option<i32>,
    },
    /// The database contains a migration newer than this binary knows.
    UnknownMigrationVersion {
        /// First unsupported applied version.
        version: i32,
    },
    /// An applied migration checksum differs from the embedded SQL.
    MigrationChecksumMismatch {
        /// Migration whose immutable checksum differs.
        version: i32,
    },
}

impl OpenValidationError {
    fn database(step: &'static str, source: DbErr) -> Self {
        Self::Database { step, source }
    }
}

impl fmt::Display for OpenValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database { step, source } => {
                write!(formatter, "database error while {step}: {source}")
            }
            Self::UnsupportedServerVersion {
                version_number,
                major,
            } => write!(
                formatter,
                "unsupported PostgreSQL server_version_num {version_number} (major {major}); expected major 15 through 18"
            ),
            Self::RecoveryRoute => {
                formatter.write_str("connection is routed to a PostgreSQL server in recovery")
            }
            Self::DefaultReadOnlyRoute => formatter
                .write_str("connection defaults transactions to a write-disabled read-only route"),
            Self::TransactionReadOnly => formatter
                .write_str("serializable read-write validation transaction remained read-only"),
            Self::UnexpectedTransactionIsolation { actual } => write!(
                formatter,
                "validation transaction isolation is {actual}, expected serializable"
            ),
            Self::DurabilitySettingDisabled { setting } => {
                write!(
                    formatter,
                    "required PostgreSQL durability setting {setting} is off"
                )
            }
            Self::CannotEnforceSynchronousCommit { source } => write!(
                formatter,
                "cannot enforce transaction-local synchronous_commit=on: {source}"
            ),
            Self::UnexpectedSynchronousCommit { actual } => write!(
                formatter,
                "transaction-local synchronous_commit is {actual}, expected on"
            ),
            Self::ConfiguredLimitExceedsSchema {
                limit,
                actual,
                maximum,
            } => write!(
                formatter,
                "configured state limit {limit:?} value {actual} exceeds schema maximum {maximum}"
            ),
            Self::MissingSchema => write!(
                formatter,
                "required schema {PRODUCTION_SCHEMA} is absent; run explicit migrations"
            ),
            Self::MissingTable { table } => write!(
                formatter,
                "required table {PRODUCTION_SCHEMA}.{table} is absent; run explicit migrations"
            ),
            Self::UnexpectedRelationKind { table, actual } => write!(
                formatter,
                "required relation {PRODUCTION_SCHEMA}.{table} has relkind {actual}, expected ordinary table"
            ),
            Self::NonPermanentTable { table, actual } => write!(
                formatter,
                "required relation {PRODUCTION_SCHEMA}.{table} has relpersistence {actual}, expected permanent logged table"
            ),
            Self::MissingSchemaUsage => write!(
                formatter,
                "runtime role lacks USAGE on schema {PRODUCTION_SCHEMA}"
            ),
            Self::MissingTablePrivilege { table, privilege } => write!(
                formatter,
                "runtime role lacks {privilege} on {PRODUCTION_SCHEMA}.{table}"
            ),
            Self::InvalidEmbeddedMigrationSet { detail } => {
                write!(formatter, "invalid embedded migration set: {detail}")
            }
            Self::MigrationVersionMismatch { expected, actual } => match actual {
                Some(actual) => write!(
                    formatter,
                    "migration ledger expected version {expected}, found {actual}"
                ),
                None => write!(
                    formatter,
                    "migration version {expected} is pending; run explicit migrations"
                ),
            },
            Self::UnknownMigrationVersion { version } => write!(
                formatter,
                "database contains unknown migration version {version}"
            ),
            Self::MigrationChecksumMismatch { version } => {
                write!(
                    formatter,
                    "checksum differs for migration version {version}"
                )
            }
        }
    }
}

impl std::error::Error for OpenValidationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database { source, .. } | Self::CannotEnforceSynchronousCommit { source } => {
                Some(source)
            }
            _ => None,
        }
    }
}

impl From<OpenValidationError> for PostgresOpenError {
    fn from(error: OpenValidationError) -> Self {
        match error {
            OpenValidationError::Database { source, .. }
            | OpenValidationError::CannotEnforceSynchronousCommit { source } => {
                Self::Database(source)
            }
            error @ (OpenValidationError::MigrationVersionMismatch { .. }
            | OpenValidationError::UnknownMigrationVersion { .. }
            | OpenValidationError::MigrationChecksumMismatch { .. }) => {
                Self::Migration(MigrationError::Drift(error.to_string()))
            }
            error => Self::Validation(error.to_string()),
        }
    }
}

#[derive(Clone, Debug)]
struct AppliedMigration {
    version: i32,
    checksum: Vec<u8>,
}

#[derive(Debug)]
struct RelationCatalogEntry {
    kind: String,
    persistence: String,
}

/// Validates that one acquired route can uphold the advertised production contract.
pub(crate) async fn validate_open(
    database: &PostgresConnection,
    config: &PostgresStateConfig,
) -> Result<(), OpenValidationError> {
    validate_configured_limits(config.limits())?;

    let transaction = begin_serializable_transaction(database, TransactionAccess::ReadWrite)
        .await
        .map_err(|error| OpenValidationError::database("acquiring validation connection", error))?;
    set_transaction_local_timeout(
        &transaction,
        "statement_timeout",
        config.statement_timeout(),
    )
    .await
    .map_err(|error| {
        OpenValidationError::database("setting validation statement timeout", error)
    })?;
    set_transaction_local_timeout(&transaction, "lock_timeout", config.lock_timeout())
        .await
        .map_err(|error| OpenValidationError::database("setting validation lock timeout", error))?;

    let route = query(
        r#"SELECT
               current_setting('server_version_num')::bigint AS server_version_number,
               pg_catalog.pg_is_in_recovery() AS in_recovery,
               current_setting('default_transaction_read_only')::boolean AS default_read_only,
               current_setting('fsync')::boolean AS fsync,
               current_setting('full_page_writes')::boolean AS full_page_writes"#,
    )
    .fetch_one(&transaction)
    .await
    .map_err(|error| OpenValidationError::database("reading server and route settings", error))?;
    let version_number: i64 = route
        .try_get("server_version_number")
        .map_err(|error| OpenValidationError::database("decoding server version", error))?;
    validate_server_version(version_number)?;
    let in_recovery = route
        .try_get::<bool>("in_recovery")
        .map_err(|error| OpenValidationError::database("decoding recovery state", error))?;
    let default_read_only = route
        .try_get::<bool>("default_read_only")
        .map_err(|error| {
            OpenValidationError::database("decoding default transaction mode", error)
        })?;

    validate_route(in_recovery, default_read_only)?;

    if matches!(config.durability(), PrimaryWalDurability::Required) {
        let fsync = route
            .try_get::<bool>("fsync")
            .map_err(|error| OpenValidationError::database("decoding fsync", error))?;
        let full_page_writes = route
            .try_get::<bool>("full_page_writes")
            .map_err(|error| OpenValidationError::database("decoding full_page_writes", error))?;
        validate_durability_settings(fsync, full_page_writes)?;
    }

    let transaction_settings = query(
        r#"SELECT
               current_setting('transaction_isolation') AS isolation,
               current_setting('transaction_read_only')::boolean AS read_only"#,
    )
    .fetch_one(&transaction)
    .await
    .map_err(|error| {
        OpenValidationError::database("reading validation transaction settings", error)
    })?;
    let isolation: String = transaction_settings
        .try_get("isolation")
        .map_err(|error| OpenValidationError::database("decoding transaction isolation", error))?;
    if isolation != "serializable" {
        return Err(OpenValidationError::UnexpectedTransactionIsolation { actual: isolation });
    }
    if transaction_settings
        .try_get::<bool>("read_only")
        .map_err(|error| OpenValidationError::database("decoding transaction mode", error))?
    {
        return Err(OpenValidationError::TransactionReadOnly);
    }

    if let Err(source) = query("SET LOCAL synchronous_commit = 'on'")
        .execute(&transaction)
        .await
    {
        return Err(OpenValidationError::CannotEnforceSynchronousCommit { source });
    }
    let synchronous_commit: String = query_scalar("SELECT current_setting('synchronous_commit')")
        .fetch_one(&transaction)
        .await
        .map_err(|error| {
            OpenValidationError::database("reading transaction-local synchronous_commit", error)
        })?;
    if synchronous_commit != "on" {
        return Err(OpenValidationError::UnexpectedSynchronousCommit {
            actual: synchronous_commit,
        });
    }

    validate_schema(&transaction).await?;
    validate_runtime_privileges(&transaction).await?;
    validate_migration_ledger(&transaction).await?;

    transaction.rollback().await.map_err(|error| {
        OpenValidationError::database("rolling back validation transaction", error)
    })?;
    Ok(())
}

fn validate_server_version(version_number: i64) -> Result<(), OpenValidationError> {
    let major = version_number / 10_000;
    if !(MINIMUM_POSTGRES_MAJOR..=MAXIMUM_POSTGRES_MAJOR).contains(&major) {
        return Err(OpenValidationError::UnsupportedServerVersion {
            version_number,
            major,
        });
    }
    Ok(())
}

fn validate_route(in_recovery: bool, default_read_only: bool) -> Result<(), OpenValidationError> {
    if in_recovery {
        return Err(OpenValidationError::RecoveryRoute);
    }
    if default_read_only {
        return Err(OpenValidationError::DefaultReadOnlyRoute);
    }
    Ok(())
}

fn validate_durability_settings(
    fsync: bool,
    full_page_writes: bool,
) -> Result<(), OpenValidationError> {
    if !fsync {
        return Err(OpenValidationError::DurabilitySettingDisabled { setting: "fsync" });
    }
    if !full_page_writes {
        return Err(OpenValidationError::DurabilitySettingDisabled {
            setting: "full_page_writes",
        });
    }
    Ok(())
}

fn validate_configured_limits(limits: StateLimits) -> Result<(), OpenValidationError> {
    let actual = limits.values();
    for (limit, actual, maximum) in limit_pairs(actual, SCHEMA_LIMITS) {
        if actual > maximum {
            return Err(OpenValidationError::ConfiguredLimitExceedsSchema {
                limit,
                actual,
                maximum,
            });
        }
    }
    Ok(())
}

fn limit_pairs(
    actual: StateLimitValues,
    maximum: StateLimitValues,
) -> [(SchemaLimit, u64, u64); 20] {
    [
        (
            SchemaLimit::EntryNameBytes,
            usize_u64(actual.max_entry_name_bytes),
            usize_u64(maximum.max_entry_name_bytes),
        ),
        (
            SchemaLimit::XattrNameBytes,
            usize_u64(actual.max_xattr_name_bytes),
            usize_u64(maximum.max_xattr_name_bytes),
        ),
        (
            SchemaLimit::XattrValueBytes,
            usize_u64(actual.max_xattr_value_bytes),
            usize_u64(maximum.max_xattr_value_bytes),
        ),
        (
            SchemaLimit::PrincipalBytes,
            usize_u64(actual.max_principal_bytes),
            usize_u64(maximum.max_principal_bytes),
        ),
        (
            SchemaLimit::GroupBytes,
            usize_u64(actual.max_group_bytes),
            usize_u64(maximum.max_group_bytes),
        ),
        (
            SchemaLimit::SymlinkBytes,
            usize_u64(actual.max_symlink_bytes),
            usize_u64(maximum.max_symlink_bytes),
        ),
        (
            SchemaLimit::MutationResultBytes,
            usize_u64(actual.max_mutation_result_bytes),
            usize_u64(maximum.max_mutation_result_bytes),
        ),
        (
            SchemaLimit::ReadQueries,
            u64::from(actual.max_read_queries),
            u64::from(maximum.max_read_queries),
        ),
        (
            SchemaLimit::ScanItems,
            u64::from(actual.max_scan_items),
            u64::from(maximum.max_scan_items),
        ),
        (
            SchemaLimit::ScanBytes,
            usize_u64(actual.max_scan_bytes),
            usize_u64(maximum.max_scan_bytes),
        ),
        (
            SchemaLimit::Preconditions,
            u64::from(actual.max_preconditions),
            u64::from(maximum.max_preconditions),
        ),
        (
            SchemaLimit::Changes,
            u64::from(actual.max_changes),
            u64::from(maximum.max_changes),
        ),
        (
            SchemaLimit::ChangeKeys,
            u64::from(actual.max_change_keys),
            u64::from(maximum.max_change_keys),
        ),
        (
            SchemaLimit::TransactionBytes,
            usize_u64(actual.max_transaction_bytes),
            usize_u64(maximum.max_transaction_bytes),
        ),
        (
            SchemaLimit::Locks,
            u64::from(actual.max_locks_per_request),
            u64::from(maximum.max_locks_per_request),
        ),
        (
            SchemaLimit::OpenPins,
            u64::from(actual.max_open_pins_per_request),
            u64::from(maximum.max_open_pins_per_request),
        ),
        (
            SchemaLimit::Xattrs,
            u64::from(actual.max_xattrs_per_request),
            u64::from(maximum.max_xattrs_per_request),
        ),
        (
            SchemaLimit::LeaseDurationTicks,
            actual.max_lease_duration_ticks,
            maximum.max_lease_duration_ticks,
        ),
        (
            SchemaLimit::LeaseOperationHistory,
            u64::from(actual.max_lease_operation_history),
            u64::from(maximum.max_lease_operation_history),
        ),
        (
            SchemaLimit::ChangeHistoryCommits,
            u64::from(actual.max_change_history_commits),
            u64::from(maximum.max_change_history_commits),
        ),
    ]
}

async fn validate_schema(transaction: &PostgresTransaction) -> Result<(), OpenValidationError> {
    let schema_exists: bool =
        query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = $1)")
            .bind(PRODUCTION_SCHEMA)
            .fetch_one(transaction)
            .await
            .map_err(|error| OpenValidationError::database("checking production schema", error))?;
    if !schema_exists {
        return Err(OpenValidationError::MissingSchema);
    }

    let relation_names: Vec<String> = PRODUCTION_TABLES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let rows = query(
        r#"SELECT
               class.relname AS relation_name,
               class.relkind::text AS relation_kind,
               class.relpersistence::text AS persistence
           FROM pg_catalog.pg_class AS class
           INNER JOIN pg_catalog.pg_namespace AS namespace
               ON namespace.oid = class.relnamespace
           WHERE namespace.nspname = $1
             AND class.relname = ANY($2::text[])"#,
    )
    .bind(PRODUCTION_SCHEMA)
    .bind(relation_names)
    .fetch_all(transaction)
    .await
    .map_err(|error| OpenValidationError::database("reading production table catalog", error))?;
    let mut relations = BTreeMap::new();
    for row in rows {
        let name: String = row
            .try_get("relation_name")
            .map_err(|error| OpenValidationError::database("decoding relation name", error))?;
        let kind: String = row
            .try_get("relation_kind")
            .map_err(|error| OpenValidationError::database("decoding relation kind", error))?;
        let persistence: String = row.try_get("persistence").map_err(|error| {
            OpenValidationError::database("decoding relation persistence", error)
        })?;
        relations.insert(name, RelationCatalogEntry { kind, persistence });
    }
    validate_relation_catalog(&relations)
}

fn validate_relation_catalog(
    relations: &BTreeMap<String, RelationCatalogEntry>,
) -> Result<(), OpenValidationError> {
    for &table in PRODUCTION_TABLES {
        let Some(relation) = relations.get(table) else {
            return Err(OpenValidationError::MissingTable { table });
        };
        if relation.kind != "r" {
            return Err(OpenValidationError::UnexpectedRelationKind {
                table,
                actual: relation.kind.clone(),
            });
        }
        if relation.persistence != "p" {
            return Err(OpenValidationError::NonPermanentTable {
                table,
                actual: relation.persistence.clone(),
            });
        }
    }
    Ok(())
}

async fn validate_runtime_privileges(
    transaction: &PostgresTransaction,
) -> Result<(), OpenValidationError> {
    let schema_usage: bool = query_scalar("SELECT pg_catalog.has_schema_privilege($1, 'USAGE')")
        .bind(PRODUCTION_SCHEMA)
        .fetch_one(transaction)
        .await
        .map_err(|error| OpenValidationError::database("checking schema privileges", error))?;
    if !schema_usage {
        return Err(OpenValidationError::MissingSchemaUsage);
    }

    let relations: Vec<String> = RUNTIME_PRIVILEGES
        .iter()
        .map(|requirement| requirement.relation.to_owned())
        .collect();
    let privileges: Vec<String> = RUNTIME_PRIVILEGES
        .iter()
        .map(|requirement| requirement.privilege.as_sql().to_owned())
        .collect();
    let missing = query(
        r#"SELECT required.relation_name, required.privilege_name
           FROM unnest($1::text[], $2::text[])
               AS required(relation_name, privilege_name)
           WHERE NOT pg_catalog.has_table_privilege(
               pg_catalog.format('%I.%I', $3::text, required.relation_name),
               required.privilege_name
           )
           ORDER BY required.relation_name, required.privilege_name
           LIMIT 1"#,
    )
    .bind(relations)
    .bind(privileges)
    .bind(PRODUCTION_SCHEMA)
    .fetch_optional(transaction)
    .await
    .map_err(|error| OpenValidationError::database("checking runtime table privileges", error))?;
    if let Some(row) = missing {
        return Err(OpenValidationError::MissingTablePrivilege {
            table: row.try_get("relation_name").map_err(|error| {
                OpenValidationError::database("decoding missing-privilege relation", error)
            })?,
            privilege: row.try_get("privilege_name").map_err(|error| {
                OpenValidationError::database("decoding missing table privilege", error)
            })?,
        });
    }
    Ok(())
}

async fn validate_migration_ledger(
    transaction: &PostgresTransaction,
) -> Result<(), OpenValidationError> {
    let row_limit = migration_ledger_query_limit()
        .map_err(|detail| OpenValidationError::InvalidEmbeddedMigrationSet { detail })?;
    let rows = query(MIGRATION_LEDGER_SQL)
        .bind(row_limit)
        .fetch_all(transaction)
        .await
        .map_err(|error| OpenValidationError::database("reading migration ledger", error))?;
    let mut applied = Vec::with_capacity(rows.len());
    for row in rows {
        applied.push(AppliedMigration {
            version: row.try_get("version").map_err(|error| {
                OpenValidationError::database("decoding migration version", error)
            })?,
            checksum: row.try_get("checksum").map_err(|error| {
                OpenValidationError::database("decoding migration checksum", error)
            })?,
        });
    }
    validate_applied_migrations(&applied)
}

fn validate_applied_migrations(applied: &[AppliedMigration]) -> Result<(), OpenValidationError> {
    for (index, row) in applied.iter().enumerate() {
        let expected_version =
            i32::try_from(index + 1).map_err(|_| OpenValidationError::UnknownMigrationVersion {
                version: row.version,
            })?;
        if row.version != expected_version {
            return Err(OpenValidationError::MigrationVersionMismatch {
                expected: expected_version,
                actual: Some(row.version),
            });
        }
        let Some(expected) = MIGRATIONS.get(index) else {
            return Err(OpenValidationError::UnknownMigrationVersion {
                version: row.version,
            });
        };
        if row.checksum.as_slice() != expected.checksum() {
            return Err(OpenValidationError::MigrationChecksumMismatch {
                version: row.version,
            });
        }
    }

    if applied.len() < MIGRATIONS.len() {
        let expected = MIGRATIONS[applied.len()].version;
        return Err(OpenValidationError::MigrationVersionMismatch {
            expected,
            actual: None,
        });
    }
    Ok(())
}

fn usize_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use w9pt_fs_state::{StateLimitValues, StateLimits};

    use super::*;

    fn exact_applied_migrations() -> Vec<AppliedMigration> {
        MIGRATIONS
            .iter()
            .map(|migration| AppliedMigration {
                version: migration.version,
                checksum: migration.checksum().to_vec(),
            })
            .collect()
    }

    fn exact_relation_catalog() -> BTreeMap<String, RelationCatalogEntry> {
        PRODUCTION_TABLES
            .iter()
            .map(|name| {
                (
                    (*name).to_owned(),
                    RelationCatalogEntry {
                        kind: "r".to_owned(),
                        persistence: "p".to_owned(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn only_postgres_15_through_18_are_supported() {
        for version in [150_000, 160_001, 170_999, 180_000] {
            assert!(validate_server_version(version).is_ok());
        }
        for version in [140_999, 190_000] {
            assert!(matches!(
                validate_server_version(version),
                Err(OpenValidationError::UnsupportedServerVersion { .. })
            ));
        }
    }

    #[test]
    fn standby_and_default_read_only_routes_are_rejected() {
        assert!(validate_route(false, false).is_ok());
        assert!(matches!(
            validate_route(true, false),
            Err(OpenValidationError::RecoveryRoute)
        ));
        assert!(matches!(
            validate_route(false, true),
            Err(OpenValidationError::DefaultReadOnlyRoute)
        ));
    }

    #[test]
    fn weakened_primary_wal_settings_are_rejected() {
        assert!(validate_durability_settings(true, true).is_ok());
        assert!(matches!(
            validate_durability_settings(false, true),
            Err(OpenValidationError::DurabilitySettingDisabled { setting: "fsync" })
        ));
        assert!(matches!(
            validate_durability_settings(true, false),
            Err(OpenValidationError::DurabilitySettingDisabled {
                setting: "full_page_writes"
            })
        ));
    }

    #[test]
    fn migration_ledger_must_be_complete_gap_free_and_exact() {
        let exact = exact_applied_migrations();
        assert!(validate_applied_migrations(&exact).is_ok());
        assert!(matches!(
            validate_applied_migrations(&[]),
            Err(OpenValidationError::MigrationVersionMismatch {
                expected: 1,
                actual: None
            })
        ));

        let mut wrong_checksum = exact.clone();
        wrong_checksum[0].checksum[0] ^= 0xff;
        assert!(matches!(
            validate_applied_migrations(&wrong_checksum),
            Err(OpenValidationError::MigrationChecksumMismatch { version: 1 })
        ));

        let mut newer = exact;
        newer.push(AppliedMigration {
            version: 2,
            checksum: vec![0; 32],
        });
        assert!(matches!(
            validate_applied_migrations(&newer),
            Err(OpenValidationError::UnknownMigrationVersion { version: 2 })
        ));
    }

    #[test]
    fn every_required_relation_must_be_an_ordinary_permanent_table() {
        let exact = exact_relation_catalog();
        assert!(validate_relation_catalog(&exact).is_ok());

        let mut missing = exact_relation_catalog();
        missing.remove("w9pt_fs_state_inodes");
        assert!(matches!(
            validate_relation_catalog(&missing),
            Err(OpenValidationError::MissingTable {
                table: "w9pt_fs_state_inodes"
            })
        ));

        let mut unlogged = exact_relation_catalog();
        unlogged.get_mut("w9pt_fs_state_locks").unwrap().persistence = "u".to_owned();
        assert!(matches!(
            validate_relation_catalog(&unlogged),
            Err(OpenValidationError::NonPermanentTable {
                table: "w9pt_fs_state_locks",
                ..
            })
        ));

        let mut view = exact_relation_catalog();
        view.get_mut("w9pt_fs_state_xattrs").unwrap().kind = "v".to_owned();
        assert!(matches!(
            validate_relation_catalog(&view),
            Err(OpenValidationError::UnexpectedRelationKind {
                table: "w9pt_fs_state_xattrs",
                ..
            })
        ));
    }

    #[test]
    fn defensive_limit_check_matches_compiled_schema_maxima() {
        assert!(validate_configured_limits(StateLimits::default()).is_ok());
        let limits = StateLimits::new(StateLimitValues {
            max_entry_name_bytes: SCHEMA_LIMITS.max_entry_name_bytes + 1,
            ..StateLimitValues::default()
        })
        .unwrap();
        assert!(matches!(
            validate_configured_limits(limits),
            Err(OpenValidationError::ConfiguredLimitExceedsSchema {
                limit: SchemaLimit::EntryNameBytes,
                ..
            })
        ));
    }

    #[test]
    fn privilege_requirements_are_unique_and_cover_every_runtime_table() {
        let unique: BTreeSet<_> = RUNTIME_PRIVILEGES.iter().copied().collect();
        assert_eq!(unique.len(), RUNTIME_PRIVILEGES.len());
        for table in PRODUCTION_TABLES {
            assert!(
                RUNTIME_PRIVILEGES
                    .iter()
                    .any(|requirement| requirement.relation == *table)
            );
        }
    }
}
