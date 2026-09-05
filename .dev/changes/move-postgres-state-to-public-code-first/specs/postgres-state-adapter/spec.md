# Delta for PostgreSQL State Adapter

## ADDED Requirements

### Requirement: Code-First Schema Transfer Evidence
The system SHALL derive the initial Rust migration from the existing reviewed PostgreSQL schema using exact SeaORM CLI 1.1.20 scaffolding and database-introspection evidence.

#### Scenario: Existing schema is transferred
- WHEN maintainers regenerate transfer evidence
- THEN the current SQL schema is recreated in a disposable database
- AND exact SeaORM CLI 1.1.20 generates an entity snapshot and empty migration scaffold
- AND normalized source and code-first target catalogs match for every correctness-bearing object

#### Scenario: SeaQuery cannot express an existing feature
- WHEN named CHECK constraints or deferred foreign keys are transferred
- THEN the Rust migration uses a narrowly scoped reviewed PostgreSQL DDL escape hatch
- AND exact constraint names, expressions, and deferrability are preserved

## MODIFIED Requirements

### Requirement: SeaORM Raw Statement Authority
The system SHALL use SeaORM for runtime PostgreSQL statements and SeaORM Migration/SeaQuery code for production schema construction while retaining explicit PostgreSQL semantics wherever generic builders are insufficient.

#### Scenario: Correctness-bearing runtime SQL executes
- WHEN the adapter performs locks, predicate reads, numeric casts, bounded scans, catalog checks, clocks, ledger operations, or revision publication
- THEN it executes reviewed parameterized PostgreSQL statements through SeaORM
- AND all production objects are fully qualified in `public`

#### Scenario: Production schema initializes
- WHEN a fresh authorized database is migrated
- THEN a CLI-scaffolded `MigrationTrait` creates the schema through `SchemaManager`
- AND runtime entities or ActiveModels do not replace portable state records or semantic SQL

### Requirement: Explicit Embedded Checksummed Migrations
The system SHALL execute checked-in Rust `MigrationTrait` implementations inside the bounded adapter-owned advisory-locked transaction and SHALL checksum the immutable Rust migration source in its custom ledger.

#### Scenario: Fresh database is migrated
- WHEN an authorized caller invokes `migrate` with a PostgreSQL `DatabaseConnection`
- THEN the adapter creates its custom ledger and all initial production objects in `public`
- AND migration construction and ledger insertion commit atomically

#### Scenario: Migration is invoked repeatedly or concurrently
- WHEN all Rust migrations are recorded with matching source checksums or multiple runners start together
- THEN the transaction advisory lock serializes bounded ledger validation
- AND repeated runners report no pending changes without altering existing state

#### Scenario: Applied checksum or version differs
- WHEN the database contains source-checksum drift, a version gap, or an unknown newer migration
- THEN migration and `open` fail closed
- AND the adapter does not reinterpret existing public tables

### Requirement: Normalized PostgreSQL Record Tables
The system SHALL store each finalized authoritative record family in normalized permanent logged tables under PostgreSQL's `public` schema with the fixed `w9pt_fs_state_` relation prefix while keeping adapter-private coordination rows distinct.

#### Scenario: Fresh code-first schema is inspected
- WHEN catalog parity validation runs
- THEN all table names, columns, PostgreSQL types, defaults, primary keys, unique constraints, deferred foreign keys, named checks, and indexes match the transferred schema
- AND no production object depends on `search_path`

#### Scenario: Runtime statement addresses state
- WHEN the adapter reads or mutates an authoritative record
- THEN the SQL explicitly qualifies the target under `public`
- AND no visible filesystem path or content payload is stored as a database object name

## REMOVED Requirements

### Requirement: Existing Version-One Database Compatibility
Reason: The adapter schema has not been committed or released, and the user explicitly requested a direct cut to the public code-first initial format with no legacy behavior.
