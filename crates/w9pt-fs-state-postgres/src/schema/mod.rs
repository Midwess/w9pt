//! Code-first production schema definition and versioned migration steps.

#[allow(clippy::enum_variant_names)]
mod entities;
mod m20260904_182105_create_w9pt_fs_state;

use sea_orm::{DatabaseTransaction, DbErr};
use sea_orm_migration::{MigrationTrait, SchemaManager};

pub(crate) use m20260904_182105_create_w9pt_fs_state::TABLE_NAMES;
#[cfg(test)]
pub(crate) use m20260904_182105_create_w9pt_fs_state::{CONSTRAINT_NAMES, INDEX_NAMES};

pub(crate) const INITIAL_MIGRATION_VERSION: i32 = 1;
#[cfg(test)]
pub(crate) const INITIAL_MIGRATION_NAME: &str = "m20260904_182105_create_w9pt_fs_state";
pub(crate) const INITIAL_MIGRATION_SOURCE: &str =
    include_str!("m20260904_182105_create_w9pt_fs_state.rs");
pub(crate) const INITIAL_MIGRATION_SOURCES: &[&str] = &[
    INITIAL_MIGRATION_SOURCE,
    include_str!("entities/mod.rs"),
    include_str!("entities/w9pt_fs_state_authority_heads.rs"),
    include_str!("entities/w9pt_fs_state_change_commits.rs"),
    include_str!("entities/w9pt_fs_state_change_keys.rs"),
    include_str!("entities/w9pt_fs_state_directory_entries.rs"),
    include_str!("entities/w9pt_fs_state_filesystem_records.rs"),
    include_str!("entities/w9pt_fs_state_inodes.rs"),
    include_str!("entities/w9pt_fs_state_locks.rs"),
    include_str!("entities/w9pt_fs_state_mutation_results.rs"),
    include_str!("entities/w9pt_fs_state_open_pins.rs"),
    include_str!("entities/w9pt_fs_state_opens.rs"),
    include_str!("entities/w9pt_fs_state_orphans.rs"),
    include_str!("entities/w9pt_fs_state_schema_migrations.rs"),
    include_str!("entities/w9pt_fs_state_writer_fences.rs"),
    include_str!("entities/w9pt_fs_state_writer_lease_operations.rs"),
    include_str!("entities/w9pt_fs_state_xattr_staging.rs"),
    include_str!("entities/w9pt_fs_state_xattrs.rs"),
];

/// Ensures the adapter-owned three-column migration ledger in `public`.
pub(crate) async fn ensure_ledger(transaction: &DatabaseTransaction) -> Result<(), DbErr> {
    SchemaManager::new(transaction)
        .create_table(m20260904_182105_create_w9pt_fs_state::ledger_table())
        .await
}

/// Applies the initial code-first schema through the caller's transaction.
pub(crate) async fn apply_initial(transaction: &DatabaseTransaction) -> Result<(), DbErr> {
    let manager = SchemaManager::new(transaction);
    m20260904_182105_create_w9pt_fs_state::Migration
        .up(&manager)
        .await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use sea_orm_migration::MigrationName;

    use super::*;

    #[test]
    fn source_identity_matches_the_cli_scaffold() {
        let migration = m20260904_182105_create_w9pt_fs_state::Migration;
        assert_eq!(migration.name(), INITIAL_MIGRATION_NAME);
        assert!(INITIAL_MIGRATION_SOURCE.contains("impl MigrationTrait for Migration"));
        assert_eq!(INITIAL_MIGRATION_VERSION, 1);
    }

    #[test]
    fn generated_catalog_names_are_complete_unique_and_postgres_bounded() {
        assert_eq!(TABLE_NAMES.len(), 16);
        assert_eq!(INDEX_NAMES.len(), 23);
        assert_eq!(CONSTRAINT_NAMES.len(), 172);
        for names in [TABLE_NAMES, INDEX_NAMES, CONSTRAINT_NAMES] {
            assert!(names.iter().all(|name| name.starts_with("w9pt_fs_state_")));
            assert!(names.iter().all(|name| name.len() <= 63));
            assert_eq!(
                names.iter().copied().collect::<BTreeSet<_>>().len(),
                names.len()
            );
        }
        assert_eq!(
            INITIAL_MIGRATION_SOURCE.matches("ADD CONSTRAINT").count(),
            153
        );
        assert_eq!(
            INITIAL_MIGRATION_SOURCE
                .matches("DEFERRABLE INITIALLY DEFERRED")
                .count(),
            13
        );
    }
}
