# Delta for PostgreSQL State Adapter

## ADDED Requirements

### Requirement: Finalized State Contract Dependency
The system SHALL implement the finalized `w9pt-fs-state::FilesystemStateStore` contract without redefining its semantic records, outcomes, limits, or invariants.

#### Scenario: Prerequisite remains draft or incomplete
- WHEN implementation is requested before `add-filesystem-state-store` is approved and complete
- THEN the PostgreSQL adapter change remains blocked
- AND no provisional duplicate trait or schema is introduced

#### Scenario: State contract is finalized
- WHEN implementation begins after the prerequisite completes
- THEN every adapter type, SQL column, constraint, and conformance test is reconciled against the final public model
- AND incompatible assumptions are resolved before migration SQL is published

### Requirement: PostgreSQL Adapter Dependency Isolation
The system SHALL provide `w9pt-fs-state-postgres` as a runtime-specific adapter crate without adding PostgreSQL, SQLx, Tokio, or TLS dependencies to `w9pt-fs-state`, `w9pt-storage`, or `w9pt`.

#### Scenario: Adapter crate is compiled
- WHEN `w9pt-fs-state-postgres` is selected
- THEN it depends on `w9pt-fs-state` and SQLx exactly `0.8.6` with default features disabled
- AND it does not depend on `w9pt`, an object-store SDK, a testcontainer library, or deployment tooling

#### Scenario: Adapter is not selected
- WHEN an application uses another state adapter
- THEN PostgreSQL, SQLx, and Tokio dependencies are not pulled into the backend-neutral state or protocol crates
- AND the application retains its selected runtime/dependency boundary

### Requirement: Caller-Owned Pool and Connection Policy
The system SHALL accept a caller-created SQLx `PgPool` and SHALL not read credentials, connection URLs, TLS roots, pool sizes, or environment configuration implicitly.

#### Scenario: Adapter is opened
- WHEN a caller supplies a pool and checked adapter configuration
- THEN the adapter validates the pool target and schema and constructs a state-store implementation
- AND pool/runtime lifecycle remains caller-owned

#### Scenario: TLS is required
- WHEN a deployment connects over TLS
- THEN the embedding application enables and configures its compatible SQLx TLS feature and trust roots
- AND the adapter does not silently choose a TLS provider or certificate policy

#### Scenario: Migrations are pending
- WHEN `open` observes a missing or unsupported schema version
- THEN it fails with a typed configuration/migration error
- AND it does not apply DDL automatically

### Requirement: Supported PostgreSQL Versions and Primary Authority
The system SHALL support maintained PostgreSQL 15, 16, 17, and 18 minor releases using PostgreSQL 15-compatible SQL and SHALL execute authoritative operations only on a writable primary.

#### Scenario: Supported primary is used
- WHEN a transaction runs on PostgreSQL 15–18 and `pg_is_in_recovery()` is false
- THEN the adapter may execute the validated state operation
- AND it uses only SQL supported by PostgreSQL 15

#### Scenario: Standby or readonly route is selected
- WHEN any acquired connection reports recovery/readonly primary status
- THEN the state operation fails explicitly before authoritative results or mutations are returned
- AND the adapter does not serve stale replica state as linearizable

#### Scenario: Unsupported server version is used
- WHEN the server major version is outside 15–18
- THEN `open` fails with a typed unsupported-version error
- AND migrations or runtime writes are not attempted

### Requirement: Explicit Embedded Checksummed Migrations
The system SHALL use explicit embedded versioned migrations for a fixed schema and SHALL detect migration drift before serving state operations.

#### Scenario: Fresh database is migrated
- WHEN an authorized caller invokes `migrate`
- THEN the adapter serializes migration runners with the fixed migration advisory lock
- AND applies and records each embedded version/checksum transactionally

#### Scenario: Migration is invoked repeatedly
- WHEN all embedded migrations are already recorded with matching checksums
- THEN `migrate` reports no pending changes
- AND existing state remains unchanged

#### Scenario: Applied checksum or version differs
- WHEN the database contains checksum drift, a version gap, or an unknown newer migration
- THEN migration/open fails closed
- AND the adapter does not reinterpret the existing schema

### Requirement: Fixed Fully Qualified Normalized Schema
The system SHALL store each authoritative record family in normalized permanent logged tables under the fixed fully qualified schema `w9pt_fs_state_v1`.

#### Scenario: Runtime statement executes
- WHEN the adapter reads or mutates state
- THEN every table, index, constraint, and migration-ledger reference is fully qualified
- AND correctness does not depend on `search_path` or a configurable SQL identifier

#### Scenario: Semantic records are deleted
- WHEN a commit removes namespace, open, orphan, lock, xattr, mutation, lease, or change records
- THEN every deletion is explicit in the semantic change set
- AND cascading SQL behavior does not create unreported authoritative changes

#### Scenario: Direct malformed row is attempted
- WHEN SQL bypasses Rust validation and violates an ID, bound, enum, range, or relationship invariant
- THEN a named database constraint rejects the row
- AND no malformed authoritative record becomes readable

### Requirement: Lossless Portable Value Mapping
The system SHALL losslessly encode every public state value without locale-dependent ordering, signed narrowing, precision loss, or unchecked conversion.

#### Scenario: Full unsigned boundary is stored
- WHEN a public field contains `0`, `i64::MAX`, `i64::MAX + 1`, or `u64::MAX`
- THEN the adapter round-trips it through constrained `NUMERIC(20,0)` and checked canonical decimal conversion
- AND no unchecked signed cast is used

#### Scenario: Stable ID or digest is stored
- WHEN a fixed-width ID, fingerprint, or digest is written
- THEN PostgreSQL stores exact bytes under an `octet_length` constraint
- AND decoding rejects every incorrect width

#### Scenario: Namespace name is ordered
- WHEN directory or xattr names are compared or paginated
- THEN PostgreSQL uses their bounded byte representation and binary order
- AND database collation does not change uniqueness or ordering

### Requirement: Validated Primary-WAL Durability
The system SHALL advertise only primary-WAL durable metadata and SHALL validate the PostgreSQL settings and table properties required for that boundary.

#### Scenario: Durable adapter opens
- WHEN the server is primary, `fsync` and `full_page_writes` are enabled, state tables are logged, and transaction-local `synchronous_commit = on` can be enforced
- THEN the adapter advertises its primary-WAL durability contract
- AND successful mutation acknowledgment follows SQL `COMMIT`

#### Scenario: Durability setting is weaker
- WHEN required settings are disabled or runtime privileges cannot enforce the commit mode
- THEN `open` fails explicitly
- AND the deployment does not advertise `DurableMetadata` through this adapter

#### Scenario: Synchronous-standby durability is expected
- WHEN a deployment requires acknowledged data to survive primary loss through a synchronous standby
- THEN version 1 reports that guarantee as unsupported
- AND does not infer it from primary-WAL durability

### Requirement: One-Snapshot Serializable Read Batches
The system SHALL execute each state read batch through one primary-only `SERIALIZABLE READ ONLY` transaction and return one authoritative revision.

#### Scenario: Batch contains multiple queries
- WHEN one request reads related inode, namespace, open, lock, lease, or mutation records
- THEN all results come from the same PostgreSQL transaction snapshot
- AND they remain positionally associated with their queries

#### Scenario: Freshness floor is satisfied
- WHEN `AtLeast(R)` is requested and the filesystem revision is at least `R`
- THEN the adapter returns the snapshot and observed revision
- AND does not require a historical snapshot exactly equal to `R`

#### Scenario: Freshness floor or primary check fails
- WHEN the observed revision is below `R` or the connection is a standby
- THEN the adapter returns a typed non-authoritative/freshness result
- AND it does not return the batch as successful

### Requirement: Bounded Keyset State Scans
The system SHALL implement every ordered state scan with bounded keyset pagination rather than offset-based pagination.

#### Scenario: Directory page is requested
- WHEN a caller supplies a stable directory cookie/name cursor and item/byte bounds
- THEN the adapter returns the next entries ordered by `(cookie, name)`
- AND concurrent unrelated rows do not shift an offset cursor

#### Scenario: Other record page is requested
- WHEN locks, xattrs, opens, orphans, leases, or mutations are scanned
- THEN the adapter uses the record family's stable key suffix as the resume key
- AND enforces result bounds before returning the page

#### Scenario: Result would exceed a configured bound
- WHEN a query would materialize too many records or bytes
- THEN it returns a bounded page or typed limit error
- AND it does not first allocate the oversized result

### Requirement: Serializable Ledger-First Atomic Commits
The system SHALL map each semantic commit to one short primary-only `SERIALIZABLE READ WRITE` transaction with ledger-first replay and all-or-nothing record publication.

#### Scenario: New commit succeeds
- WHEN the mutation is absent, its fence is current, and all preconditions and invariants match
- THEN all normalized record changes, one filesystem revision, record revisions, exact terminal result, and one change event commit atomically
- AND acknowledgment occurs only after SQL `COMMIT` succeeds

#### Scenario: One semantic precondition fails
- WHEN any record absence/revision, generation, content base, link count, open pin, or fence differs
- THEN the SQL transaction rolls back without a mutation-result row
- AND the adapter returns the corresponding typed conflict/rejection

#### Scenario: Commit affects multiple record families
- WHEN create, rename, link/unlink, open-unlinked, lock, xattr, setattr, or content publication spans multiple tables
- THEN no observer can read a partial transition
- AND every changed row receives the same new record revision

### Requirement: Deterministic PostgreSQL Lock Ordering
The system SHALL sort and deduplicate affected semantic record keys and acquire PostgreSQL row/predicate protection in canonical order.

#### Scenario: Existing records are affected
- WHEN a commit changes multiple existing rows
- THEN their rows are locked in canonical `RecordKey` order after the filesystem revision/fence rows
- AND two adapters derive the same lock order from the same request

#### Scenario: Absent row is required
- WHEN a precondition requires a directory entry, inode, open, lock, xattr, or mutation row to be absent
- THEN the adapter performs the predicate read inside the serializable transaction
- AND named uniqueness constraints protect the insert race

#### Scenario: Duplicate affected key is submitted
- WHEN preflight finds duplicate or contradictory keys in one commit
- THEN it rejects the request before opening the SQL transaction
- AND it does not rely on database deadlock behavior to resolve malformed input

### Requirement: Per-Filesystem Revision Ordering
The system SHALL allocate checked monotonic revisions through a per-filesystem row so commits and change events have one authoritative order.

#### Scenario: Two commits target one filesystem
- WHEN both reach the publication phase concurrently
- THEN locking the filesystem row establishes one revision order
- AND each successful commit receives a distinct increasing revision

#### Scenario: Commits target different filesystems
- WHEN unrelated filesystem IDs commit concurrently
- THEN they do not contend on one global revision row
- AND each filesystem maintains its own ordering domain

#### Scenario: Revision would overflow
- WHEN incrementing the full-range numeric revision exceeds the state contract maximum
- THEN the transaction returns a typed exhaustion error and rolls back
- AND the revision never wraps or repeats

### Requirement: Exact Mutation Replay and Mismatch Rejection
The system SHALL read the mutation ledger before validating the current lease/fence and SHALL preserve exact idempotency across pools, processes, and expired writers.

#### Scenario: Matching mutation is retried
- GIVEN a committed mutation remains retained
- WHEN another pool submits the same mutation ID, fingerprint, client incarnation, changes, and result
- THEN the adapter returns exact `AlreadyCommitted`
- AND it does not reapply changes or reject the now-expired original fence

#### Scenario: Mutation identity differs
- WHEN the same retained mutation ID is submitted with a different fingerprint, client incarnation, change set, or result identity
- THEN the adapter returns a hard mismatch
- AND it does not return the old result or apply the new request

#### Scenario: Concurrent first inserts race
- WHEN two clients attempt the same new mutation concurrently
- THEN named mutation uniqueness and ledger reread resolve to one commit and one exact replay
- AND no raw SQL uniqueness error escapes as ambiguous filesystem behavior

### Requirement: Prepared ContentRef Publication Only
The system SHALL persist only state-validated `PreparedContent`/`ContentRef` fields with the inode and SHALL not store or publish bulk target content.

#### Scenario: Prepared content is valid
- GIVEN immutable payloads and the manifest were already acknowledged durable
- WHEN the state commit validates mutation, file identity, base, size, and generation
- THEN the inode's `ContentRef`, size, timestamps, and data revision commit together
- AND PostgreSQL performs no target-object operation

#### Scenario: Content preparation is inconsistent
- WHEN the prepared mutation, file ID, base reference, logical size, or generation disagrees with authoritative state
- THEN the serializable transaction rejects publication
- AND the old inode content remains current

#### Scenario: Per-block mapping is requested
- WHEN a deployment wants PostgreSQL-resident block or extent mappings
- THEN version 1 reports that model as outside this adapter contract
- AND requires a separate approved state/content-index proposal before schema changes

### Requirement: Exact SQLSTATE and Constraint Classification
The system SHALL classify PostgreSQL failures through SQLSTATE, transaction phase, and known named constraints without matching localized server messages.

#### Scenario: Serializable transaction aborts definitively
- WHEN PostgreSQL returns `40001` or `40P01`
- THEN the adapter may retry the identical transaction within its configured bound
- AND never changes the mutation identity, content, or semantic preconditions

#### Scenario: Known constraint rejects a race
- WHEN a named uniqueness, foreign-key, check, or numeric constraint rejects a request
- THEN the adapter maps only recognized constraint names to the documented semantic outcome
- AND unknown constraints remain adapter invariant errors

#### Scenario: Retry bound is exhausted
- WHEN definitive aborts repeat to the configured maximum
- THEN the adapter returns explicit retry exhaustion/conflict
- AND it does not loop indefinitely or silently weaken isolation

### Requirement: Ambiguous COMMIT Recovery
The system SHALL treat an uncertain SQL `COMMIT` response as potentially committed and resolve it only through bounded replay of the identical mutation request.

#### Scenario: Commit succeeded but response was lost
- WHEN a fresh primary transaction finds the matching mutation ledger record
- THEN the adapter returns the exact retained committed result
- AND it does not apply the state changes twice

#### Scenario: Commit did not occur
- WHEN exact recovery finds no retained mutation and the same request remains semantically valid
- THEN only that identical request may be attempted again
- AND no new mutation ID or blind rebase is generated

#### Scenario: Recovery cannot determine status
- WHEN fresh-primary connection/recovery attempts reach their configured bound
- THEN the adapter returns an explicit ambiguous-mutation error
- AND it does not claim rollback or success

### Requirement: Database-Time Leases and Monotonic Fencing
The system SHALL implement idempotent lease operations using PostgreSQL time and permanently retained monotonically increasing fence counters.

#### Scenario: Scope is newly acquired or taken over
- WHEN the scope is free or its prior lease expired according to database time
- THEN acquisition locks the scope row and allocates a token greater than all prior tokens
- AND records the exact lease-operation result for replay

#### Scenario: Lease is renewed or released
- WHEN the exact lease ID, holder, and token match
- THEN renew preserves the token while extending its deadline or release clears active fields
- AND neither operation resets/deletes the greatest fence counter

#### Scenario: Stale writer commits
- WHEN a non-replayed commit carries an expired, replaced, wrong-holder, or lower token
- THEN the adapter rejects the commit inside its serializable transaction
- AND the stale process cannot mutate newer state

### Requirement: Bounded Revision Change Polling
The system SHALL implement primary-only bounded keyset polling of whole committed revisions with explicit retained-history gaps.

#### Scenario: Events are available
- WHEN a caller polls after a retained revision
- THEN the adapter returns bounded event headers and all ordered changed keys for each included commit
- AND no event is split between pages

#### Scenario: More events remain
- WHEN the configured event count stops the page before the current revision
- THEN the adapter returns a stable resume revision and `has_more`
- AND the next query resumes by revision key rather than offset

#### Scenario: Cursor predates retained history
- WHEN the requested revision is older than the filesystem's oldest retained event
- THEN the adapter returns `RevisionCompacted`
- AND callers must rebuild caches from authoritative reads

### Requirement: PostgreSQL Adapter Conformance and Recovery
The system SHALL run offline adapter tests and the reusable live state-store conformance suite against current PostgreSQL 15–18 minor releases through independently created pools.

#### Scenario: Offline workspace tests run
- WHEN no PostgreSQL DSN is configured
- THEN numeric, codec, configuration, migration-checksum, key-order, and error-classification tests still run deterministically
- AND ordinary workspace tests do not provision or require a database

#### Scenario: Live test is required
- WHEN CI sets `W9PT_POSTGRES_TEST_REQUIRED=1` and a version-specific DSN
- THEN migrations, startup validation, conformance, concurrency, leases, ambiguity, change polling, and fresh-client recovery execute
- AND a missing DSN fails rather than silently skipping

#### Scenario: Independent clients reopen state
- WHEN one pool/store is discarded and another is constructed without shared local state
- THEN the second adapter reconstructs authoritative records, mutation results, fences, and revisions from PostgreSQL alone
- AND behavior matches the shared conformance reference

### Requirement: Explicit PostgreSQL Adapter Exclusions
The system SHALL keep content layout, session durability, replica reads, wake-up notifications, and deployment concerns outside the PostgreSQL state adapter.

#### Scenario: File data or block map is supplied
- WHEN a caller attempts to store payload bytes, S3 manifests, blocks, or extent maps through version 1
- THEN the adapter rejects or lacks that operation
- AND stores only the finalized state contract's metadata and `ContentRef` fields

#### Scenario: Session migration state is required
- WHEN fids, tags, flush dependencies, effects, or response outboxes must survive node loss
- THEN a separate session-state layer owns them
- AND PostgreSQL filesystem-state tables do not become an implicit session database

#### Scenario: Notification or deployment feature is requested
- WHEN a caller needs `LISTEN/NOTIFY`, replica reads, synchronous-standby durability, database provisioning, backups, failover orchestration, or monitoring
- THEN those features require separate deployment or approved adapter changes
- AND version 1 does not claim them as correctness guarantees
