# Delta for PostgreSQL State Adapter

## ADDED Requirements

### Requirement: SeaORM Compatibility and Feasibility Gate
The system SHALL use exact SeaORM 1.1.20 under the Rust 1.85 baseline and SHALL prove that SeaORM preserves every correctness-bearing PostgreSQL behavior before removing the direct SQLx implementation.

#### Scenario: Compatible SeaORM version is selected
- WHEN the adapter dependency graph is resolved
- THEN SeaORM is pinned exactly to 1.1.20 with default features disabled
- AND only the required PostgreSQL and Tokio runtime features are enabled
- AND the graph compiles under Rust 1.85

#### Scenario: SeaORM 2 is considered
- WHEN a dependency update selects SeaORM 2.x or SQLx 0.9
- THEN the change is rejected under the current workspace MSRV
- AND a separate MSRV and driver-upgrade proposal is required

#### Scenario: Native database behavior cannot be preserved
- WHEN SeaORM cannot expose exact SQLSTATE, named constraints, explicit commit-phase errors, serializable access modes, bounded row behavior, or required bind shapes without a direct SQLx dependency
- THEN implementation stops and the proposal returns to draft
- AND no state-store guarantee or semantic outcome is weakened

#### Scenario: Literal SQLx-free graph is required
- WHEN "no SQLx" is interpreted to forbid SeaORM's transitive SQLx PostgreSQL driver
- THEN this proposal is infeasible
- AND no implementation claims a SQLx-free dependency graph

### Requirement: SeaORM Raw Statement Authority
The system SHALL use SeaORM as an execution facade while retaining fixed, fully qualified, parameterized PostgreSQL statements as the normative authority implementation.

#### Scenario: Correctness-bearing SQL executes
- WHEN the adapter performs locks, predicate reads, numeric casts, bounded scans, catalog checks, clocks, ledger operations, or revision publication
- THEN it executes the reviewed PostgreSQL statement through SeaORM `Statement` and `ConnectionTrait`
- AND it does not substitute ORM-generated SQL with different semantics

#### Scenario: Entity generation is proposed
- WHEN an entity, ActiveModel, schema-sync, or generated migration would duplicate portable state records or reinterpret the fixed schema
- THEN the feature is excluded from this change
- AND the existing row/key/numeric codecs remain authoritative

## MODIFIED Requirements

### Requirement: PostgreSQL Adapter Dependency Isolation
The system SHALL provide `w9pt-fs-state-postgres` as a SeaORM-backed runtime-specific adapter without adding SeaORM, SQLx, Tokio, or TLS dependencies to `w9pt-fs-state`, `w9pt-fs-storage`, or `w9pt`, and SHALL contain no direct SQLx dependency or API in the adapter crate.

#### Scenario: Adapter crate is compiled
- WHEN `w9pt-fs-state-postgres` is selected
- THEN it depends directly on `w9pt-fs-state`, `w9pt-fs-storage`, and SeaORM exactly 1.1.20
- AND it has no direct normal or development dependency on SQLx
- AND adapter production code, test code, examples, and public APIs contain no `sqlx::` path or directly imported SQLx type

#### Scenario: Transitive dependencies are inspected
- WHEN the complete dependency tree is inspected
- THEN SQLx may appear only beneath SeaORM's PostgreSQL backend
- AND documentation states that direct SQLx isolation does not mean a SQLx-free resolved graph

#### Scenario: Adapter is not selected
- WHEN an application uses another state adapter
- THEN PostgreSQL, SeaORM, SQLx, and Tokio dependencies are not pulled into backend-neutral state, storage, or protocol crates
- AND the application retains its chosen runtime boundary

### Requirement: Caller-Owned Pool and Connection Policy
The system SHALL accept a caller-created SeaORM `DatabaseConnection` and SHALL not read credentials, connection URLs, TLS roots, pool sizes, or environment configuration implicitly.

#### Scenario: Adapter is opened
- WHEN a caller supplies a connected PostgreSQL `DatabaseConnection` and checked adapter configuration
- THEN the adapter validates the backend, target, schema, clock mode, and durability boundary
- AND connection, pool, and runtime lifecycle remain caller-owned

#### Scenario: Non-PostgreSQL or disconnected connection is supplied
- WHEN `open` receives another SeaORM backend or a disconnected connection
- THEN it fails explicitly before authoritative work
- AND it does not panic or reinterpret another database as PostgreSQL

#### Scenario: TLS is required
- WHEN a deployment connects over TLS
- THEN the embedding application enables and configures the compatible SeaORM runtime/TLS feature and trust roots
- AND the adapter does not silently choose a TLS provider or certificate policy

#### Scenario: Existing SQLx pool must be reused
- WHEN a caller already owns a SQLx PostgreSQL pool
- THEN any conversion to SeaORM occurs outside `w9pt-fs-state-postgres`
- AND the adapter public API still names only `DatabaseConnection`

#### Scenario: Migrations are pending
- WHEN `open` observes a missing or unsupported schema version
- THEN it fails with a typed configuration or migration error
- AND it does not apply DDL automatically

### Requirement: Explicit Embedded Checksummed Migrations
The system SHALL use explicit embedded versioned migrations for the fixed production schema, SHALL preserve existing migration bytes and checksums, and SHALL serialize SeaORM migration runners transactionally.

#### Scenario: Fresh database is migrated
- WHEN an authorized caller invokes `migrate` with a PostgreSQL `DatabaseConnection`
- THEN the adapter begins one explicit read-write transaction and acquires the fixed `pg_advisory_xact_lock`
- AND it bootstraps, applies, and records all pending transaction-safe embedded migrations before one commit

#### Scenario: Migration is invoked repeatedly or concurrently
- WHEN all embedded migrations are recorded with matching checksums or multiple runners start together
- THEN the transaction advisory lock serializes ledger validation
- AND repeated runners report no pending changes without altering existing state

#### Scenario: Applied checksum or version differs
- WHEN the database contains checksum drift, a version gap, or an unknown newer migration
- THEN migration and `open` fail closed
- AND the adapter does not reinterpret the existing schema

#### Scenario: Existing version-one database is opened
- WHEN a database was migrated by the direct-SQLx adapter
- THEN the SeaORM adapter accepts the unchanged schema and checksum ledger
- AND no DDL or data conversion is required

#### Scenario: Future migration is nontransactional
- WHEN an embedded migration cannot execute inside the outer PostgreSQL transaction
- THEN that migration is rejected for this runner design
- AND a separate approved migration-protocol change is required

### Requirement: One-Snapshot Serializable Read Batches
The system SHALL execute each state read batch through one primary-only SeaORM `DatabaseTransaction` configured `SERIALIZABLE READ ONLY` and return exact finalized read outcomes.

#### Scenario: Batch contains multiple queries
- WHEN one request reads related records or scans
- THEN all SeaORM statements execute against the same explicit transaction snapshot and authoritative revision
- AND results remain positionally associated with their queries

#### Scenario: Freshness floor is satisfied
- WHEN `AtLeast(R)` is requested and current revision is at least `R`
- THEN the adapter returns `ReadOutcome::Snapshot`
- AND it need not return a historical snapshot exactly equal to `R`

#### Scenario: Freshness floor is not satisfied
- WHEN current revision is below `R`
- THEN the adapter returns `ReadOutcome::RevisionUnavailable`
- AND it does not return partial batch data

#### Scenario: Receiving adapter has tighter limits
- WHEN a request was constructed under limits looser than the adapter contract
- THEN the adapter returns `ReadOutcome::MalformedRequest`
- AND no unbounded SeaORM query is executed

#### Scenario: Bounded scan is executed
- WHEN a scan contains large variable values
- THEN SeaORM first executes the existing metadata-only retained-size query
- AND full values are fetched only for rows selected within item and byte bounds

### Requirement: Serializable Ledger-First Atomic Commits
The system SHALL follow the finalized ledger-first protocol and map each new semantic commit to one explicit primary-only SeaORM `DatabaseTransaction` configured `SERIALIZABLE READ WRITE`.

#### Scenario: Retained mutation is submitted under tighter current limits
- WHEN the short primary ledger probe finds a retained mutation before adapter-limit or fence validation
- THEN the adapter decodes it against immutable schema bounds and classifies it only through `MutationContext::classify_record`
- AND it returns the exact replay or mismatch without rejecting changed current limits or lease state

#### Scenario: Ledger probe is absent
- WHEN no retained mutation exists
- THEN the adapter validates the complete request against its contract limits
- AND repeats ledger lookup first inside every SeaORM write attempt before current-state validation

#### Scenario: New commit succeeds
- WHEN the mutation is absent, its fence is current, and all preconditions and invariants match
- THEN all public changes, one private authority revision, record revisions, exact terminal result, and one whole change event commit atomically
- AND acknowledgment occurs only after explicit `DatabaseTransaction::commit` succeeds

#### Scenario: Semantic validation fails
- WHEN any typed precondition or targeted cross-record invariant fails
- THEN the SeaORM transaction rolls back without a mutation-result row
- AND the adapter returns the exact finalized semantic conflict or malformed outcome

#### Scenario: Native transaction aborts
- WHEN a SeaORM statement or commit exposes `40001`, `40P01`, or a recognized named constraint race
- THEN the adapter preserves the native classification through its retry layer
- AND retries only the identical request within configured bounds

### Requirement: Exact SQLSTATE and Constraint Classification
The system SHALL classify PostgreSQL failures through adapter phase plus exact SQLSTATE and known named constraints obtained from SeaORM's public runtime-error surface, without direct SQLx imports or localized message matching.

#### Scenario: Serializable transaction aborts definitively
- WHEN PostgreSQL returns `40001` or `40P01` through a SeaORM error
- THEN the adapter may retry the identical request within its configured bound
- AND never changes mutation identity, content, result, or semantic preconditions

#### Scenario: Known constraint rejects a race
- WHEN a named uniqueness, foreign-key, check, or numeric constraint is exposed through SeaORM
- THEN only recognized constraint names map to documented outcomes
- AND unknown constraints remain adapter invariant failures

#### Scenario: Commit returns an unknown driver error
- WHEN explicit `DatabaseTransaction::commit` fails without proving transaction abort
- THEN the adapter treats status as potentially committed
- AND resolves only through fresh exact ledger replay

#### Scenario: SeaORM exposes only message text
- WHEN exact code or constraint fields cannot be obtained from the supported public SeaORM API
- THEN implementation stops
- AND it does not parse, match, or depend on localized error strings

#### Scenario: Retry bound is exhausted
- WHEN definitive aborts repeat to the configured maximum
- THEN the adapter returns the specified infrastructure or semantic result
- AND it does not loop indefinitely or weaken isolation

### Requirement: PostgreSQL Adapter Conformance and Recovery
The system SHALL run offline tests and the reusable live state-store conformance suite through independently constructed SeaORM connections against current PostgreSQL 15–18 minor releases.

#### Scenario: Offline workspace tests run
- WHEN no PostgreSQL DSN is configured
- THEN SeaORM gateway, numeric, codec, cursor, configuration, migration-checksum, key-order, clock-conversion, and error-classification tests run deterministically
- AND ordinary workspace tests do not provision a database

#### Scenario: Live test is required
- WHEN CI sets `W9PT_POSTGRES_TEST_REQUIRED=1` with version-specific DSNs
- THEN migrations, startup validation, conformance, concurrency, leases, ambiguity, change polling, and fresh-client recovery execute through SeaORM
- AND a missing required DSN fails rather than silently skipping

#### Scenario: Independent clients are opened
- WHEN conformance creates writer, observer, control, or isolated-limit clients
- THEN each required authority client is constructed from an independent SeaORM connection/pool
- AND no correctness property depends on a shared adapter or connection cache

#### Scenario: Direct SQLx isolation is checked
- WHEN dependency and source checks run
- THEN SQLx is absent from the adapter's depth-one dependency tree and source/test paths
- AND the expected transitive SQLx edge beneath SeaORM is documented

#### Scenario: Baseline compatibility is checked
- WHEN the SeaORM adapter targets a database created by the direct-SQLx version
- THEN all records, revisions, mutation results, leases, changes, and migration checksums round-trip unchanged
- AND PostgreSQL 15–18 behavior matches the existing conformance baseline

### Requirement: Explicit PostgreSQL Adapter Exclusions
The system SHALL keep content layout, session durability, replica reads, wake-up notifications, deployment concerns, ORM-generated authority semantics, and direct SQLx APIs outside the SeaORM-backed PostgreSQL state adapter.

#### Scenario: File data or block map is supplied
- WHEN a caller attempts to store payload bytes, S3 manifests, blocks, or extent maps through version 1
- THEN the adapter lacks that operation
- AND stores only finalized metadata and inode `ContentRef` fields

#### Scenario: Session migration state is required
- WHEN fids, tags, flush dependencies, effects, or response outboxes must survive node loss
- THEN a separate session-state layer owns them
- AND filesystem-state tables do not become an implicit session database

#### Scenario: ORM entity or schema generation is requested
- WHEN a caller requests SeaORM entities, ActiveModels, schema sync, or generated migrations for authoritative tables
- THEN those features remain outside this change
- AND the fixed schema plus portable state codecs remain normative

#### Scenario: Notification or deployment feature is requested
- WHEN a caller needs `LISTEN/NOTIFY`, replica reads, synchronous-standby durability, provisioning, backups, failover orchestration, or monitoring
- THEN those features require separate deployment work or approved changes
- AND version 1 does not claim them as correctness guarantees
