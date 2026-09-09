# Delta for Filesystem Semantic Engine

## ADDED Requirements

### Requirement: Coherent Filesystem Crate Naming
The system SHALL expose filesystem semantics as `w9pt-fs`, authoritative metadata as `w9pt-fs-state`, and immutable content as `w9pt-fs-storage`, while retaining `w9pt` as the independent protocol/session crate.

#### Scenario: Workspace is built after the rename
- WHEN Cargo resolves the active workspace packages and path dependencies
- THEN semantic code imports `w9pt_fs`, state code imports `w9pt_fs_state`, and content code imports `w9pt_fs_storage`
- AND no alternate package or import identity is provided

#### Scenario: Earlier development storage is encountered
- GIVEN content or code was produced under the former development crate identity
- WHEN the current `w9pt-fs-storage` build is deployed
- THEN no old package alias, fingerprint domain, reader, or key-space migration is provided
- AND the operator recreates the development object prefix using the current build

### Requirement: Runtime-Neutral Semantic Engine
The system SHALL provide a backend-neutral filesystem semantic engine that executes owned `w9pt` filesystem operations through authoritative state and immutable content interfaces without performing transport, database-driver, target-SDK, clock, random, or executor work internally.

#### Scenario: Filesystem effect is executed
- GIVEN `w9pt::Session` emitted an owned filesystem effect
- WHEN the host supplies it with a valid execution context to the engine
- THEN the engine returns the exact successful result kind or stable filesystem error expected by that effect
- AND the host can complete the original operation without interpreting state records or storage manifests

#### Scenario: Concrete infrastructure is selected
- WHEN a host embeds the engine with a state adapter, target adapter, policy, identities, clocks, and executor
- THEN those dependencies remain caller-owned and statically dispatched
- AND the engine core does not depend on their SDKs or runtime

### Requirement: Explicit Execution Identity and Authority
The system SHALL require caller-owned globally stable mutation/client identities, a frozen operation timestamp, retention identity, and an exact current writer fence for every authoritative state mutation.

#### Scenario: Mutating operation is executed
- WHEN the engine receives a state-changing operation
- THEN it requires a stable mutation ID distinct from the 9P tag and session-local operation ID
- AND includes the client incarnation, retention, complete request fingerprint, and exact writer fence in the authoritative commit

#### Scenario: Required authority is absent
- WHEN a mutating operation lacks its stable identity, timestamp, or writer fence
- THEN the engine returns a typed non-committing failure
- AND performs no target upload or authoritative state change

#### Scenario: Planner changes runner-owned authority
- WHEN semantic planning returns a commit for another filesystem, mutation context, or writer fence
- THEN the mutation runner returns a typed planned-commit mismatch before dispatching it
- AND neither the initial commit nor an ambiguity retry reaches the state adapter

#### Scenario: Read-only operation is executed
- WHEN an operation does not change authoritative state
- THEN it may execute without a mutation ID or writer fence
- AND it cannot accidentally enter a publication path

### Requirement: Safe Terminal Outcome Boundary
The system SHALL distinguish a client-safe terminal filesystem result from an unresolved infrastructure or commit-authority failure.

#### Scenario: Terminal semantic result is known
- WHEN an operation has definitively succeeded or failed without an uncertain commit
- THEN the engine returns the originating operation ID and a client-safe `FilesystemResult` or `FilesystemError`
- AND that value may be supplied exactly once to `Session::complete`

#### Scenario: Commit status is uncertain
- WHEN the state adapter reports an ambiguous commit
- THEN the engine does not guess a client-visible result
- AND retains or reconstructs the identical commit request for mutation-ledger resolution

### Requirement: Stable QID and Portable Handle Identity
The system SHALL use portable 128-bit inode/open handles and separately persist a nonzero, per-filesystem unique, monotonically allocated, non-reused 64-bit QID path for every inode.

#### Scenario: Object or open handle is routed
- WHEN any engine instance resolves an `ObjectHandle` or `OpenHandle`
- THEN it reconstructs the stable inode or open identity without a pointer, process-local index, SDK value, or cache address
- AND verifies that the resolved state belongs to the request export and client where applicable

#### Scenario: QID is returned
- WHEN the engine returns an inode identity to a 9P client
- THEN `Qid::path` comes from the inode's persisted `QidPath`
- AND it is never produced by hashing or truncating the 128-bit inode identity

#### Scenario: Inode is allocated
- WHEN a namespace mutation creates an inode
- THEN QID-path allocation and inode insertion commit atomically
- AND zero, duplicate, reused, or overflowing QID paths are rejected

### Requirement: Authoritative Directory Ancestry
The system SHALL persist one authoritative parent inode for every directory, with the export root self-parented, and update ancestry atomically with directory moves.

#### Scenario: Dot-dot is walked
- WHEN a walk resolves `..` from a directory below the export root
- THEN it resolves the authoritative parent inode
- AND walking `..` from the export root remains at the root

#### Scenario: Directory is moved
- WHEN `RenameAt` moves a directory to another parent
- THEN source/destination namespace changes and the directory parent update commit together
- AND no observer sees the moved dentry with the old parent relationship

#### Scenario: Rename would create a cycle
- WHEN the destination lies within the moved directory's bounded descendant tree
- THEN the mutation fails with the documented loop/invalid error
- AND no namespace or parent record changes

### Requirement: Revalidated Authorization and Export Policy
The system SHALL evaluate standard filesystem access rules from caller-resolved credentials and authoritative records, and SHALL protect mutation decisions with exact policy and record preconditions.

#### Scenario: Access is evaluated
- WHEN a principal walks, opens, reads, writes, or changes a namespace or inode
- THEN the engine applies directory search/write, inode mode, owner/group, supplementary-group, privilege, sticky-bit, setgid-directory, and read-only export rules applicable to the operation
- AND bidirectional numeric uid/gid and canonical principal/group conversion is provided by the caller-owned export identity policy

#### Scenario: Policy or inode changes before commit
- WHEN the policy generation or any authorization-relevant record changes after evaluation
- THEN the commit conflicts
- AND the engine rereads and reevaluates authorization before retrying

#### Scenario: Access is denied
- WHEN resolved credentials do not authorize the complete operation
- THEN the engine returns `EACCES`, `EPERM`, or `EROFS` as appropriate
- AND performs no content preparation or state mutation

#### Scenario: Unprivileged regular-file creation requests setgid
- GIVEN setgid-directory inheritance selects a group outside the creator's groups
- WHEN an unprivileged creator requests setgid on a new regular file
- THEN the engine clears the setgid bit while preserving the inherited group
- AND privileged creators or creators belonging to the resulting group may retain it

### Requirement: Bounded Correct Walk
The system SHALL resolve walk components with stable identity, root confinement, directory search permission, correct dot semantics, and stock non-empty partial-walk behavior.

#### Scenario: Components resolve successfully
- WHEN every requested component exists and is searchable
- THEN the engine returns one ordered `WalkElement` per component
- AND every element contains the stable portable object handle and QID of that inode

#### Scenario: Later component is absent
- GIVEN at least one requested component resolved successfully
- WHEN a later component is missing
- THEN the engine returns the non-empty resolved prefix
- AND does not fabricate a result for the missing suffix

#### Scenario: First component is absent or path is invalid
- WHEN the first component is missing, an intermediate inode is not a directory, root confinement would be escaped, or a configured bound is exceeded
- THEN the engine returns the corresponding stable error
- AND performs no namespace mutation

### Requirement: One-Revision Directory Enumeration
The system SHALL return bounded complete directory entries in persistent cookie order using child type and QID data belonging to one authoritative revision.

#### Scenario: Directory page is read
- WHEN `ReadDir` starts at a valid cookie with a byte/count bound
- THEN the engine returns only whole entries after that cookie in stable order
- AND every entry contains its exact name, next cookie, child kind, and stable QID from the same state revision

#### Scenario: Entry cannot fit
- WHEN the next complete entry cannot fit the client or configured result bound
- THEN the engine stops before that entry or returns the documented range error when no entry can fit
- AND it never emits a truncated directory entry

### Requirement: Portable Open and Release Lifecycle
The system SHALL create, resolve, and release authoritative portable open records and pins atomically with their retained mutation results.

#### Scenario: Existing inode is opened
- WHEN flags, kind, export policy, and permissions allow an open
- THEN the engine atomically inserts the deterministic open record and pin
- AND returns the stable QID, portable `OpenHandle`, and bounded I/O unit

#### Scenario: Open requests unavailable write guarantees
- WHEN a writable or truncating open targets a read-only export, or requests unsupported `O_DSYNC` or `O_SYNC` semantics
- THEN the engine returns `EROFS` or `EOPNOTSUPP` before target or state mutation
- AND no accepted open silently loses a requested durability guarantee

#### Scenario: Truncate-on-open is requested
- WHEN an existing writable regular file is opened with truncation
- THEN immutable truncated content is prepared before the metadata transaction
- AND open, pin, content, size, timestamps, generations, and exact result publish together

#### Scenario: Open is released
- WHEN the owning client releases a portable open
- THEN the open and pin are removed atomically with any required orphan update
- AND content objects are not synchronously deleted

### Requirement: Atomic Empty Regular-File Creation
The system SHALL create a regular file as one atomic namespace, inode, open, pin, allocation, timestamp, and retained-result mutation with an explicit unpublished-empty content state.

#### Scenario: File is created successfully
- WHEN the destination is absent and policy permits creation
- THEN the engine inserts the empty generation-zero inode, dentry, open, pin, stable cookie/QID allocations, parent updates, and exact result in one commit
- AND the file is immediately readable as zero bytes without an object-store head

#### Scenario: Destination races with creation
- WHEN another mutation creates or replaces the destination before commit
- THEN the engine revalidates the complete create operation or returns `EEXIST`
- AND never exposes an inode without its namespace/open transaction

### Requirement: Immutable Positioned Reads
The system SHALL resolve a read through one authoritative open and one immutable content reference, returning only verified bytes within the requested positioned range and logical EOF.

#### Scenario: Published regular file is read
- WHEN a readable open references an inode with published content
- THEN the engine reads through the persisted raw or block-split method
- AND one response contains bytes from exactly one verified immutable version

#### Scenario: New empty file is read
- WHEN a readable open references an unpublished generation-zero regular inode
- THEN the engine returns EOF without target-object access
- AND does not create content as a side effect

#### Scenario: Stored content is corrupt
- WHEN manifest or payload validation fails
- THEN the engine returns `EIO`
- AND does not publish metadata accepting the corrupt bytes

#### Scenario: Read completes under first-slice time policy
- WHEN a file or directory read succeeds
- THEN the engine leaves access time unchanged under the explicit no-atime policy
- AND strict or relative automatic-atime mutation remains outside this change

### Requirement: Atomic Positioned Write and Append
The system SHALL prepare immutable content before atomically publishing the exact content reference, logical size, timestamps, generations, and retained written-count result.

#### Scenario: Existing content is written
- WHEN a positioned write is authorized against an exact current content base
- THEN the engine prepares immutable payloads and manifest before the state commit
- AND the commit publishes all inode summaries and the exact result together

#### Scenario: New empty file receives a sparse write
- WHEN the first write begins beyond offset zero on an unpublished file
- THEN preparation is bound to `BaseContentIdentity::NEW_FILE`
- AND block-split leaves the gap sparse while raw obeys its materialization bound

#### Scenario: Append conflicts
- WHEN another writer changes EOF before an append commit
- THEN the engine rereads the inode, recomputes EOF, reauthorizes, and reprepares within a checked retry bound
- AND never acknowledges two appends at the same serialized position

#### Scenario: Partial-write base changes
- WHEN a content-base or data-generation precondition conflicts
- THEN the engine discards the old semantic plan and reprepares against the new complete base
- AND never publishes the stale prepared partial update by last-write-wins

### Requirement: Atomic Selected Setattr
The system SHALL validate and apply every selected `Setattr` field as one authorized inode mutation, including immutable content publication when size changes.

#### Scenario: Metadata-only attributes change
- WHEN mode, owner, group, or selected timestamps are authorized without a size change
- THEN all selected fields and inode generation publish together
- AND unselected fields remain unchanged

#### Scenario: Size and metadata change together
- WHEN one request selects size plus other attributes
- THEN truncate preparation completes before one content-plus-attributes state transition
- AND no observer sees the new size/content with old selected attributes or the reverse

#### Scenario: Unpublished file is extended
- WHEN an empty generation-zero file is truncated to a nonzero size
- THEN the engine prepares generation-one content against the new-file base
- AND every newly visible byte reads as zero

#### Scenario: Size is selected
- WHEN `Setattr` selects size for a non-regular inode
- THEN the engine rejects the request without mutation
- AND a successful regular-file size change advances modification time unless an explicit selected value replaces it

### Requirement: Atomic Namespace Operations
The system SHALL execute `Mkdir`, `Symlink`, `Link`, `UnlinkAt`, and `RenameAt` as serializable multi-record namespace mutations with stable cookies/QIDs, link counts, generations, timestamps, ancestry, and retained results.

#### Scenario: Directory or symlink is created
- WHEN the parent and destination satisfy kind, absence, bounds, and permission rules
- THEN inode/dentry allocation and all parent updates commit atomically
- AND retries return the exact recorded node result

#### Scenario: Hard link is created
- WHEN a non-directory target and destination directory belong to the same export and policy permits the link
- THEN the new dentry, cookie, target link count/times, and parent changes commit together
- AND the target QID remains unchanged

#### Scenario: Name is unlinked
- WHEN `UnlinkAt` passes flag/kind, permission, sticky, and directory-emptiness checks
- THEN dentry removal, parent updates, link count, and orphan/retirement decision commit atomically
- AND an exact open-pin count is revalidated inside that transaction

#### Scenario: Name is renamed
- WHEN `RenameAt` moves or replaces a source name
- THEN source/destination entries, preserved source cookie, parent generations/times, overwritten target state, and moved-directory ancestry commit together
- AND conflicts cause full namespace and authorization revalidation

### Requirement: Open-Unlinked Lifetime
The system SHALL retain a zero-link inode while authoritative open pins exist and retire it atomically after the final pin is released.

#### Scenario: Last name is unlinked while open
- WHEN unlink reduces the link count to zero and the exact open-pin count is nonzero
- THEN dentry removal and orphan creation commit together
- AND the portable open continues to read or write the inode

#### Scenario: Final pin is released
- WHEN the final open on a zero-link orphan is released
- THEN open, pin, orphan, and inode retirement commit atomically
- AND immutable content becomes unreachable for later GC rather than being deleted on the request path

#### Scenario: Empty open directory is unlinked
- WHEN an empty directory loses its last name while a directory open pin exists
- THEN the directory and pin remain in the orphan set until release
- AND namespace mutation through the zero-link directory is rejected

### Requirement: Durable Exact Mutation Replay
The system SHALL construct a complete stable-intent fingerprint and probe the mutation ledger before allocation or content preparation, returning a retained result only for an identical semantic request and client incarnation.

#### Scenario: Exact mutation is retried
- WHEN mutation ID, request fingerprint, client incarnation, retention, and result-codec version match a retained record
- THEN the engine returns the decoded exact prior result without reapplying state or uploading content
- AND create/open retries reproduce the same handles and QIDs

#### Scenario: Mutation identity is reused
- WHEN the same mutation ID carries different stable context, operands, timestamp-dependent choices, fingerprint, client incarnation, or retention
- THEN the engine returns a hard mismatch
- AND performs no state or target mutation

#### Scenario: Zero-length write is replayed
- WHEN a validated zero-length write is repeated with the same mutation identity and fingerprint
- THEN the exact zero-count result is replayed from the ledger
- AND reuse of that identity for nonempty bytes is a hard mismatch

#### Scenario: Ledger probe is reconstructed after restart
- WHEN an engine with empty local state retries a mutation whose state allocation high-water marks have advanced
- THEN it reconstructs the same fingerprint entirely from stable pre-allocation intent
- AND exact allocated handles, QIDs, cookies, and counts come from the retained terminal result rather than becoming fingerprint inputs

#### Scenario: Retained result is decoded
- WHEN the engine reads a mutation result from authoritative state
- THEN it validates kind, version, bounds, exact lengths, handles, QIDs, and trailing data
- AND unknown or malformed encodings fail without inventing a response

### Requirement: Bounded Conflict and Ambiguity Handling
The system SHALL distinguish definitive semantic conflicts from ambiguous commits and handle each with a bounded correctness-preserving algorithm.

#### Scenario: Definitive conflict occurs
- WHEN a commit proves that its precondition did not match and did not apply
- THEN the engine rereads, reauthorizes, recomputes the full operation, and reprepares content if needed
- AND stops with `EAGAIN` after the configured conflict bound

#### Scenario: Commit is ambiguous
- WHEN the adapter cannot reveal whether the exact commit applied
- THEN the engine retries the identical commit request through ledger-first resolution
- AND does not rebase or issue a different mutation while the original status is unknown

#### Scenario: Writer fence is stale
- WHEN the authoritative commit rejects an expired or superseded writer fence
- THEN the engine does not commit or retry under that fence
- AND returns an authority failure requiring a current caller-supplied fence

### Requirement: Write-Through Fsync Semantics
The system SHALL implement data-only and full `Fsync` according to the exact durability guarantees of the selected target and authoritative state adapter.

#### Scenario: Data-only sync succeeds
- WHEN an open regular file references content whose immutable dependencies are already durable and validate successfully
- THEN data-only sync succeeds without a background write-back flush
- AND it does not claim unrelated namespace durability

#### Scenario: Full sync is requested
- WHEN both target data and authoritative metadata durability are proven by the selected contracts
- THEN full sync succeeds after validating the open and current content state
- AND the engine never upgrades primary-only or memory-reference guarantees into a stronger failover promise

### Requirement: Capability-Complete First Slice
The system SHALL derive export capabilities from completed end-to-end semantics, export policy, state guarantees, target guarantees, and available providers rather than advertising a static all-features set.

#### Scenario: First-slice capability is advertised
- WHEN every required layer provides the operation and its atomicity/durability semantics
- THEN the engine may include that operation and guarantee in the attach capability set
- AND conformance exercises its complete request-to-publication-to-result path

#### Scenario: Deferred operation is requested
- WHEN fid-based `Rename`/`Remove`, `Mknod`, `Statfs`, xattr, lock, cancellation, or migration semantics are not implemented by this change
- THEN the corresponding capability is absent and the operation returns `EOPNOTSUPP` before state or target mutation
- AND the engine does not approximate the missing semantics

### Requirement: Deterministic Semantic Conformance
The system SHALL provide deterministic tests that drive real 9P session effects through independently constructed semantic engines, authoritative state clients, and immutable content repositories.

#### Scenario: Full request path is tested
- WHEN encoded 9P requests are fed to `w9pt::Session`
- THEN emitted filesystem effects execute through the engine and return exact completions and response frames
- AND traces are deterministic for identical inputs, identities, policy, time, and failure schedules

#### Scenario: Two engines operate concurrently
- WHEN engines with no shared correctness-bearing RAM perform overlapping/disjoint writes, append, truncate, renameat, link/unlink, permission changes, and open-unlinked operations
- THEN results match one serializable filesystem history
- AND rebuilding an engine from empty local state does not change authoritative behavior

#### Scenario: Failure is injected
- WHEN failure occurs before or after payload, manifest, metadata commit/result publication, completion handoff, or response enqueue
- THEN recovery observes the complete old or complete new state and exact retained result
- AND never observes mixed content/metadata or applies a mutation twice
