# Delta for PostgreSQL State Adapter

## ADDED Requirements

### Requirement: Finalized State Contract Dependency
The system SHALL implement the approved `w9pt-fs-state::FilesystemStateStore` contract without redefining its semantic records, outcomes, limits, ordering, or invariants.

#### Scenario: Reconciled implementation begins
- GIVEN `add-filesystem-state-store` is approved and complete
- WHEN PostgreSQL adapter implementation begins
- THEN every adapter type, SQL column, constraint, cursor, outcome, and conformance test follows the finalized public model
- AND no provisional duplicate state contract is introduced

#### Scenario: Future contract drift is detected
- WHEN the finalized public state model changes incompatibly before the adapter is complete
- THEN implementation pauses for an explicit reconciliation
- AND published migration SQL is not allowed to silently reinterpret the changed model

### Requirement: PostgreSQL Adapter Dependency Isolation
The system SHALL provide `w9pt-fs-state-postgres` as a runtime-specific adapter crate without adding PostgreSQL, SQLx, Tokio, or TLS dependencies to `w9pt-fs-state`, `w9pt-fs-storage`, or `w9pt`.

#### Scenario: Adapter crate is compiled
- WHEN `w9pt-fs-state-postgres` is selected
- THEN it depends on `w9pt-fs-state` and SQLx exactly `0.8.6` with default features disabled
- AND it does not depend on `w9pt`, an object-store SDK, testcontainers, or deployment tooling

#### Scenario: Adapter is not selected
- WHEN an application uses another state adapter
- THEN PostgreSQL, SQLx, and Tokio dependencies are not pulled into the backend-neutral state or protocol crates
- AND the application retains its chosen runtime boundary

### Requirement: Caller-Owned Pool and Connection Policy
The system SHALL accept a caller-created SQLx `PgPool` and SHALL not read credentials, connection URLs, TLS roots, pool sizes, or environment configuration implicitly.

#### Scenario: Adapter is opened
- WHEN a caller supplies a pool and checked adapter configuration
- THEN the adapter validates the target, schema, clock mode, and durability boundary
- AND pool/runtime lifecycle remains caller-owned

#### Scenario: TLS is required
- WHEN a deployment connects over TLS
- THEN the embedding application enables and configures its compatible SQLx TLS feature and trust roots
- AND the adapter does not silently choose a TLS provider or certificate policy

#### Scenario: Migrations are pending
- WHEN `open` observes a missing or unsupported schema version
- THEN it fails with a typed configuration or migration error
- AND it does not apply DDL automatically

### Requirement: Supported PostgreSQL Versions and Primary Authority
The system SHALL support maintained PostgreSQL 15, 16, 17, and 18 minor releases using PostgreSQL 15-compatible SQL and SHALL execute authoritative operations only on a writable primary.

#### Scenario: Supported writable primary is used
- WHEN a transaction runs on PostgreSQL 15–18 with recovery disabled and the connection is not routed to a write-disabled authority
- THEN the adapter may execute the validated state operation
- AND write transactions verify they are writable while read batches remain intentionally `READ ONLY`

#### Scenario: Standby or read-only route is selected
- WHEN any acquired connection reports recovery mode or a read-only transaction
- THEN the state operation fails explicitly before authoritative results or mutations are returned
- AND the adapter does not serve replica state as linearizable

#### Scenario: Unsupported server version is used
- WHEN the server major version is outside 15–18
- THEN `open` fails with a typed unsupported-version error
- AND migrations or runtime writes are not attempted

### Requirement: Explicit Embedded Checksummed Migrations
The system SHALL use explicit embedded versioned migrations for a fixed production schema and SHALL detect migration drift before serving state operations.

#### Scenario: Fresh database is migrated
- WHEN an authorized caller invokes `migrate`
- THEN the adapter serializes migration runners with the fixed advisory lock
- AND applies and records each embedded version and checksum transactionally

#### Scenario: Migration is invoked repeatedly
- WHEN all embedded migrations are recorded with matching checksums
- THEN `migrate` reports no pending changes
- AND existing state remains unchanged

#### Scenario: Applied checksum or version differs
- WHEN the database contains checksum drift, a version gap, or an unknown newer migration
- THEN migration and `open` fail closed
- AND the adapter does not reinterpret the existing schema

### Requirement: Private Authority Head and Empty Bootstrap
The system SHALL keep private revision/change-history state separate from the optional public `FilesystemRecord` and SHALL model an unseen filesystem as an empty authority at revision one.

#### Scenario: Empty authority is read
- WHEN no private authority-head row or public filesystem record exists
- THEN a read observes authoritative revision one and an absent filesystem point result
- AND change polling uses revision one as both current and oldest baseline

#### Scenario: Lease precedes filesystem creation
- WHEN a writer acquires or renews a lease before the public filesystem record exists
- THEN the adapter lazily creates and advances only the private authority head
- AND `ReadQuery::Filesystem` remains absent

#### Scenario: Filesystem is bootstrapped
- GIVEN a valid writer lease exists for an empty authority
- WHEN a commit requires `RecordAbsent(RecordKey::Filesystem(...))` and inserts the public filesystem/root records
- THEN the precondition succeeds if no public record exists
- AND the private head allocates the commit revision independently

#### Scenario: Public filesystem record is deleted
- WHEN a semantic commit deletes the public filesystem record
- THEN private revisions, retained changes, mutation results, and greatest fence values are not implicitly deleted
- AND no private row is exposed as a public state record

### Requirement: Fixed Fully Qualified Normalized Schema
The system SHALL store each finalized authoritative record family in normalized permanent logged tables under `w9pt_fs_state_v1` while keeping adapter-private coordination rows distinct.

#### Scenario: Runtime statement executes
- WHEN the adapter reads or mutates production state
- THEN every object reference is fixed and fully qualified
- AND correctness does not depend on `search_path`, locale, or a configurable SQL identifier

#### Scenario: Semantic key is stored
- WHEN a public record is persisted
- THEN its primary key matches finalized `RecordKey` exactly, including `(filesystem, inode, lock)` for locks and `(filesystem, inode, name)` for xattrs
- AND no stronger identity uniqueness is invented by the adapter

#### Scenario: Semantic records are deleted
- WHEN a commit removes a namespace, open, pin, orphan, lock, xattr, staging, mutation, or active lease record
- THEN every public deletion is explicit in the semantic transition
- AND cascading SQL behavior does not create hidden authoritative changes

#### Scenario: Direct malformed row is attempted
- WHEN SQL bypasses Rust validation and violates a stable ID, bound, enum, range, optional-field, or relationship constraint
- THEN a named database constraint rejects the row
- AND no malformed public record becomes readable

### Requirement: Lossless Portable Value Mapping
The system SHALL losslessly encode every finalized public state value without locale-dependent ordering, signed narrowing, precision loss, or unchecked conversion.

#### Scenario: Full unsigned boundary is stored
- WHEN a public field contains `0`, `i64::MAX`, `i64::MAX + 1`, or `u64::MAX`
- THEN the adapter round-trips it through constrained `NUMERIC(20,0)` and canonical decimal conversion
- AND no unchecked signed or floating-point cast is used

#### Scenario: Stable ID or digest is stored
- WHEN a fixed-width ID, fingerprint, or digest is written
- THEN PostgreSQL stores exact bytes under an `octet_length` constraint
- AND decoding rejects every incorrect width

#### Scenario: Byte string is ordered
- WHEN entry names, xattr names, or semantic byte identities are compared or paginated
- THEN PostgreSQL uses bounded `BYTEA` and binary ordering
- AND database collation does not change identity or ordering

#### Scenario: Content reference is reconstructed
- WHEN an inode row contains published content
- THEN all `ContentRef` fields are present and reconstructed through `ContentRef::from_persisted`
- AND partial optional field sets or inode/content summary mismatches are rejected

### Requirement: Exact Database Lease Ticks
The system SHALL define version-1 lease ticks as unsigned Unix-epoch microseconds, persist deadlines as `NUMERIC(20,0)`, and derive production time from PostgreSQL.

#### Scenario: Production time is evaluated
- WHEN a lease operation or non-replayed commit needs authoritative time
- THEN the transaction evaluates `clock_timestamp()` exactly once and converts it to a checked integral microsecond tick
- AND every expiry comparison in that operation uses the captured tick

#### Scenario: Deadline is persisted
- WHEN a grant or renewal calculates a deadline
- THEN checked tick addition produces an exact `LeaseDeadline` value stored as numeric
- AND `TIMESTAMPTZ` precision or range cannot narrow the public value

#### Scenario: Deterministic conformance time is used
- WHEN the feature-gated PostgreSQL conformance harness opens independent clients
- THEN they obtain time from one test-only database-resident integer tick row
- AND `advance_time` updates that row with checked arithmetic and no correctness-bearing shared process RAM

### Requirement: Validated Primary-WAL Durability
The system SHALL advertise only primary-WAL durable metadata and SHALL validate the PostgreSQL settings and table properties required for that boundary.

#### Scenario: Durable adapter opens
- WHEN the server is writable primary, `fsync` and `full_page_writes` are enabled, state tables are logged, and transaction-local `synchronous_commit = on` can be enforced
- THEN the adapter advertises the complete production state-store contract
- AND successful mutation acknowledgment follows SQL `COMMIT`

#### Scenario: Durability setting is weaker
- WHEN a required setting is disabled or the runtime role cannot enforce commit mode
- THEN `open` fails explicitly
- AND the deployment cannot advertise durable metadata through this adapter

#### Scenario: Synchronous-standby durability is expected
- WHEN a deployment requires acknowledged data to survive primary loss through a synchronous standby
- THEN version 1 reports that guarantee as unsupported
- AND does not infer it from primary-WAL durability

### Requirement: One-Snapshot Serializable Read Batches
The system SHALL execute each state read batch through one primary-only `SERIALIZABLE READ ONLY` transaction and return exact finalized read outcomes.

#### Scenario: Batch contains multiple queries
- WHEN one request reads related records or scans
- THEN all results come from the same PostgreSQL transaction snapshot and authoritative revision
- AND they remain positionally associated with their queries

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
- AND no unbounded query is executed

### Requirement: Exact Bounded Keyset State Scans
The system SHALL implement every finalized `RecordScan` using its exact exclusive cursor and bounded keyset order rather than offset pagination.

#### Scenario: Directory page is requested
- WHEN a caller supplies a parent inode and exclusive `DirectoryCookie`
- THEN entries are returned in unique cookie order for that parent
- AND no name component is required in the resume cursor

#### Scenario: Composite scans are requested
- WHEN open pins, locks, or xattrs are scanned
- THEN their orders are respectively `(inode, open)`, `(inode, lock)`, and `(inode, name-bytes)`
- AND the returned `ScanResume` matches the final complete record

#### Scenario: Identity scans are requested
- WHEN inodes, opens, orphans, xattr staging records, mutations, or active writer leases are scanned
- THEN the adapter orders them by their finalized identity cursor
- AND released private fence rows are not returned as active writer leases

#### Scenario: Next record cannot fit
- WHEN the next complete record exceeds the requested scan byte bound
- THEN the adapter returns `ReadOutcome::ScanBoundTooSmall` with the query position and required bytes
- AND it does not allocate or split the oversized record page

### Requirement: Serializable Ledger-First Atomic Commits
The system SHALL follow the finalized ledger-first protocol and map each new semantic commit to one short primary-only `SERIALIZABLE READ WRITE` transaction.

#### Scenario: Retained mutation is submitted under tighter current limits
- WHEN the short primary serializable fixed-size ledger probe finds a retained mutation before adapter-limit or fence validation
- THEN the adapter classifies and returns its exact replay or mismatch
- AND it does not reject an exact replay because current limits or lease state changed

#### Scenario: Ledger probe is absent
- WHEN no retained mutation exists
- THEN the adapter validates the complete request against its contract limits
- AND repeats ledger lookup first inside every write attempt before current-state validation

#### Scenario: New commit succeeds
- WHEN the mutation is absent, its fence is current, and all preconditions and invariants match
- THEN all public changes, one private authority revision, record revisions, exact terminal result, and one whole change event commit atomically
- AND acknowledgment occurs only after SQL `COMMIT` succeeds

#### Scenario: Semantic validation fails
- WHEN any typed precondition or cross-record invariant fails
- THEN the SQL transaction rolls back without a mutation-result row
- AND the adapter returns the exact finalized semantic conflict or malformed outcome

### Requirement: Deterministic PostgreSQL Lock Ordering
The system SHALL mirror finalized `RecordKey::Ord`, lock existing semantic rows in that order, and protect absent predicates serializably.

#### Scenario: Existing records are affected
- WHEN a commit changes records from multiple families
- THEN it locks the private authority head, the writer-fence row, and then public records in canonical semantic-key order
- AND independent adapters derive the same order from the same request

#### Scenario: Absent row is required
- WHEN a precondition requires a semantic row to be absent
- THEN the adapter performs the corresponding predicate read inside the serializable transaction
- AND named uniqueness constraints protect the insert race

#### Scenario: Lock conflict is evaluated
- WHEN a requested lock overlaps an incompatible existing lock on the same inode
- THEN the adapter selects the conflicting lock in canonical `LockId` order
- AND returns the finalized `CommitConflictKind::LockConflict`

#### Scenario: Duplicate affected key is submitted
- WHEN finalized preflight finds duplicate change targets
- THEN it returns `CommitOutcome::MalformedRequest` before transactional state work
- AND database deadlock behavior is not used to resolve malformed input

### Requirement: Per-Filesystem Revision Ordering
The system SHALL allocate checked monotonic revisions through the private per-filesystem authority head.

#### Scenario: Two changes target one filesystem
- WHEN two successful commits or lease transitions reach publication concurrently
- THEN locking the authority head establishes one revision order
- AND each receives a distinct increasing revision

#### Scenario: Changes target different filesystems
- WHEN unrelated filesystem IDs change concurrently
- THEN they do not contend on one global revision row
- AND each filesystem maintains its own ordering domain

#### Scenario: Revision would overflow
- WHEN incrementing the public revision domain would exceed `u64::MAX`
- THEN the operation fails explicitly and rolls back
- AND the revision never wraps or repeats

### Requirement: Exact Finalized Mutation Replay
The system SHALL classify retained mutations exclusively through `MutationContext::classify_record` before current fence validation.

#### Scenario: Matching mutation is retried
- WHEN mutation ID, request fingerprint, client incarnation, and retention match a retained record
- THEN the adapter returns exact `CommitOutcome::AlreadyCommitted`
- AND it returns the recorded result without reapplying state or validating the old fence

#### Scenario: Finalized replay identity differs
- WHEN one of mutation ID, fingerprint, client incarnation, or retention differs
- THEN the adapter returns the corresponding finalized `MutationMismatch`
- AND it does not invent adapter-specific change-set or submitted-result mismatch variants

#### Scenario: Concurrent first inserts race
- WHEN two clients attempt the same new mutation concurrently
- THEN serializable retry plus named mutation uniqueness resolves to one commit and one fresh ledger classification
- AND no raw uniqueness error escapes as filesystem behavior

### Requirement: Dedicated Content and Xattr Publication
The system SHALL implement the finalized prepared-content and xattr-staging transitions without storing target content or allowing generic publication bypasses.

#### Scenario: Prepared content is valid
- GIVEN immutable payloads and the manifest are already durable
- WHEN `PublishContent` validates mutation, base, file identity, size, data generation, inode generation, and timestamps
- THEN the inode `ContentRef` and summaries commit atomically
- AND PostgreSQL performs no target-object operation

#### Scenario: Prepared content is inconsistent
- WHEN any finalized content-publication invariant disagrees with authoritative state
- THEN the transaction returns the finalized malformed outcome
- AND the old inode content remains current

#### Scenario: Xattr staging is published
- WHEN `PublishXattrStaging` references a complete matching staging record
- THEN staging removal and xattr insertion/replacement occur atomically
- AND generic delete/insert changes cannot bypass the dedicated validation

#### Scenario: Per-block mapping is requested
- WHEN a deployment wants PostgreSQL-resident block or extent mappings
- THEN version 1 lacks that operation
- AND requires a separate approved state/content-index proposal

### Requirement: Exact SQLSTATE and Constraint Classification
The system SHALL classify PostgreSQL failures through SQLSTATE, operation phase, and known named constraints without localized message matching.

#### Scenario: Serializable transaction aborts definitively
- WHEN PostgreSQL returns `40001` or `40P01`
- THEN the adapter may retry the identical request within its configured bound
- AND never changes mutation identity, content, result, or semantic preconditions

#### Scenario: Known constraint rejects a race
- WHEN a named uniqueness, foreign-key, check, or numeric constraint rejects a request
- THEN only recognized constraint names map to documented outcomes
- AND unknown constraints remain adapter invariant failures

#### Scenario: Retry bound is exhausted
- WHEN definitive aborts repeat to the configured maximum
- THEN the adapter returns an explicit adapter failure or finalized conflict as specified by phase
- AND it does not loop indefinitely or weaken isolation

### Requirement: Ambiguous COMMIT Recovery
The system SHALL treat an uncertain SQL `COMMIT` response as potentially committed and resolve it only through bounded replay of the same owned mutation request.

#### Scenario: Commit succeeded but response was lost
- WHEN a fresh primary ledger probe finds the matching retained mutation
- THEN the adapter returns its exact committed result
- AND it does not apply changes twice

#### Scenario: Commit did not occur
- WHEN exact recovery finds no retained mutation and the same request remains valid
- THEN only that request may be attempted again
- AND no new mutation ID or semantic rebase is generated

#### Scenario: Recovery cannot determine status
- WHEN fresh-primary recovery attempts reach their configured bound
- THEN the adapter returns `CommitOutcome::Ambiguous(AmbiguousCommit)`
- AND it does not claim rollback or success

### Requirement: Database-Time Leases and Monotonic Fencing
The system SHALL implement finalized idempotent lease operations using authoritative database ticks and permanently retained monotonically increasing fence counters.

#### Scenario: Lease operation is replayed
- WHEN a retained lease-operation ID is found
- THEN its finalized request fingerprint is checked before duration, current lease, or fence validation
- AND the exact `AlreadyApplied` or retained rejection behavior is returned

#### Scenario: Scope is newly acquired or taken over
- WHEN the scope is free or expired at the captured database tick
- THEN acquisition allocates a token greater than every prior token and publishes an active lease record
- AND it emits one lease-origin revision event

#### Scenario: Lease is renewed
- WHEN the exact current unexpired fence is renewed
- THEN the token is preserved and the deadline never shortens
- AND one lease-origin revision event is emitted

#### Scenario: Lease is released
- WHEN the exact current unexpired fence is released
- THEN active lease fields are cleared while the greatest token remains
- AND the public writer-lease record becomes absent with one lease-origin revision event

#### Scenario: Lease operation is rejected
- WHEN acquire, renew, or release returns a finalized rejection
- THEN the adapter retains only the bounded replay information required by the finalized protocol
- AND it does not emit a false public revision event

#### Scenario: Stale writer commits
- WHEN a non-replayed commit presents an expired, replaced, wrong-holder, wrong-lease, or wrong-token fence
- THEN the adapter returns `StaleFence` or `ExpiredLease` as finalized
- AND the stale process cannot mutate state

### Requirement: Exact Bounded Revision Change Polling
The system SHALL implement primary-only bounded keyset polling and return exactly the finalized `ChangePollOutcome` variants.

#### Scenario: Events are available
- WHEN a caller polls after a retained revision and complete events fit
- THEN the adapter returns `Changes(ChangeBatch)` with ordered events, `next`, and `current_revision`
- AND no event is split

#### Scenario: More events remain
- WHEN event or aggregate-key bounds stop the page after at least one event
- THEN `ChangeBatch::next()` identifies the stable resume revision below `current_revision()`
- AND no adapter-specific `has_more` field is introduced

#### Scenario: First event cannot fit
- WHEN the first available whole event exceeds `max_keys`
- THEN the adapter returns `PollBoundTooSmall` with its revision and required key count
- AND it does not partially materialize that event

#### Scenario: Cursor is in the future
- WHEN the requested cursor exceeds current authority revision
- THEN the adapter returns `RevisionUnavailable`
- AND no event data is returned

#### Scenario: Cursor predates retained history
- WHEN the cursor is older than the oldest retained revision
- THEN the adapter returns `RevisionCompacted`
- AND callers must rebuild caches from authoritative reads

#### Scenario: Poll request exceeds adapter limits
- WHEN a poll was constructed under looser event/key bounds
- THEN the adapter returns `MalformedRequest`
- AND it does not issue an unbounded query

### Requirement: PostgreSQL Adapter Conformance and Recovery
The system SHALL run offline tests and the reusable live state-store conformance suite against current PostgreSQL 15–18 minor releases through independent pools.

#### Scenario: Offline workspace tests run
- WHEN no PostgreSQL DSN is configured
- THEN numeric, codec, cursor, configuration, migration-checksum, key-order, clock-conversion, and error-classification tests run deterministically
- AND ordinary workspace tests do not provision a database

#### Scenario: Live test is required
- WHEN CI sets `W9PT_POSTGRES_TEST_REQUIRED=1` with version-specific DSNs
- THEN migrations, startup validation, conformance, concurrency, leases, ambiguity, change polling, and fresh-client recovery execute
- AND a missing required DSN fails rather than silently skipping

#### Scenario: Bootstrap conformance runs
- WHEN the shared suite acquires a lease before inserting `FilesystemRecord`
- THEN independent PostgreSQL clients observe the exact revision and absence semantics of the reference authority
- AND bootstrap succeeds under `RecordAbsent`

#### Scenario: Independent client reopens state
- WHEN one pool/store is discarded and another is constructed without shared state caches
- THEN the second reconstructs records, mutation results, fences, lease-operation results, and revisions from PostgreSQL
- AND behavior matches the reusable conformance reference

### Requirement: Explicit PostgreSQL Adapter Exclusions
The system SHALL keep content layout, session durability, replica reads, wake-up notifications, and deployment concerns outside the PostgreSQL state adapter.

#### Scenario: File data or block map is supplied
- WHEN a caller attempts to store payload bytes, S3 manifests, blocks, or extent maps through version 1
- THEN the adapter lacks that operation
- AND stores only finalized metadata and inode `ContentRef` fields

#### Scenario: Session migration state is required
- WHEN fids, tags, flush dependencies, effects, or response outboxes must survive node loss
- THEN a separate session-state layer owns them
- AND filesystem-state tables do not become an implicit session database

#### Scenario: Notification or deployment feature is requested
- WHEN a caller needs `LISTEN/NOTIFY`, replica reads, synchronous-standby durability, provisioning, backups, failover orchestration, or monitoring
- THEN those features require separate deployment work or approved changes
- AND version 1 does not claim them as correctness guarantees
