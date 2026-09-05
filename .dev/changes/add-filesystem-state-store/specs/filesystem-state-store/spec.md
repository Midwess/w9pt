# Delta for Filesystem State Store

## ADDED Requirements

### Requirement: Backend-Neutral Authoritative State Boundary
The system SHALL provide a runtime-neutral filesystem-semantic state-store contract without exposing database keys, SQL, SDK clients, object-store layouts, executors, or 9P wire types.

#### Scenario: Filesystem engine uses authoritative state
- GIVEN a future filesystem engine has a semantic operation and attached security context
- WHEN it reads or commits filesystem state
- THEN it uses typed state-store requests and results
- AND the selected adapter remains private behind the contract

#### Scenario: Adapter needs a generic KV escape hatch
- WHEN an adapter cannot implement a requested semantic transition without exposing backend-specific operations
- THEN the adapter rejects the unsupported contract or configuration
- AND the public state interface is not weakened into generic key/value CRUD

#### Scenario: Protocol types change
- WHEN a 9P codec or wire result representation evolves
- THEN authoritative state records remain independent of that wire representation
- AND no state adapter is required to parse or emit 9P messages

### Requirement: Dependency and State Ownership Separation
The system SHALL keep protocol/session state, filesystem metadata coordination, immutable content layout, and database adapters in separate layers with explicit ownership.

#### Scenario: State crate is built
- WHEN `w9pt-fs-state` is compiled
- THEN it may depend on portable `w9pt-fs-storage` content types
- AND it does not depend on `w9pt`, a database SDK, an async runtime, or a transport

#### Scenario: Connection state must survive movement
- WHEN fids, tags, flush dependencies, pending responses, or session outboxes must survive node loss
- THEN that state belongs to a separate session-state contract
- AND it is not silently placed in filesystem metadata records

#### Scenario: File content changes
- WHEN immutable payloads and a manifest are prepared
- THEN `w9pt-fs-storage` owns their physical layout
- AND `w9pt-fs-state` owns only the authoritative inode publication of the resulting content reference

### Requirement: Honest Store Guarantees and Writer Topology
The system SHALL validate a state-store contract describing proven consistency, durability, fencing, revision, bounds, and writer-topology behavior.

#### Scenario: Serializable multi-writer adapter is configured
- WHEN an adapter advertises `SerializableMultiWriter`
- THEN independently opened clients may submit concurrent commits
- AND conflicting histories are serialized or returned as typed conflicts

#### Scenario: Single fenced writer adapter is configured
- WHEN an adapter advertises `SingleFencedWriter`
- THEN only the current filesystem-wide fenced writer may commit
- AND all stale or parallel writer claims are rejected

#### Scenario: Required guarantee is missing
- WHEN a writable adapter cannot prove linearizable reads, serializable multi-record commits, durable acknowledgment, atomic result persistence, monotonic fencing, or bounded operations
- THEN writable construction fails explicitly
- AND the deployment does not advertise filesystem capabilities that depend on that guarantee

### Requirement: Stable Portable State Identities
The system SHALL represent every persistent filesystem, inode, open, lock, lease, writer, client, revision, cookie, and fencing identity with a stable bounded value that is portable across processes and nodes.

#### Scenario: Work moves to another node
- GIVEN authoritative records were created by one process
- WHEN another process reads them with empty local state
- THEN every referenced identity remains resolvable
- AND no pointer, SDK handle, cache address, or process-local collection index is required

#### Scenario: Monotonic value reaches its maximum
- WHEN incrementing a revision, generation, directory cookie, or fencing token would overflow
- THEN the operation returns a typed terminal error before state mutation
- AND the value never wraps or repeats

#### Scenario: Inode owns content
- WHEN a regular inode is associated with immutable file content
- THEN it stores an explicit `w9pt_fs_storage::FileId` binding
- AND no undocumented numeric conversion from `InodeId` is used

### Requirement: Bounded Validated Authoritative Records
The system SHALL define checked, versioned semantic records for filesystems, inodes, directories, opens, orphans, locks, xattrs, mutations, and writer leases.

#### Scenario: Record is constructed
- WHEN a caller constructs an authoritative record
- THEN all names, values, counts, sizes, ranges, generations, and kind-specific fields are validated
- AND inconsistent records are rejected before entering a commit request

#### Scenario: Regular-file content is present
- WHEN an inode carries a `ContentRef`
- THEN its content-file identity, logical size, and data generation agree with the reference
- AND a mismatch is rejected as a malformed state transition

#### Scenario: Record kind and key disagree
- WHEN a record value is paired with a key for another record family or identity
- THEN validation fails explicitly
- AND the adapter does not reinterpret or store the mismatched value

### Requirement: Stable Namespace and Directory Cookies
The system SHALL represent namespace entries separately from stable inode identity and persist ordered non-reused directory cookies.

#### Scenario: File is renamed
- WHEN a directory entry moves or changes name
- THEN its child inode identity remains stable
- AND source, destination, directory generations, overwrite effects, and timestamps can be changed in one commit

#### Scenario: Directory is enumerated
- WHEN a caller resumes a bounded directory scan after a returned cookie
- THEN the adapter returns the next entries in canonical cookie order from one state revision
- AND the cookie is not interpreted as a vector offset, target listing position, or name hash

#### Scenario: Entry is removed and another is inserted
- WHEN a directory cookie has previously been assigned
- THEN a later entry does not reuse that cookie
- AND clients cannot confuse the later entry with an earlier enumeration position

### Requirement: One-Revision Consistent Bounded Reads
The system SHALL execute every bounded read batch against one authoritative revision and positionally associate each result with its query.

#### Scenario: Multiple related records are read
- WHEN a batch requests an inode, parent entry, open pins, and mutation record
- THEN all results reflect one state revision
- AND the caller can use their record revisions as commit preconditions

#### Scenario: Freshness floor is requested
- GIVEN a caller requires at least revision `R`
- WHEN the adapter serves the read
- THEN it returns a linearizable snapshot at `R` or a newer revision
- AND it does not claim to provide a historical revision that it cannot retain

#### Scenario: Scan bound would be exceeded
- WHEN a directory, lock, open, xattr, or mutation scan would exceed its item or byte limit
- THEN the adapter returns a bounded page and stable resume cursor or a typed limit error
- AND it does not materialize an oversized result first

### Requirement: Serializable Atomic Declarative Commit
The system SHALL apply a validated `CommitRequest` as one serializable, durable, all-or-nothing transition across every affected authoritative record.

#### Scenario: Preconditions remain valid
- GIVEN the mutation is not already retained and its writer fence is current
- WHEN every typed precondition matches authoritative state
- THEN all record changes, record revisions, one state revision, one change event, and the mutation result commit atomically
- AND success is returned only after the advertised durable boundary

#### Scenario: One precondition conflicts
- WHEN any record revision, absence, generation, content base, link count, open-pin count, or fence precondition does not match
- THEN no requested state change is visible
- AND the caller receives a typed conflict or rejection suitable for semantic revalidation

#### Scenario: Invalid change exists late in the request
- WHEN any change or cross-record invariant in a bounded commit is malformed
- THEN complete preflight validation rejects the request
- AND no earlier change in the same request is applied

### Requirement: Immutable Content Publication Handoff
The system SHALL publish prepared immutable content only through the same authoritative transaction that updates its inode metadata and retained mutation result.

#### Scenario: Prepared content is published
- GIVEN all immutable payloads and the manifest were acknowledged durable by `w9pt-fs-storage`
- WHEN `PublishContent` validates against the current inode base
- THEN the new `ContentRef`, logical size, timestamps, data generation, and terminal result commit together
- AND the state store performs no target-object reads or writes

#### Scenario: Prepared identity is inconsistent
- WHEN the prepared mutation, file identity, base content, logical size, or generation does not match the commit or inode
- THEN the commit is rejected before metadata mutation
- AND the existing content reference remains authoritative

#### Scenario: Database transaction conflicts after preparation
- WHEN content was prepared but an authoritative precondition changes before commit
- THEN the commit exposes none of the new inode state
- AND unreachable immutable objects remain safe for later garbage collection

### Requirement: Durable Idempotent Mutation Ledger
The system SHALL atomically retain each committed mutation's filesystem ID, stable mutation ID, complete request fingerprint, client incarnation, writer context, exact terminal result, committed revision, and retention horizon.

#### Scenario: Identical committed mutation is retried
- GIVEN the mutation record remains within its retention horizon
- WHEN the same mutation ID, fingerprint, and client incarnation are submitted again
- THEN the store returns `AlreadyCommitted` with the exact retained result
- AND it does not revalidate an expired fence or reapply state changes

#### Scenario: Mutation ID is reused with different operands
- WHEN a retained mutation ID is submitted with another fingerprint, client incarnation, or semantic identity
- THEN the store returns a hard mutation-mismatch result
- AND it does not return the old result or apply the new changes

#### Scenario: Mutation is not committed
- WHEN a commit returns a precondition conflict before the atomic commit point
- THEN no committed mutation result is retained
- AND the future engine may reread, reauthorize, and submit a newly valid attempt

### Requirement: Monotonic Writer Fencing
The system SHALL use monotonically increasing fencing tokens for every writer scope and reject stale tokens on every non-replayed mutation commit.

#### Scenario: Lease is taken over after expiry
- GIVEN writer A held token `N` and its lease expired
- WHEN writer B acquires the same scope
- THEN writer B receives a token greater than `N`
- AND every later commit carrying token `N` is rejected

#### Scenario: Lease is released and reacquired
- WHEN a holder releases a writer lease and any holder later reacquires the scope
- THEN the newly granted token is greater than all previous tokens
- AND release never resets or permits reuse of a token

#### Scenario: Clock behavior is uncertain
- WHEN lease deadline observations race or clocks differ
- THEN fencing-token comparison still prevents a stale writer from committing
- AND the system does not rely on lease timing alone for safety

### Requirement: Idempotent Writer Lease Lifecycle
The system SHALL provide bounded, replayable acquire, renew, and release operations using an explicit adapter-owned time authority supplied by the caller.

#### Scenario: Free scope is acquired
- WHEN a caller acquires a free or expired scope with a stable lease operation identity
- THEN the store grants one lease ID, deadline, holder, and new fence token atomically
- AND retrying the same acquire returns the same recorded outcome

#### Scenario: Active scope is held by another writer
- WHEN another non-expired holder attempts acquisition
- THEN the store returns a typed held/conflict outcome
- AND the active lease and fence remain unchanged

#### Scenario: Lease is renewed or released ambiguously
- WHEN the adapter cannot reveal whether renew or release committed
- THEN retrying the identical lease operation resolves to its recorded outcome
- AND the caller never guesses a new lease identity or fence

### Requirement: Atomic Open-Unlinked and Orphan State
The system SHALL coordinate portable open records, durable open pins, link counts, and orphan records through atomic state transitions.

#### Scenario: Last name is unlinked while opens remain
- GIVEN an inode has one link and one or more durable open pins
- WHEN unlink commits
- THEN the directory entry is removed, link count becomes zero, and an orphan record retains the inode atomically
- AND existing portable opens remain resolvable

#### Scenario: Final open pin is released
- GIVEN an orphan has no links and one final open pin
- WHEN that open is released
- THEN the open, pin, orphan, and inode retirement state change atomically
- AND no observer sees an unpinned orphan incorrectly retained or prematurely removed

#### Scenario: Processing node disappears
- WHEN another node resolves an open or orphan record
- THEN it uses portable shared identities and current record revisions
- AND correctness does not depend on the original node's memory

### Requirement: Cross-Session Locks and Xattr State
The system SHALL store byte-range lock ownership and correctness-bearing xattr staging state in portable bounded records.

#### Scenario: Byte-range lock conflicts
- WHEN a lock request overlaps an incompatible lock owned by another session or open
- THEN the authoritative transition reports the deterministic conflict
- AND no process-local mutex is treated as the cross-session authority

#### Scenario: Lock owner is stale
- WHEN a lock mutation carries an expired or stale writer/owner fence required by its policy
- THEN the mutation is rejected
- AND the stale process cannot release or replace a newer owner's lock

#### Scenario: Xattr staging is published
- WHEN a bounded xattr staging record reaches its exact expected size
- THEN staging retirement and xattr publication can commit atomically
- AND oversized or incomplete staging is rejected without partial publication

### Requirement: Bounded Revision Change Polling
The system SHALL expose nonblocking bounded whole-commit revision polling for cache invalidation while keeping authoritative reads revision-validated.

#### Scenario: Changes are available
- WHEN a caller polls after revision `R` within retained history
- THEN it receives a bounded ordered batch of whole commit events after `R`
- AND no event is split across polling pages

#### Scenario: No changes are available
- WHEN the current authoritative revision equals the requested cursor
- THEN polling returns an empty batch with the current revision
- AND it does not block on a runtime-specific stream

#### Scenario: Requested history was compacted
- WHEN a polling cursor predates the oldest retained event
- THEN the store returns `RevisionCompacted` with the oldest available revision
- AND the caller must discard affected caches and perform authoritative reads

### Requirement: Ambiguous Failure and Reopen Safety
The system SHALL permit safe recovery from failures before or after the atomic state commit point without exposing a partial transaction.

#### Scenario: Failure occurs before commit
- WHEN an adapter failure is injected before the authoritative transition
- THEN independently opened clients observe the complete old record set
- AND no mutation result or change event claims the transition committed

#### Scenario: Failure occurs after commit acknowledgment becomes ambiguous
- WHEN all records and the result ledger committed but the caller did not receive a definitive response
- THEN independently opened clients observe the complete new record set
- AND retrying the identical mutation returns the retained result without reapplying changes

#### Scenario: Client-local state is discarded
- WHEN a new client is constructed with empty local caches after a process loss
- THEN it reconstructs behavior from the authoritative store alone
- AND the result is unchanged from a client that remained alive

### Requirement: Deterministic Reference and Adapter Conformance
The system SHALL provide a deterministic memory authority and reusable conformance suite for state semantics, concurrency, failures, bounds, leases, and recovery.

#### Scenario: Two independent clients operate concurrently
- WHEN clients with no shared correctness-bearing cache perform conflicting and disjoint reads and commits
- THEN conflicts and successful state revisions match the serializable reference model
- AND the same operation schedule produces the same trace

#### Scenario: Adapter is added later
- WHEN a SQLite, PostgreSQL, etcd, SlateDB, or other adapter is implemented
- THEN it runs the shared state-store conformance suite
- AND adds adapter-specific schema, migration, durability, failover, and transaction-boundary tests

#### Scenario: Memory authority passes tests
- WHEN the deterministic memory implementation passes semantic conformance
- THEN it serves as a reference oracle for adapter behavior
- AND it does not independently justify production `DurableMetadata` capability
