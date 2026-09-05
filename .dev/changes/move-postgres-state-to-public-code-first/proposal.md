# Move PostgreSQL State Schema to Public Code-First Migrations

Status: approved

## Summary

Replace the handwritten `0001_initial.sql` production bootstrap with a SeaORM 1.1.20 CLI-scaffolded Rust migration and move the production tables to PostgreSQL's `public` schema. Exact SeaORM CLI entity generation from the reviewed final database supplies the checked-in table, column, type, nullability, key, and relation metadata used by the initial migration. The checked-in generated entities, migration wrapper, and PostgreSQL semantic overlays are the authority for fresh installations.

This supersedes the completed SeaORM-switch decision that froze the private schema and SQL migration bytes. The repository has not committed or released the adapter schema, so this is a direct format cut with no legacy compatibility path.

## Goals

- Use exact SeaORM CLI 1.1.20 to scaffold the migration and check in entities generated from the reviewed final database.
- Keep Rust 1.85 and the direct-SQLx isolation boundary.
- Create collision-resistant `public.w9pt_fs_state_<table>` production tables with unchanged column types, defaults, keys, named constraints, indexes, durability, and semantic bounds.
- Preserve the adapter-owned bounded migration transaction, advisory lock, integer version/checksum ledger, and transactional publication order.
- Make the Rust migration and generated entity sources the checksummed migration identity and remove `0001_initial.sql` after parity is proven.
- Keep all runtime SQL fully qualified as `public.<table>`.

## Non-Goals

- Preserving, detecting, importing, or relocating any legacy private-schema database.
- Using stock `MigratorTrait::up`, `seaql_migrations`, host-time migration bookkeeping, schema sync, or ActiveModels for runtime authority.
- Weakening named-constraint SQLSTATE handling because SeaQuery lacks a direct builder.
- Making the PostgreSQL-specific adapter portable to another database.

## Key Decision

The CLI generates an empty migration scaffold rather than a database-to-migration conversion. Its database-first expanded entities are checked in and drive table columns through SeaORM `Schema` metadata. The migration intentionally does not emit generated entity relationships directly because that would lose stable names and deferred behavior. SeaQuery overlays preserve defaults, primary/unique keys, and indexes; narrow reviewed PostgreSQL DDL statements preserve named CHECK constraints and `DEFERRABLE INITIALLY DEFERRED` foreign keys that SeaQuery 0.32 cannot model faithfully.

## Compatibility

- Fresh databases create the transferred layout in `public` under the fixed `w9pt_fs_state_` relation prefix.
- The old private schema, SQL migration, checksum, and database layout are unsupported after this direct cut.
- Record encodings, revision semantics, fencing, mutation replay, and content references do not change.

## Risks

| Risk | Mitigation |
|---|---|
| Public table names collide with application tables | Reserve the `w9pt_fs_state_` prefix, validate exact catalog ownership/shape, and fail before DDL or runtime work |
| Generated metadata or code-first overlays silently change PostgreSQL types or names | Re-generate entities from the code-created database, compare them byte-for-byte, compare normalized catalog snapshots, and run every named-constraint regression |
| Stock SeaORM migrator weakens custom ledger semantics | Invoke each `MigrationTrait` directly inside the adapter-owned transaction |
| Unsupported SeaQuery features lose names/deferrability | Retain minimal reviewed PostgreSQL DDL escape hatches in Rust migration source |
