//! Initial public-schema migration scaffolded by exact `sea-orm-cli 1.1.20`.
//!
//! The table/column inventory was transferred from CLI-generated entities for
//! the reviewed source database. SeaQuery owns tables, columns, keys, and
//! indexes; explicit PostgreSQL fragments are limited to named CHECK
//! constraints and deferred foreign keys unsupported by SeaQuery 0.32.

use sea_orm::{
    DbBackend, EntityTrait, Iterable, PrimaryKeyArity, PrimaryKeyToColumn, PrimaryKeyTrait, Schema,
};
use sea_orm_migration::prelude::*;

use super::entities::{
    w9pt_fs_state_authority_heads as authority_heads,
    w9pt_fs_state_change_commits as change_commits, w9pt_fs_state_change_keys as change_keys,
    w9pt_fs_state_directory_entries as directory_entries,
    w9pt_fs_state_filesystem_records as filesystem_records, w9pt_fs_state_inodes as inodes,
    w9pt_fs_state_locks as locks, w9pt_fs_state_mutation_results as mutation_results,
    w9pt_fs_state_open_pins as open_pins, w9pt_fs_state_opens as opens,
    w9pt_fs_state_orphans as orphans, w9pt_fs_state_schema_migrations as schema_migrations,
    w9pt_fs_state_writer_fences as writer_fences,
    w9pt_fs_state_writer_lease_operations as writer_lease_operations,
    w9pt_fs_state_xattr_staging as xattr_staging, w9pt_fs_state_xattrs as xattrs,
};

const SCHEMA: &str = "public";

const SCHEMA_MIGRATIONS: &str = "w9pt_fs_state_schema_migrations";
const AUTHORITY_HEADS: &str = "w9pt_fs_state_authority_heads";
const FILESYSTEM_RECORDS: &str = "w9pt_fs_state_filesystem_records";
const INODES: &str = "w9pt_fs_state_inodes";
const DIRECTORY_ENTRIES: &str = "w9pt_fs_state_directory_entries";
const OPENS: &str = "w9pt_fs_state_opens";
const OPEN_PINS: &str = "w9pt_fs_state_open_pins";
const ORPHANS: &str = "w9pt_fs_state_orphans";
const LOCKS: &str = "w9pt_fs_state_locks";
const XATTRS: &str = "w9pt_fs_state_xattrs";
const XATTR_STAGING: &str = "w9pt_fs_state_xattr_staging";
const MUTATION_RESULTS: &str = "w9pt_fs_state_mutation_results";
const WRITER_FENCES: &str = "w9pt_fs_state_writer_fences";
const WRITER_LEASE_OPERATIONS: &str = "w9pt_fs_state_writer_lease_operations";
const CHANGE_COMMITS: &str = "w9pt_fs_state_change_commits";
const CHANGE_KEYS: &str = "w9pt_fs_state_change_keys";

pub(crate) const TABLE_NAMES: &[&str] = &[
    SCHEMA_MIGRATIONS,
    AUTHORITY_HEADS,
    FILESYSTEM_RECORDS,
    INODES,
    DIRECTORY_ENTRIES,
    OPENS,
    OPEN_PINS,
    ORPHANS,
    LOCKS,
    XATTRS,
    XATTR_STAGING,
    MUTATION_RESULTS,
    WRITER_FENCES,
    WRITER_LEASE_OPERATIONS,
    CHANGE_COMMITS,
    CHANGE_KEYS,
];

#[cfg(test)]
pub(crate) const INDEX_NAMES: &[&str] = &[
    "w9pt_fs_state_schema_migrations_pkey",
    "w9pt_fs_state_authority_heads_pkey",
    "w9pt_fs_state_filesystem_records_pkey",
    "w9pt_fs_state_inodes_pkey",
    "w9pt_fs_state_inodes_qid_path_unique",
    "w9pt_fs_state_directory_entries_pkey",
    "w9pt_fs_state_directory_entries_cookie_unique",
    "w9pt_fs_state_directory_entries_child_idx",
    "w9pt_fs_state_opens_pkey",
    "w9pt_fs_state_opens_inode_identity_unique",
    "w9pt_fs_state_opens_inode_idx",
    "w9pt_fs_state_open_pins_pkey",
    "w9pt_fs_state_orphans_pkey",
    "w9pt_fs_state_locks_pkey",
    "w9pt_fs_state_locks_conflict_idx",
    "w9pt_fs_state_xattrs_pkey",
    "w9pt_fs_state_xattr_staging_pkey",
    "w9pt_fs_state_xattr_staging_inode_idx",
    "w9pt_fs_state_mutation_results_pkey",
    "w9pt_fs_state_writer_fences_pkey",
    "w9pt_fs_state_writer_lease_operations_pkey",
    "w9pt_fs_state_change_commits_pkey",
    "w9pt_fs_state_change_keys_pkey",
];

#[cfg(test)]
pub(crate) const CONSTRAINT_NAMES: &[&str] = &[
    "w9pt_fs_state_schema_migrations_pkey",
    "w9pt_fs_state_schema_migrations_version_positive",
    "w9pt_fs_state_schema_migrations_checksum_width",
    "w9pt_fs_state_authority_heads_pkey",
    "w9pt_fs_state_authority_heads_filesystem_id_width",
    "w9pt_fs_state_authority_heads_current_revision_range",
    "w9pt_fs_state_authority_heads_oldest_revision_range",
    "w9pt_fs_state_authority_heads_retention_order",
    "w9pt_fs_state_filesystem_records_pkey",
    "w9pt_fs_state_filesystem_records_filesystem_id_width",
    "w9pt_fs_state_filesystem_records_root_inode_id_width",
    "w9pt_fs_state_filesystem_records_state_revision_range",
    "w9pt_fs_state_filesystem_records_record_revision_range",
    "w9pt_fs_state_filesystem_records_revision_match",
    "w9pt_fs_state_filesystem_records_next_qid_path_range",
    "w9pt_fs_state_filesystem_records_next_cookie_range",
    "w9pt_fs_state_filesystem_records_policy_generation_range",
    "w9pt_fs_state_filesystem_records_root_inode_fk",
    "w9pt_fs_state_inodes_pkey",
    "w9pt_fs_state_inodes_qid_path_unique",
    "w9pt_fs_state_inodes_filesystem_id_width",
    "w9pt_fs_state_inodes_inode_id_width",
    "w9pt_fs_state_inodes_qid_path_range",
    "w9pt_fs_state_inodes_record_revision_range",
    "w9pt_fs_state_inodes_mode_range",
    "w9pt_fs_state_inodes_owner_bounds",
    "w9pt_fs_state_inodes_group_bounds",
    "w9pt_fs_state_inodes_timestamp_nanoseconds_range",
    "w9pt_fs_state_inodes_logical_size_range",
    "w9pt_fs_state_inodes_link_count_range",
    "w9pt_fs_state_inodes_generation_range",
    "w9pt_fs_state_inodes_kind_tag",
    "w9pt_fs_state_inodes_owner_no_nul",
    "w9pt_fs_state_inodes_group_no_nul",
    "w9pt_fs_state_inodes_content_file_id_width",
    "w9pt_fs_state_inodes_data_generation_range",
    "w9pt_fs_state_inodes_content_generation_range",
    "w9pt_fs_state_inodes_content_logical_size_range",
    "w9pt_fs_state_inodes_content_manifest_key_bounds",
    "w9pt_fs_state_inodes_content_manifest_digest_width",
    "w9pt_fs_state_inodes_content_storage_method_tag",
    "w9pt_fs_state_inodes_directory_generation_range",
    "w9pt_fs_state_inodes_directory_parent_id_width",
    "w9pt_fs_state_inodes_directory_parent_shape",
    "w9pt_fs_state_inodes_directory_parent_fk",
    "w9pt_fs_state_inodes_symlink_target_bounds",
    "w9pt_fs_state_inodes_device_number_range",
    "w9pt_fs_state_inodes_content_fields_all_or_none",
    "w9pt_fs_state_inodes_regular_content_consistency",
    "w9pt_fs_state_inodes_kind_specific_shape",
    "w9pt_fs_state_directory_entries_pkey",
    "w9pt_fs_state_directory_entries_cookie_unique",
    "w9pt_fs_state_directory_entries_parent_fk",
    "w9pt_fs_state_directory_entries_child_fk",
    "w9pt_fs_state_directory_entries_filesystem_id_width",
    "w9pt_fs_state_directory_entries_parent_id_width",
    "w9pt_fs_state_directory_entries_child_id_width",
    "w9pt_fs_state_directory_entries_name_bounds",
    "w9pt_fs_state_directory_entries_name_forbidden",
    "w9pt_fs_state_directory_entries_cookie_range",
    "w9pt_fs_state_directory_entries_record_revision_range",
    "w9pt_fs_state_opens_pkey",
    "w9pt_fs_state_opens_inode_identity_unique",
    "w9pt_fs_state_opens_inode_fk",
    "w9pt_fs_state_opens_filesystem_id_width",
    "w9pt_fs_state_opens_open_id_width",
    "w9pt_fs_state_opens_inode_id_width",
    "w9pt_fs_state_opens_client_incarnation_width",
    "w9pt_fs_state_opens_access_tag",
    "w9pt_fs_state_opens_inode_generation_range",
    "w9pt_fs_state_opens_record_revision_range",
    "w9pt_fs_state_open_pins_pkey",
    "w9pt_fs_state_open_pins_inode_fk",
    "w9pt_fs_state_open_pins_open_inode_fk",
    "w9pt_fs_state_open_pins_filesystem_id_width",
    "w9pt_fs_state_open_pins_inode_id_width",
    "w9pt_fs_state_open_pins_open_id_width",
    "w9pt_fs_state_open_pins_record_revision_range",
    "w9pt_fs_state_orphans_pkey",
    "w9pt_fs_state_orphans_inode_fk",
    "w9pt_fs_state_orphans_filesystem_id_width",
    "w9pt_fs_state_orphans_inode_id_width",
    "w9pt_fs_state_orphans_open_pin_count_range",
    "w9pt_fs_state_orphans_orphaned_revision_range",
    "w9pt_fs_state_orphans_record_revision_range",
    "w9pt_fs_state_locks_pkey",
    "w9pt_fs_state_locks_inode_fk",
    "w9pt_fs_state_locks_owner_open_fk",
    "w9pt_fs_state_locks_filesystem_id_width",
    "w9pt_fs_state_locks_inode_id_width",
    "w9pt_fs_state_locks_lock_id_width",
    "w9pt_fs_state_locks_owner_client_width",
    "w9pt_fs_state_locks_owner_open_width",
    "w9pt_fs_state_locks_range_start_range",
    "w9pt_fs_state_locks_range_end_range",
    "w9pt_fs_state_locks_range_nonempty",
    "w9pt_fs_state_locks_kind_tag",
    "w9pt_fs_state_locks_generation_range",
    "w9pt_fs_state_locks_record_revision_range",
    "w9pt_fs_state_xattrs_pkey",
    "w9pt_fs_state_xattrs_inode_fk",
    "w9pt_fs_state_xattrs_filesystem_id_width",
    "w9pt_fs_state_xattrs_inode_id_width",
    "w9pt_fs_state_xattrs_name_bounds",
    "w9pt_fs_state_xattrs_name_no_nul",
    "w9pt_fs_state_xattrs_value_bound",
    "w9pt_fs_state_xattrs_record_revision_range",
    "w9pt_fs_state_xattr_staging_pkey",
    "w9pt_fs_state_xattr_staging_inode_fk",
    "w9pt_fs_state_xattr_staging_filesystem_id_width",
    "w9pt_fs_state_xattr_staging_staging_id_width",
    "w9pt_fs_state_xattr_staging_inode_id_width",
    "w9pt_fs_state_xattr_staging_name_bounds",
    "w9pt_fs_state_xattr_staging_name_no_nul",
    "w9pt_fs_state_xattr_staging_expected_size_range",
    "w9pt_fs_state_xattr_staging_bytes_bound",
    "w9pt_fs_state_xattr_staging_progress_bound",
    "w9pt_fs_state_xattr_staging_record_revision_range",
    "w9pt_fs_state_mutation_results_pkey",
    "w9pt_fs_state_mutation_results_filesystem_id_width",
    "w9pt_fs_state_mutation_results_mutation_id_width",
    "w9pt_fs_state_mutation_results_fingerprint_width",
    "w9pt_fs_state_mutation_results_client_incarnation_width",
    "w9pt_fs_state_mutation_results_writer_scope_width",
    "w9pt_fs_state_mutation_results_writer_incarnation_width",
    "w9pt_fs_state_mutation_results_fencing_token_range",
    "w9pt_fs_state_mutation_results_result_kind_range",
    "w9pt_fs_state_mutation_results_result_format_range",
    "w9pt_fs_state_mutation_results_result_bytes_bound",
    "w9pt_fs_state_mutation_results_committed_revision_range",
    "w9pt_fs_state_mutation_results_retention_range",
    "w9pt_fs_state_mutation_results_record_revision_range",
    "w9pt_fs_state_mutation_results_revision_match",
    "w9pt_fs_state_writer_fences_pkey",
    "w9pt_fs_state_writer_fences_filesystem_id_width",
    "w9pt_fs_state_writer_fences_scope_id_width",
    "w9pt_fs_state_writer_fences_token_range",
    "w9pt_fs_state_writer_fences_active_fields_all_or_none",
    "w9pt_fs_state_writer_fences_active_holder_width",
    "w9pt_fs_state_writer_fences_active_lease_width",
    "w9pt_fs_state_writer_fences_active_deadline_range",
    "w9pt_fs_state_writer_fences_active_revision_range",
    "w9pt_fs_state_writer_lease_operations_pkey",
    "w9pt_fs_state_writer_lease_operations_filesystem_id_width",
    "w9pt_fs_state_writer_lease_operations_operation_id_width",
    "w9pt_fs_state_writer_lease_operations_kind_tag",
    "w9pt_fs_state_writer_lease_operations_fingerprint_width",
    "w9pt_fs_state_writer_lease_operations_outcome_tag",
    "w9pt_fs_state_writer_lease_operations_grant_all_or_none",
    "w9pt_fs_state_writer_lease_operations_grant_shape",
    "w9pt_fs_state_writer_lease_operations_scope_width",
    "w9pt_fs_state_writer_lease_operations_holder_width",
    "w9pt_fs_state_writer_lease_operations_lease_id_width",
    "w9pt_fs_state_writer_lease_operations_deadline_range",
    "w9pt_fs_state_writer_lease_operations_token_range",
    "w9pt_fs_state_change_commits_pkey",
    "w9pt_fs_state_change_commits_filesystem_id_width",
    "w9pt_fs_state_change_commits_revision_range",
    "w9pt_fs_state_change_commits_origin_kind_tag",
    "w9pt_fs_state_change_commits_origin_id_width",
    "w9pt_fs_state_change_commits_key_count_range",
    "w9pt_fs_state_change_keys_pkey",
    "w9pt_fs_state_change_keys_commit_fk",
    "w9pt_fs_state_change_keys_filesystem_id_width",
    "w9pt_fs_state_change_keys_revision_range",
    "w9pt_fs_state_change_keys_ordinal_range",
    "w9pt_fs_state_change_keys_family_tag",
    "w9pt_fs_state_change_keys_component_a_width",
    "w9pt_fs_state_change_keys_component_b_bound",
    "w9pt_fs_state_change_keys_component_shape",
    "w9pt_fs_state_change_keys_component_b_shape",
    "w9pt_fs_state_change_keys_name_shape",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in production_tables() {
            manager.create_table(table).await?;
        }
        for index in production_indexes() {
            manager.create_index(index).await?;
        }
        for ddl in CHECK_CONSTRAINT_DDL {
            manager.get_connection().execute_unprepared(ddl).await?;
        }
        for ddl in DEFERRED_FOREIGN_KEY_DDL {
            manager.get_connection().execute_unprepared(ddl).await?;
        }
        Ok(())
    }
}

pub(super) fn ledger_table() -> TableCreateStatement {
    generated_table(
        schema_migrations::Entity,
        "w9pt_fs_state_schema_migrations_pkey",
        true,
    )
}

fn production_tables() -> Vec<TableCreateStatement> {
    vec![
        generated_table(
            authority_heads::Entity,
            "w9pt_fs_state_authority_heads_pkey",
            false,
        ),
        generated_table(
            filesystem_records::Entity,
            "w9pt_fs_state_filesystem_records_pkey",
            false,
        ),
        generated_table(inodes::Entity, "w9pt_fs_state_inodes_pkey", false),
        generated_table(
            directory_entries::Entity,
            "w9pt_fs_state_directory_entries_pkey",
            false,
        ),
        generated_table(opens::Entity, "w9pt_fs_state_opens_pkey", false),
        generated_table(open_pins::Entity, "w9pt_fs_state_open_pins_pkey", false),
        generated_table(orphans::Entity, "w9pt_fs_state_orphans_pkey", false),
        generated_table(locks::Entity, "w9pt_fs_state_locks_pkey", false),
        generated_table(xattrs::Entity, "w9pt_fs_state_xattrs_pkey", false),
        generated_table(
            xattr_staging::Entity,
            "w9pt_fs_state_xattr_staging_pkey",
            false,
        ),
        generated_table(
            mutation_results::Entity,
            "w9pt_fs_state_mutation_results_pkey",
            false,
        ),
        generated_table(
            writer_fences::Entity,
            "w9pt_fs_state_writer_fences_pkey",
            false,
        ),
        generated_table(
            writer_lease_operations::Entity,
            "w9pt_fs_state_writer_lease_operations_pkey",
            false,
        ),
        generated_table(
            change_commits::Entity,
            "w9pt_fs_state_change_commits_pkey",
            false,
        ),
        generated_table(change_keys::Entity, "w9pt_fs_state_change_keys_pkey", false),
    ]
}

/// Builds tables from database-first SeaORM entities while deliberately
/// deferring relationships to the reviewed PostgreSQL FK overlay below.
/// SeaORM codegen does not retain defaults, stable constraint names, or
/// deferred-constraint behavior, so those details are restored here.
fn generated_table<E>(
    entity: E,
    primary_key_name: &str,
    if_not_exists: bool,
) -> TableCreateStatement
where
    E: EntityTrait,
    E::PrimaryKey: PrimaryKeyToColumn<Column = E::Column>,
{
    let schema = Schema::new(DbBackend::Postgres);
    let mut table = Table::create();
    table.table(entity.table_ref());
    if if_not_exists {
        table.if_not_exists();
    }

    for column in E::Column::iter() {
        let column_name = column.to_string();
        let mut definition = schema.get_column_def::<E>(column);
        match (entity.table_name(), column_name.as_str()) {
            (SCHEMA_MIGRATIONS, "applied_at") => {
                definition.default(Expr::cust("clock_timestamp()"));
            }
            (AUTHORITY_HEADS, "current_revision" | "oldest_retained_revision") => {
                definition.default(1);
            }
            _ => {}
        }
        table.col(&mut definition);
    }

    if <<E::PrimaryKey as PrimaryKeyTrait>::ValueType as PrimaryKeyArity>::ARITY > 1 {
        let mut key = Index::create();
        key.name(primary_key_name).primary();
        for column in E::PrimaryKey::iter() {
            key.col(column);
        }
        table.primary_key(&mut key);
    }

    match entity.table_name() {
        INODES => add_unique_constraint(
            &mut table,
            "w9pt_fs_state_inodes_qid_path_unique",
            &["filesystem_id", "qid_path"],
        ),
        DIRECTORY_ENTRIES => add_unique_constraint(
            &mut table,
            "w9pt_fs_state_directory_entries_cookie_unique",
            &["filesystem_id", "parent_inode_id", "cookie"],
        ),
        OPENS => add_unique_constraint(
            &mut table,
            "w9pt_fs_state_opens_inode_identity_unique",
            &["filesystem_id", "inode_id", "open_id"],
        ),
        _ => {}
    }

    table.take()
}

fn production_indexes() -> Vec<IndexCreateStatement> {
    vec![
        index(
            "w9pt_fs_state_directory_entries_child_idx",
            DIRECTORY_ENTRIES,
            &["filesystem_id", "child_inode_id"],
        ),
        index(
            "w9pt_fs_state_opens_inode_idx",
            OPENS,
            &["filesystem_id", "inode_id", "open_id"],
        ),
        index(
            "w9pt_fs_state_locks_conflict_idx",
            LOCKS,
            &[
                "filesystem_id",
                "inode_id",
                "range_start",
                "range_end",
                "kind",
                "lock_id",
            ],
        ),
        index(
            "w9pt_fs_state_xattr_staging_inode_idx",
            XATTR_STAGING,
            &["filesystem_id", "inode_id", "staging_id"],
        ),
    ]
}

fn public_table(name: &str) -> (Alias, Alias) {
    (Alias::new(SCHEMA), Alias::new(name))
}

fn add_unique_constraint(table: &mut TableCreateStatement, name: &str, columns: &[&str]) {
    let mut key = Index::create();
    key.name(name).unique();
    for column in columns {
        key.col(Alias::new(*column));
    }
    table.index(&mut key);
}

fn index(name: &str, table: &str, columns: &[&str]) -> IndexCreateStatement {
    let mut index = Index::create();
    index.name(name).table(public_table(table));
    for column in columns {
        index.col(Alias::new(*column));
    }
    index.take()
}

// SeaQuery 0.32 cannot attach stable names to CHECK constraints. These
// PostgreSQL-only fragments preserve the reviewed names and expressions that
// the adapter uses for exact SQLSTATE/constraint classification.
const CHECK_CONSTRAINT_DDL: &[&str] = &[
    r#"ALTER TABLE "public"."w9pt_fs_state_schema_migrations"
        ADD CONSTRAINT "w9pt_fs_state_schema_migrations_version_positive" CHECK ("version" > 0),
        ADD CONSTRAINT "w9pt_fs_state_schema_migrations_checksum_width" CHECK (octet_length("checksum") = 32);"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_authority_heads"
        ADD CONSTRAINT "w9pt_fs_state_authority_heads_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_authority_heads_current_revision_range" CHECK (
            "current_revision" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_authority_heads_oldest_revision_range" CHECK (
            "oldest_retained_revision" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_authority_heads_retention_order" CHECK (
            "oldest_retained_revision" <= "current_revision"
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_filesystem_records"
        ADD CONSTRAINT "w9pt_fs_state_filesystem_records_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_filesystem_records_root_inode_id_width" CHECK (octet_length("root_inode_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_filesystem_records_state_revision_range" CHECK (
            "state_revision" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_filesystem_records_record_revision_range" CHECK (
            "record_revision" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_filesystem_records_revision_match" CHECK (
            "state_revision" = "record_revision"
        ),
        ADD CONSTRAINT "w9pt_fs_state_filesystem_records_next_qid_path_range" CHECK (
            "next_qid_path" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_filesystem_records_next_cookie_range" CHECK (
            "next_directory_cookie" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_filesystem_records_policy_generation_range" CHECK (
            "policy_generation" BETWEEN 1 AND 18446744073709551615
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_inodes"
        ADD CONSTRAINT "w9pt_fs_state_inodes_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_inodes_inode_id_width" CHECK (octet_length("inode_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_inodes_qid_path_range" CHECK (
            "qid_path" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_record_revision_range" CHECK (
            "record_revision" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_mode_range" CHECK ("mode" BETWEEN 0 AND 4095),
        ADD CONSTRAINT "w9pt_fs_state_inodes_owner_bounds" CHECK (octet_length("owner") BETWEEN 1 AND 65536),
        ADD CONSTRAINT "w9pt_fs_state_inodes_group_bounds" CHECK (octet_length("group_id") BETWEEN 1 AND 65536),
        ADD CONSTRAINT "w9pt_fs_state_inodes_timestamp_nanoseconds_range" CHECK (
            "accessed_nanoseconds" BETWEEN 0 AND 999999999
            AND "modified_nanoseconds" BETWEEN 0 AND 999999999
            AND "changed_nanoseconds" BETWEEN 0 AND 999999999
            AND "created_nanoseconds" BETWEEN 0 AND 999999999
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_logical_size_range" CHECK (
            "logical_size" BETWEEN 0 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_link_count_range" CHECK (
            "link_count" BETWEEN 0 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_generation_range" CHECK (
            "inode_generation" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_kind_tag" CHECK ("kind" BETWEEN 1 AND 7),
        ADD CONSTRAINT "w9pt_fs_state_inodes_owner_no_nul" CHECK (
            position(E'\\x00'::bytea IN "owner") = 0
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_group_no_nul" CHECK (
            position(E'\\x00'::bytea IN "group_id") = 0
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_content_file_id_width" CHECK (
            "content_file_id" IS NULL OR octet_length("content_file_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_data_generation_range" CHECK (
            "data_generation" IS NULL
            OR "data_generation" BETWEEN 0 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_content_generation_range" CHECK (
            "content_generation" IS NULL
            OR "content_generation" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_content_logical_size_range" CHECK (
            "content_logical_size" IS NULL
            OR "content_logical_size" BETWEEN 0 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_content_manifest_key_bounds" CHECK (
            "content_manifest_key" IS NULL
            OR (
                octet_length("content_manifest_key") BETWEEN 1 AND 67108864
                AND "content_manifest_key" COLLATE "C" !~ '[[:cntrl:]]'
            )
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_content_manifest_digest_width" CHECK (
            "content_manifest_digest" IS NULL
            OR octet_length("content_manifest_digest") = 32
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_content_storage_method_tag" CHECK (
            "content_storage_method" IS NULL OR "content_storage_method" IN (1, 2)
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_directory_generation_range" CHECK (
            "directory_generation" IS NULL
            OR "directory_generation" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_directory_parent_id_width" CHECK (
            "directory_parent_inode_id" IS NULL
            OR octet_length("directory_parent_inode_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_directory_parent_shape" CHECK (
            ("kind" = 2 AND "directory_parent_inode_id" IS NOT NULL)
            OR ("kind" <> 2 AND "directory_parent_inode_id" IS NULL)
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_symlink_target_bounds" CHECK (
            "symlink_target" IS NULL
            OR (
                octet_length("symlink_target") BETWEEN 1 AND 1048576
                AND position(E'\\x00'::bytea IN "symlink_target") = 0
            )
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_device_number_range" CHECK (
            ("device_major" IS NULL OR "device_major" BETWEEN 0 AND 4294967295)
            AND ("device_minor" IS NULL OR "device_minor" BETWEEN 0 AND 4294967295)
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_content_fields_all_or_none" CHECK (
            num_nonnulls(
                "content_generation",
                "content_logical_size",
                "content_manifest_key",
                "content_manifest_digest",
                "content_storage_method"
            ) IN (0, 5)
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_regular_content_consistency" CHECK (
            "kind" <> 1
            OR (
                "content_file_id" IS NOT NULL
                AND "data_generation" IS NOT NULL
                AND (
                    (
                        "data_generation" = 0
                        AND "logical_size" = 0
                        AND "content_generation" IS NULL
                    )
                    OR (
                        "data_generation" BETWEEN 1 AND 18446744073709551615
                        AND "content_generation" = "data_generation"
                        AND "content_logical_size" = "logical_size"
                    )
                )
            )
        ),
        ADD CONSTRAINT "w9pt_fs_state_inodes_kind_specific_shape" CHECK (
            (
                "kind" = 1
                AND "content_file_id" IS NOT NULL
                AND "data_generation" IS NOT NULL
                AND "directory_generation" IS NULL
                AND "symlink_target" IS NULL
                AND "device_major" IS NULL
                AND "device_minor" IS NULL
            )
            OR (
                "kind" = 2
                AND "logical_size" = 0
                AND "content_file_id" IS NULL
                AND "data_generation" IS NULL
                AND "content_generation" IS NULL
                AND "directory_generation" IS NOT NULL
                AND "symlink_target" IS NULL
                AND "device_major" IS NULL
                AND "device_minor" IS NULL
            )
            OR (
                "kind" = 3
                AND "logical_size" = octet_length("symlink_target")
                AND "content_file_id" IS NULL
                AND "data_generation" IS NULL
                AND "content_generation" IS NULL
                AND "directory_generation" IS NULL
                AND "symlink_target" IS NOT NULL
                AND "device_major" IS NULL
                AND "device_minor" IS NULL
            )
            OR (
                "kind" IN (4, 5)
                AND "logical_size" = 0
                AND "content_file_id" IS NULL
                AND "data_generation" IS NULL
                AND "content_generation" IS NULL
                AND "directory_generation" IS NULL
                AND "symlink_target" IS NULL
                AND "device_major" IS NOT NULL
                AND "device_minor" IS NOT NULL
            )
            OR (
                "kind" IN (6, 7)
                AND "logical_size" = 0
                AND "content_file_id" IS NULL
                AND "data_generation" IS NULL
                AND "content_generation" IS NULL
                AND "directory_generation" IS NULL
                AND "symlink_target" IS NULL
                AND "device_major" IS NULL
                AND "device_minor" IS NULL
            )
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_directory_entries"
        ADD CONSTRAINT "w9pt_fs_state_directory_entries_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_directory_entries_parent_id_width" CHECK (octet_length("parent_inode_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_directory_entries_child_id_width" CHECK (octet_length("child_inode_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_directory_entries_name_bounds" CHECK (
            octet_length("name") BETWEEN 1 AND 1024
        ),
        ADD CONSTRAINT "w9pt_fs_state_directory_entries_name_forbidden" CHECK (
            position(E'\\x00'::bytea IN "name") = 0
            AND position(E'\\x2f'::bytea IN "name") = 0
            AND "name" <> E'\\x2e'::bytea
            AND "name" <> E'\\x2e2e'::bytea
        ),
        ADD CONSTRAINT "w9pt_fs_state_directory_entries_cookie_range" CHECK (
            "cookie" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_directory_entries_record_revision_range" CHECK (
            "record_revision" BETWEEN 1 AND 18446744073709551615
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_opens"
        ADD CONSTRAINT "w9pt_fs_state_opens_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_opens_open_id_width" CHECK (octet_length("open_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_opens_inode_id_width" CHECK (octet_length("inode_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_opens_client_incarnation_width" CHECK (octet_length("client_incarnation_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_opens_access_tag" CHECK ("access" BETWEEN 1 AND 4),
        ADD CONSTRAINT "w9pt_fs_state_opens_inode_generation_range" CHECK (
            "retained_inode_generation" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_opens_record_revision_range" CHECK (
            "record_revision" BETWEEN 1 AND 18446744073709551615
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_open_pins"
        ADD CONSTRAINT "w9pt_fs_state_open_pins_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_open_pins_inode_id_width" CHECK (octet_length("inode_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_open_pins_open_id_width" CHECK (octet_length("open_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_open_pins_record_revision_range" CHECK (
            "record_revision" BETWEEN 1 AND 18446744073709551615
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_orphans"
        ADD CONSTRAINT "w9pt_fs_state_orphans_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_orphans_inode_id_width" CHECK (octet_length("inode_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_orphans_open_pin_count_range" CHECK (
            "open_pin_count" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_orphans_orphaned_revision_range" CHECK (
            "orphaned_revision" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_orphans_record_revision_range" CHECK (
            "record_revision" BETWEEN 1 AND 18446744073709551615
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_locks"
        ADD CONSTRAINT "w9pt_fs_state_locks_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_locks_inode_id_width" CHECK (octet_length("inode_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_locks_lock_id_width" CHECK (octet_length("lock_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_locks_owner_client_width" CHECK (
            octet_length("owner_client_incarnation_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_locks_owner_open_width" CHECK (octet_length("owner_open_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_locks_range_start_range" CHECK (
            "range_start" BETWEEN 0 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_locks_range_end_range" CHECK (
            "range_end" IS NULL
            OR "range_end" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_locks_range_nonempty" CHECK (
            "range_end" IS NULL OR "range_end" > "range_start"
        ),
        ADD CONSTRAINT "w9pt_fs_state_locks_kind_tag" CHECK ("kind" IN (1, 2)),
        ADD CONSTRAINT "w9pt_fs_state_locks_generation_range" CHECK (
            "lock_generation" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_locks_record_revision_range" CHECK (
            "record_revision" BETWEEN 1 AND 18446744073709551615
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_xattrs"
        ADD CONSTRAINT "w9pt_fs_state_xattrs_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_xattrs_inode_id_width" CHECK (octet_length("inode_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_xattrs_name_bounds" CHECK (octet_length("name") BETWEEN 1 AND 1024),
        ADD CONSTRAINT "w9pt_fs_state_xattrs_name_no_nul" CHECK (position(E'\\x00'::bytea IN "name") = 0),
        ADD CONSTRAINT "w9pt_fs_state_xattrs_value_bound" CHECK (octet_length("value") <= 1048576),
        ADD CONSTRAINT "w9pt_fs_state_xattrs_record_revision_range" CHECK (
            "record_revision" BETWEEN 1 AND 18446744073709551615
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_xattr_staging"
        ADD CONSTRAINT "w9pt_fs_state_xattr_staging_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_xattr_staging_staging_id_width" CHECK (octet_length("staging_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_xattr_staging_inode_id_width" CHECK (octet_length("inode_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_xattr_staging_name_bounds" CHECK (octet_length("name") BETWEEN 1 AND 1024),
        ADD CONSTRAINT "w9pt_fs_state_xattr_staging_name_no_nul" CHECK (position(E'\\x00'::bytea IN "name") = 0),
        ADD CONSTRAINT "w9pt_fs_state_xattr_staging_expected_size_range" CHECK (
            "expected_size" BETWEEN 0 AND 1048576
        ),
        ADD CONSTRAINT "w9pt_fs_state_xattr_staging_bytes_bound" CHECK (
            octet_length("staged_bytes") <= 1048576
        ),
        ADD CONSTRAINT "w9pt_fs_state_xattr_staging_progress_bound" CHECK (
            octet_length("staged_bytes") <= "expected_size"
        ),
        ADD CONSTRAINT "w9pt_fs_state_xattr_staging_record_revision_range" CHECK (
            "record_revision" BETWEEN 1 AND 18446744073709551615
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_mutation_results"
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_mutation_id_width" CHECK (octet_length("mutation_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_fingerprint_width" CHECK (octet_length("request_fingerprint") = 32),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_client_incarnation_width" CHECK (
            octet_length("client_incarnation_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_writer_scope_width" CHECK (octet_length("writer_scope_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_writer_incarnation_width" CHECK (
            octet_length("writer_incarnation_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_fencing_token_range" CHECK (
            "fencing_token" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_result_kind_range" CHECK ("result_kind" BETWEEN 1 AND 65535),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_result_format_range" CHECK (
            "result_format" BETWEEN 1 AND 65535
        ),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_result_bytes_bound" CHECK (
            octet_length("result_bytes") <= 8388608
        ),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_committed_revision_range" CHECK (
            "committed_revision" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_retention_range" CHECK (
            "retention_horizon" BETWEEN 0 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_record_revision_range" CHECK (
            "record_revision" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_mutation_results_revision_match" CHECK (
            "committed_revision" = "record_revision"
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_writer_fences"
        ADD CONSTRAINT "w9pt_fs_state_writer_fences_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_writer_fences_scope_id_width" CHECK (octet_length("writer_scope_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_writer_fences_token_range" CHECK (
            "greatest_fencing_token" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_fences_active_fields_all_or_none" CHECK (
            num_nonnulls(
                "active_holder_id",
                "active_lease_id",
                "active_deadline_tick",
                "active_record_revision"
            ) IN (0, 4)
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_fences_active_holder_width" CHECK (
            "active_holder_id" IS NULL OR octet_length("active_holder_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_fences_active_lease_width" CHECK (
            "active_lease_id" IS NULL OR octet_length("active_lease_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_fences_active_deadline_range" CHECK (
            "active_deadline_tick" IS NULL
            OR "active_deadline_tick" BETWEEN 0 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_fences_active_revision_range" CHECK (
            "active_record_revision" IS NULL
            OR "active_record_revision" BETWEEN 1 AND 18446744073709551615
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_writer_lease_operations"
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_filesystem_id_width" CHECK (
            octet_length("filesystem_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_operation_id_width" CHECK (
            octet_length("lease_operation_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_kind_tag" CHECK ("operation_kind" BETWEEN 1 AND 3),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_fingerprint_width" CHECK (
            octet_length("request_fingerprint") = 32
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_outcome_tag" CHECK (
            ("operation_kind" = 1 AND "outcome_tag" BETWEEN 1 AND 5)
            OR ("operation_kind" = 2 AND "outcome_tag" BETWEEN 1 AND 5)
            OR ("operation_kind" = 3 AND "outcome_tag" BETWEEN 1 AND 4)
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_grant_all_or_none" CHECK (
            num_nonnulls(
                "grant_scope_id",
                "grant_holder_id",
                "grant_lease_id",
                "grant_deadline_tick",
                "grant_fencing_token"
            ) IN (0, 5)
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_grant_shape" CHECK (
            (
                ("operation_kind" IN (1, 2) AND "outcome_tag" = 1)
                OR ("operation_kind" = 1 AND "outcome_tag" = 2)
            ) = ("grant_scope_id" IS NOT NULL)
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_scope_width" CHECK (
            "grant_scope_id" IS NULL OR octet_length("grant_scope_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_holder_width" CHECK (
            "grant_holder_id" IS NULL OR octet_length("grant_holder_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_lease_id_width" CHECK (
            "grant_lease_id" IS NULL OR octet_length("grant_lease_id") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_deadline_range" CHECK (
            "grant_deadline_tick" IS NULL
            OR "grant_deadline_tick" BETWEEN 0 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_writer_lease_operations_token_range" CHECK (
            "grant_fencing_token" IS NULL
            OR "grant_fencing_token" BETWEEN 1 AND 18446744073709551615
        );"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_change_commits"
        ADD CONSTRAINT "w9pt_fs_state_change_commits_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_change_commits_revision_range" CHECK (
            "revision" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_change_commits_origin_kind_tag" CHECK ("origin_kind" IN (1, 2)),
        ADD CONSTRAINT "w9pt_fs_state_change_commits_origin_id_width" CHECK (octet_length("origin_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_change_commits_key_count_range" CHECK ("key_count" BETWEEN 1 AND 16385);"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_change_keys"
        ADD CONSTRAINT "w9pt_fs_state_change_keys_filesystem_id_width" CHECK (octet_length("filesystem_id") = 16),
        ADD CONSTRAINT "w9pt_fs_state_change_keys_revision_range" CHECK (
            "revision" BETWEEN 1 AND 18446744073709551615
        ),
        ADD CONSTRAINT "w9pt_fs_state_change_keys_ordinal_range" CHECK ("ordinal" BETWEEN 0 AND 16384),
        ADD CONSTRAINT "w9pt_fs_state_change_keys_family_tag" CHECK ("family_tag" BETWEEN 1 AND 11),
        ADD CONSTRAINT "w9pt_fs_state_change_keys_component_a_width" CHECK (
            "component_a" IS NULL OR octet_length("component_a") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_change_keys_component_b_bound" CHECK (
            "component_b" IS NULL OR octet_length("component_b") BETWEEN 1 AND 1024
        ),
        ADD CONSTRAINT "w9pt_fs_state_change_keys_component_shape" CHECK (
            ("family_tag" = 1 AND "component_a" IS NULL AND "component_b" IS NULL)
            OR (
                "family_tag" IN (2, 4, 6, 9, 10, 11)
                AND "component_a" IS NOT NULL
                AND "component_b" IS NULL
            )
            OR (
                "family_tag" IN (3, 5, 7, 8)
                AND "component_a" IS NOT NULL
                AND "component_b" IS NOT NULL
            )
        ),
        ADD CONSTRAINT "w9pt_fs_state_change_keys_component_b_shape" CHECK (
            "component_b" IS NULL
            OR "family_tag" IN (3, 8)
            OR octet_length("component_b") = 16
        ),
        ADD CONSTRAINT "w9pt_fs_state_change_keys_name_shape" CHECK (
            "family_tag" NOT IN (3, 8)
            OR (
                position(E'\\x00'::bytea IN "component_b") = 0
                AND (
                    "family_tag" <> 3
                    OR (
                        position(E'\\x2f'::bytea IN "component_b") = 0
                        AND "component_b" <> E'\\x2e'::bytea
                        AND "component_b" <> E'\\x2e2e'::bytea
                    )
                )
            )
        );"#,
];

// SeaQuery 0.32 does not expose PostgreSQL's DEFERRABLE or INITIALLY DEFERRED
// clauses. Keep these narrow statements explicit so namespace and open-state
// changes retain their commit-time relationship checks.
const DEFERRED_FOREIGN_KEY_DDL: &[&str] = &[
    r#"ALTER TABLE "public"."w9pt_fs_state_filesystem_records"
        ADD CONSTRAINT "w9pt_fs_state_filesystem_records_root_inode_fk" FOREIGN KEY (
            "filesystem_id", "root_inode_id"
        ) REFERENCES "public"."w9pt_fs_state_inodes" ("filesystem_id", "inode_id")
        DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_inodes"
        ADD CONSTRAINT "w9pt_fs_state_inodes_directory_parent_fk" FOREIGN KEY (
            "filesystem_id", "directory_parent_inode_id"
        ) REFERENCES "public"."w9pt_fs_state_inodes" ("filesystem_id", "inode_id")
        DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_directory_entries"
        ADD CONSTRAINT "w9pt_fs_state_directory_entries_parent_fk" FOREIGN KEY (
            "filesystem_id", "parent_inode_id"
        ) REFERENCES "public"."w9pt_fs_state_inodes" ("filesystem_id", "inode_id")
        DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_directory_entries"
        ADD CONSTRAINT "w9pt_fs_state_directory_entries_child_fk" FOREIGN KEY (
            "filesystem_id", "child_inode_id"
        ) REFERENCES "public"."w9pt_fs_state_inodes" ("filesystem_id", "inode_id")
        DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_opens"
        ADD CONSTRAINT "w9pt_fs_state_opens_inode_fk" FOREIGN KEY (
            "filesystem_id", "inode_id"
        ) REFERENCES "public"."w9pt_fs_state_inodes" ("filesystem_id", "inode_id")
        DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_open_pins"
        ADD CONSTRAINT "w9pt_fs_state_open_pins_inode_fk" FOREIGN KEY (
            "filesystem_id", "inode_id"
        ) REFERENCES "public"."w9pt_fs_state_inodes" ("filesystem_id", "inode_id")
        DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_open_pins"
        ADD CONSTRAINT "w9pt_fs_state_open_pins_open_inode_fk" FOREIGN KEY (
            "filesystem_id", "inode_id", "open_id"
        ) REFERENCES "public"."w9pt_fs_state_opens" (
            "filesystem_id", "inode_id", "open_id"
        ) DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_orphans"
        ADD CONSTRAINT "w9pt_fs_state_orphans_inode_fk" FOREIGN KEY (
            "filesystem_id", "inode_id"
        ) REFERENCES "public"."w9pt_fs_state_inodes" ("filesystem_id", "inode_id")
        DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_locks"
        ADD CONSTRAINT "w9pt_fs_state_locks_inode_fk" FOREIGN KEY (
            "filesystem_id", "inode_id"
        ) REFERENCES "public"."w9pt_fs_state_inodes" ("filesystem_id", "inode_id")
        DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_locks"
        ADD CONSTRAINT "w9pt_fs_state_locks_owner_open_fk" FOREIGN KEY (
            "filesystem_id", "inode_id", "owner_open_id"
        ) REFERENCES "public"."w9pt_fs_state_opens" (
            "filesystem_id", "inode_id", "open_id"
        ) DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_xattrs"
        ADD CONSTRAINT "w9pt_fs_state_xattrs_inode_fk" FOREIGN KEY (
            "filesystem_id", "inode_id"
        ) REFERENCES "public"."w9pt_fs_state_inodes" ("filesystem_id", "inode_id")
        DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_xattr_staging"
        ADD CONSTRAINT "w9pt_fs_state_xattr_staging_inode_fk" FOREIGN KEY (
            "filesystem_id", "inode_id"
        ) REFERENCES "public"."w9pt_fs_state_inodes" ("filesystem_id", "inode_id")
        DEFERRABLE INITIALLY DEFERRED;"#,
    r#"ALTER TABLE "public"."w9pt_fs_state_change_keys"
        ADD CONSTRAINT "w9pt_fs_state_change_keys_commit_fk" FOREIGN KEY (
            "filesystem_id", "revision"
        ) REFERENCES "public"."w9pt_fs_state_change_commits" (
            "filesystem_id", "revision"
        ) DEFERRABLE INITIALLY DEFERRED;"#,
];
