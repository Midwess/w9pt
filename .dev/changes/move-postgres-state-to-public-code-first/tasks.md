# Tasks: Move PostgreSQL State Schema to Public Code-First Migrations

## Progress: [22/22]

### 1. Transfer evidence and scaffold

- [x] 1.1 Recreate the current SQL schema in disposable PostgreSQL and capture normalized catalog inventory.
- [x] 1.2 Run exact SeaORM CLI 1.1.20 entity generation against the reviewed final schema and check in its expanded output.
- [x] 1.3 Run exact SeaORM CLI 1.1.20 `migrate generate` and retain the generated migration scaffold.
- [x] 1.4 Record unsupported SeaQuery constructs and the allowed PostgreSQL DDL escape hatches.

### 2. Code-first migration integration

- [x] 2.1 Add exact `sea-orm-migration = 1.1.20` with minimal PostgreSQL/Tokio features.
- [x] 2.2 Add the generated `MigrationTrait` module and checksum the wrapper plus all generated entity sources.
- [x] 2.3 Convert the custom ledger bootstrap to code-first `SchemaManager` construction in `public`.
- [x] 2.4 Invoke code-first migrations inside the existing bounded advisory-locked transaction without `seaql_migrations`.
- [x] 2.5 Reject reserved-prefix public-name collisions before creating authority tables.

### 3. Schema transfer

- [x] 3.1 Transfer all 16 tables and 130 columns with exact PostgreSQL types, nullability, and defaults.
- [x] 3.2 Transfer all primary keys, named unique constraints, and four explicit indexes.
- [x] 3.3 Transfer all named CHECK constraints without changing SQLSTATE constraint identities.
- [x] 3.4 Transfer all deferred foreign keys with exact names and deferrability.
- [x] 3.5 Remove the handwritten initial SQL after source/target catalog parity passes.

### 4. Runtime and validation

- [x] 4.1 Change all production runtime statements and generated statement helpers to the `public` schema.
- [x] 4.2 Change catalog, privilege, persistence, migration-ledger, test cleanup, and documentation expectations to `public`.
- [x] 4.3 Preserve exact bounded reads, commits, leases, retries, ambiguity, fencing, and change polling.

### 5. Verification

- [x] 5.1 Add normalized source-versus-code-first catalog parity tests and prove entity regeneration is byte-identical.
- [x] 5.2 Run fresh/repeat/concurrent/drift/rollback/collision migration regressions.
- [x] 5.3 Run the full single-DSN suite and required PostgreSQL 15–18 conformance matrix.
- [x] 5.4 Run Rust 1.85 locked workspace tests, formatting, Clippy/rustdoc with warnings denied, and dependency/source enforcement.
- [x] 5.5 Update README and `.dev/project.md`; record the CLI command, transfer evidence, escape hatches, and final progress.

## Notes

- Final PostgreSQL inventory: 16 tables, 130 columns, 16 primary keys, three named unique constraints, 13 `DEFERRABLE INITIALLY DEFERRED` foreign keys, 140 named CHECK constraints, and four explicit indexes.
- Exact SeaORM CLI 1.1.20 generated 16 expanded entity files from `public` and scaffolded `m20260904_182105_create_w9pt_fs_state.rs` with `--universal-time`.
- The CLI scaffold is empty by design. Checked-in entities drive table and column definitions; SeaQuery 0.32 lacks faithful named-CHECK and deferred-FK builders, so reviewed PostgreSQL constraint DDL in Rust remains required.
- Entity reproduction uses exact CLI 1.1.20: `sea-orm-cli generate entity --database-schema public --expanded-format --with-prelude none --with-serde none --output-dir <dir> --database-url <dsn>`. Regenerating from the code-created database is byte-identical to `src/schema/entities/`.
- Normalized final catalog MD5 evidence is columns `7f3d74bc3cd8d0767905fc84e79e048f`, constraints `a494e9a593ea21357ccb274387a60f84`, and indexes `2d7113a6c7bf648a4c7d8ac0f69151e9`; the public code-first target matches all three.
- Earlier development databases are unsupported and must be reset explicitly; no automatic reset, import, relocation, or dual-schema code exists.
