# Design: Public Code-First PostgreSQL Schema

## Layout

```text
w9pt-fs-state-postgres
  src/schema/
    mod.rs
    entities/                                  # exact database-first CLI output
    m20260904_182105_create_w9pt_fs_state.rs   # exact CLI scaffold, completed code-first
  src/migration.rs     # bounded custom runner and checksum ledger
```

The versioned schema step is generated with SeaORM CLI 1.1.20 and completed around expanded entities generated from the reviewed database. Those entities drive code-first table columns through SeaORM `Schema` metadata. The runtime crate depends on exact SeaORM and `sea-orm-migration` 1.1.20 without CLI/default features; the ORM enables only its PostgreSQL/Tokio and generated-type features.

## Identifiers

Production objects use schema-qualified `(Alias::new("public"), Iden)` table references. Runtime SQL remains explicitly qualified with `"public"`. Columns and semantic constraint names remain unchanged; tables and schema-global indexes use the fixed collision-resistant `w9pt_fs_state_` prefix without `v1`.

## Creation Order

1. Ensure the adapter checksum ledger in `public`.
2. Create authority and record tables from generated entity column/key metadata without emitting the entity relationships.
3. Create supporting unique and non-unique indexes.
4. Add named CHECK constraints through reviewed PostgreSQL DDL.
5. Add named deferred foreign keys through reviewed PostgreSQL DDL.
6. Record the checksum of the migration wrapper plus every generated entity source and commit once.

## Catalog Parity

Tests compare the reviewed database and code-first target for table persistence, columns, PostgreSQL types, nullability, defaults, primary/unique keys, constraint names/types/deferrability, and index definitions. A second exact SeaORM entity-generation pass against the code-created database must reproduce the checked-in entity tree byte-for-byte. Runtime conformance then proves semantic parity on PostgreSQL 15–18.

## Direct Cut

Only the public prefixed code-first layout is supported. The private schema and its checksum are removed without a compatibility path because the adapter format has not been released from this repository.
