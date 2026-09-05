# Analysis: Public Code-First PostgreSQL Schema

## Existing Source

`0001_initial.sql` defines 16 permanent tables, 127 columns, 16 primary keys, two named unique constraints, 12 named deferred foreign keys, 136 named CHECK constraints, and four explicit non-unique indexes under `w9pt_fs_state_v1`.

The adapter's runtime statements are deliberately PostgreSQL-specific and fully qualified. Moving to `public` requires changing every production qualifier, catalog validation, cleanup fixture, and schema expectation together. Generic names such as `public.inodes` and `public.locks` are unsafe in a shared namespace, so transferred relations use the fixed `w9pt_fs_state_` prefix without a format-version suffix.

## CLI Capability

Exact SeaORM CLI 1.1.20 provides:

- `migrate generate`, which creates an empty `MigrationTrait` scaffold;
- `generate entity`, which introspects an existing database into entity definitions.

It does not synthesize a code-first migration from an existing database. Generated entities capture useful table/column/key/relationship evidence but do not preserve the complete named-check and deferred-FK contract by themselves.

## SeaQuery Gaps

SeaQuery 0.32 expresses table/column definitions, schema-qualified identifiers, primary and unique indexes, ordinary foreign keys, and explicit indexes. It does not faithfully model named CHECK constraints or PostgreSQL `DEFERRABLE INITIALLY DEFERRED` foreign keys. Those definitions must remain explicit PostgreSQL DDL executed from the Rust migration.

## Migration Integration

The stock SeaORM migrator is unsuitable because it owns its ledger and transaction. The adapter must retain:

1. bounded `READ COMMITTED READ WRITE` transaction;
2. fixed transaction advisory lock;
3. bounded custom integer/checksum ledger;
4. code-first `MigrationTrait::up` calls through `SchemaManager`;
5. ledger insertion and one explicit commit.

The immutable checksum input becomes the checked-in Rust migration source rather than a deleted SQL file.

## Compatibility Boundary

Repository history contains no committed adapter schema, so the requested layout directly replaces the uncommitted initial format. No legacy detection, import, relocation, or dual-schema behavior remains in the product.
