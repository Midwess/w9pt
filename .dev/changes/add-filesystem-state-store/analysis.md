# Analysis: Add Filesystem State Store

## Current State

- The workspace contains `w9pt` and `w9pt-fs-storage`; no crate owns authoritative filesystem metadata or cross-session coordination.
- `w9pt` is a dependency-free Sans-I/O protocol/session state machine. It emits high-level filesystem requests using opaque `ObjectHandle`, `OpenHandle`, and `XattrHandle` values, but those handles are not persistent database records.
- `w9pt-fs-storage` prepares immutable file content and returns portable `PreparedContent`/`ContentRef` values. Its object-head publisher is explicitly standalone-only; clustered publication belongs in an authoritative inode transaction.
- The repository has no archived `.dev/specs/` baseline yet. Normative implemented behavior remains in the completed `complete-sans-io-core` and `add-storage-methods` change packages plus `AGENTS.md`.
- The Git history contains only the initial repository commit. Current Rust crates and `.dev` documents are uncommitted user work and must be preserved.

## Request Interpretation

The requested trait crate is interpreted as `crates/w9pt-fs-state`, containing:

- authoritative filesystem-state domain types;
- a runtime-neutral `FilesystemStateStore` trait;
- consistent read revisions and bounded ordered scans;
- serializable declarative transactions;
- durable mutation identity and result replay;
- leases plus monotonically increasing fencing tokens;
- bounded revision polling for cache invalidation;
- a deterministic memory authority and reusable conformance suite.

SQLite/local-file, PostgreSQL, etcd, SlateDB, the filesystem semantic engine, and durable 9P session state are separate future changes. A future local-file implementation should normally use SQLite rather than invent an ad hoc database/WAL format.

## Similar Features and Conventions

### Runtime-neutral target contract

`w9pt-fs-storage::TargetStore` establishes the local pattern for:

- associated adapter errors;
- return-position future methods without a mandatory executor;
- owned request/result values;
- explicit semantic guarantees;
- bounded reads;
- deterministic memory behavior and public conformance support.

`FilesystemStateStore` should follow that mechanical style while exposing filesystem-state transactions rather than target-object operations.

### Strong portable values

`w9pt-fs-storage` already provides fixed-width caller-supplied identifiers, checked constructors, validated limits, typed failures, deterministic operation fingerprints, and reconstructible `ContentRef` values. The state crate should reuse those content values where their semantics match and define distinct state-mutation fingerprints for the complete filesystem operation.

### Capabilities are promises

`w9pt::CapabilitySet` treats advertised functionality as enforceable semantic guarantees. The state crate should likewise describe proven isolation, durability, fencing, revision, and writer-topology behavior. A future filesystem engine must omit capabilities such as `AtomicNamespace`, `DurableMetadata`, `OpenUnlinked`, or `CrossSessionLocks` when the selected adapter cannot satisfy the complete contract.

### Wire types are not database records

The existing `w9pt` file attributes, directory entries, QIDs, lock requests, and opaque handles are protocol-facing semantic values. Reusing them as persistent storage records would couple adapter schemas to wire details. The future filesystem engine should map between the two layers.

## Layering and Dependencies

```text
w9pt                         w9pt-fs-state -> w9pt-fs-storage
  \                               /
   \-> future filesystem engine <-/
```

- `w9pt-fs-state` depends on `w9pt-fs-storage` for immutable content publication values.
- It does not depend on `w9pt`, so database adapters do not inherit protocol parsing, session state, or wire-shaped types.
- `w9pt` remains unchanged and dependency-free.
- The future filesystem engine depends on all three layers and maps 9P requests into state reads, content preparation, and state commits.
- Production adapter crates depend on their selected database SDK and runtime without pulling those dependencies into the state contract.

## Required State Ownership

The authoritative state contract must cover:

- stable filesystem and inode identities;
- filesystem roots and export bindings;
- inode kind, mode, ownership, timestamps, size, link count, generations, and current `ContentRef`;
- directory entries, directory generations, and stable non-reused readdir cookies;
- portable opens, open pins, orphans, and open-unlinked lifetime;
- byte-range locks with cluster-resolvable owners;
- xattr metadata and portable staging state;
- mutation IDs, complete request fingerprints, client incarnations, committed results, and retention horizons;
- writer scopes, leases, holder incarnations, and monotonically increasing fencing tokens;
- bounded revision events used only to invalidate optional caches.

Fid bindings, active tags, flush dependencies, ordered responses, and effect inbox/outbox state belong to a later session-state contract rather than this crate.

## Proposed Contract Shape

Use a small semantic interface:

```text
contract() -> StateStoreContract
read(ReadBatch) -> StateSnapshot
commit(CommitRequest) -> CommitOutcome
acquire_writer_lease(request) -> AcquireLeaseOutcome
renew_writer_lease(request) -> RenewLeaseOutcome
release_writer_lease(request) -> ReleaseLeaseOutcome
poll_changes(request) -> ChangePollOutcome
```

The commit request contains a filesystem ID, stable mutation identity, full request fingerprint, client incarnation, exact fence, bounded typed preconditions, bounded typed state changes, and an exact retained terminal result.

One atomic durable commit must:

1. Check the mutation ledger before the current lease or fence.
2. Return the recorded result when mutation identity, fingerprint, and client incarnation match.
3. Reject reuse of a mutation ID with different operands or identity.
4. Validate the current fence, record revisions, content base, and all other preconditions.
5. Validate the complete change set and cross-record invariants before mutation.
6. Apply all records, assign one new authoritative revision, retain the result, and append one bounded change event atomically.
7. Acknowledge only after the adapter's advertised durable commit boundary.

A conflict is not a committed result. The future semantic engine rereads authoritative state, repeats authorization, reprepares immutable content when necessary, and constructs a new valid commit request.

## Domain Model Notes

- `InodeId` and `w9pt_fs_storage::FileId` remain distinct strong types. A regular inode stores an explicit content-file identity binding; no undocumented numeric cast connects them.
- A regular-file inode's logical size must match the selected `ContentRef`.
- Directory cookies are allocated stable values, not collection indexes or name hashes.
- Open records are portable and refer to stable IDs rather than SDK or process handles.
- Open pins and orphan lifetime are updated in the same transaction as unlink/release operations.
- Lock ranges use checked half-open or explicit EOF semantics and persist their owner/open identities.
- State request fingerprints cover the whole semantic mutation and are distinct from content-preparation fingerprints.
- Opaque terminal-result bytes remain extensible only when accompanied by an explicit kind/version and strict byte bound.
- Physical SQL rows, etcd keys, or SlateDB keys remain private to adapters.

## Writer Topology

The same semantic contract can support two proven topologies:

- `SerializableMultiWriter`: independent clients may submit concurrent commits; the adapter serializes conflicts and returns typed retry outcomes.
- `SingleFencedWriter`: only one current writer scope may commit; all other writers are rejected through the lease and fence contract.

Both topologies require durable mutation replay, atomic multi-record state changes, and monotonically increasing fences. Topology describes routing and concurrency; it does not weaken correctness.

Expected future mappings:

| Adapter | Expected topology | Notes |
|---|---|---|
| SQLite/local database file | Single fenced writer | Single authoritative server with explicit fsync/WAL configuration |
| PostgreSQL | Serializable multi-writer | Database transactions, durable ledger, lease/fence tables, bounded commit log |
| etcd | Validated per deployment | Only if key/value/transaction/retention bounds satisfy the complete contract |
| SlateDB | Single fenced writer | Object-backed LSM with writer routing, compaction, GC, and durability policy |

## Files Affected

- `Cargo.toml`
- `Cargo.lock`
- `README.md`
- `.dev/project.md`
- `crates/w9pt-fs-state/Cargo.toml`
- `crates/w9pt-fs-state/src/` modules for contract, IDs, records, reads, commits, leases, changes, limits, errors, validation, and trait definition
- `crates/w9pt-fs-state/src/testing/` deterministic clock, memory authority, and conformance support
- `crates/w9pt-fs-state/tests/` domain, atomicity, idempotency, fencing, content, failure, and conformance tests

No `crates/w9pt/**` source change is required.

## Risks and Open Decisions

- A public generic KV transaction API would leak adapter mechanics and permit raceable partial mutations.
- Over-modeling authorization or operation-specific logic in the state crate would duplicate the future filesystem engine.
- Opaque retained results reduce coupling but need a stable version/kind envelope and strict limits.
- Global revision polling requires each adapter to retain a bounded commit log and report compaction gaps explicitly.
- Lease time authority must be explicit at adapter construction; clocks do not provide safety without fencing.
- Referential integrity and link-count rules need a documented split between structural store validation and semantic-engine planning.
- RPITIT uses static dispatch and is not object-safe; a boxed runtime-selected facade is deferred.
- The memory authority cannot prove production durability and therefore must never independently justify advertising `DurableMetadata`.
- Every production adapter needs schema versioning, migration behavior, failure injection, independent-client tests, and the shared conformance suite.

## OpenSpec Integration

Create one new delta domain:

```text
.dev/changes/add-filesystem-state-store/specs/filesystem-state-store/spec.md
```

No current `.dev/specs/` requirement is modified because no archived filesystem-state-store specification exists.
