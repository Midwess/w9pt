# w9pt Repository Guidance

## Purpose

`w9pt` is the foundation for a distributed 9P server cluster. The target system supports stateful 9P connections while keeping processing nodes disposable and free of authoritative local state.

The system is stateful even when its compute nodes are stateless. Every piece of state required for correctness must have an explicit owner in the connection/session layer, the authoritative metadata database, or immutable backend storage.

## Primary Architecture

```text
9P client
  -> transport gateway / 9P I/O source
  -> 9P session processor
  -> authoritative filesystem metadata and coordination database
  -> immutable file-content repository
  -> S3-compatible or other target object storage
```

The layers have distinct responsibilities:

### Transport gateway / 9P I/O source

- Owns the live TCP, Unix socket, WebSocket, virtio, or other transport connection.
- Performs framing, bounded buffering, backpressure, authentication handoff, and ordered response delivery.
- For stream transports, assembles complete 9P frames before routing them to a stateless processor.
- May retain ephemeral socket and partial-frame state, but no authoritative filesystem state.
- Cannot make a stock 9P connection transparently survive loss of the gateway itself. Reconnectable sessions require a private protocol/client recovery design.

### 9P session processor

- Negotiates the dialect and `msize` and enforces tags, fids, flush ordering, capability checks, and response encoding.
- Treats each accepted input or completion as a versioned session-state transition.
- Routes effects and completions by a globally unambiguous session incarnation and operation identity.
- Never owns authoritative file content, inode metadata, locks, or open-unlinked lifetime solely in process memory.

A stateful connection needs session state somewhere. The target distributed design stores or replicates the state required to resume processing:

- negotiated dialect and `msize`;
- fid, auth, open, and xattr-handle bindings;
- principal, export, root, and capability context;
- active tags and monotonic operation allocation;
- pending continuations, flush dependencies, cancellation state, and retained limits;
- sequenced effect and response outboxes;
- session incarnation, owner lease, and fencing epoch.

If an initial deployment keeps `Session` only in memory, the connection must remain pinned to that session actor and node loss ends the session. Call this connection-affine or durability-stateless operation, not transparent session migration.

### Filesystem metadata and coordination database

The metadata database is the authoritative control plane. It stores or transactionally coordinates:

- stable filesystem and inode identities;
- directory entries and readdir cookies;
- file kind, mode, owner, group, timestamps, size, link count, and generations;
- the immutable `ContentRef` selecting a file's current content;
- orphan records and open-unlinked pins;
- open-state records that must survive or move between nodes;
- byte-range locks and fencing tokens;
- append-position serialization;
- xattr metadata and staging references;
- mutation IDs, request fingerprints, committed results, and retention horizons;
- writer/session epochs, leases, and ownership fences;
- GC roots and any retained snapshot or reader protection.

The database contract, not a product name, is normative. A writable clustered adapter must provide:

- strongly consistent or linearizable reads for authoritative revisions;
- atomic compare-and-swap;
- serializable multi-key transactions for namespace and inode mutations;
- durable acknowledgment suitable for the advertised `fsync` guarantees;
- leases plus monotonically increasing fencing tokens where live ownership expires;
- change notification or revision-based cache invalidation;
- bounded keys, values, transactions, and retry behavior.

etcd or another consensus-backed transactional store can satisfy much of this contract when metadata remains small and its workload is validated. Redis may be used for caches or explicitly disposable state, but must not be the default authoritative metadata store unless the selected deployment proves the required transaction, durability, failover, non-eviction, and fencing guarantees. Do not design to the weakest common denominator of several databases; expose a filesystem-semantic metadata interface and validate each adapter's guarantees.

### Immutable content repository and target storage

S3-compatible storage is the bulk-data plane. It contains immutable file payloads, blocks, and manifests under a private prefix. It does not contain a visible `path -> object key` namespace.

The first storage methods are:

- `Raw`: one immutable object containing the complete logical file;
- `BlockSplit`: sparse, file-relative 32 KiB logical blocks with immutable payload objects.

Every persisted file manifest records its storage method and method parameters. Configuration selects the default for newly created files and never reinterprets an existing manifest.

For block-split content:

- logical blocks are scoped by file ID and block index;
- logical EOF comes from metadata, never physical padding;
- a materialized version-1 block decodes to exactly 32 KiB;
- the final block is zero-padded before hashing or storage;
- missing or all-zero blocks are sparse holes;
- full-block overwrites do not read old content;
- partial-block writes perform verified read-modify-write;
- shrink zeroes the discarded tail of the final retained block;
- stored-data hash mismatch is corruption, never an instruction to update metadata.

`w9pt-storage` prepares immutable payloads and returns a portable, self-validating `PreparedContent`/`ContentRef`. In clustered filesystem operation, it must not independently publish a mutable per-file S3 head as a second authority. The filesystem metadata transaction is the sole publisher of current content.

## Canonical Read Flow

```text
1. Accept a complete, ordered 9P request for one session incarnation.
2. Resolve and validate the fid/open state and attached principal/export.
3. Read a consistent inode record and immutable ContentRef from the metadata DB.
4. Fetch the referenced raw payload, manifest, or blocks from target storage.
5. Verify persisted format, lengths, identities, and content hashes.
6. Return only bytes within the requested positioned range and logical EOF.
7. Persist/queue the session transition and ordered response before delivery.
```

Because content references and payloads are immutable, a reader observes one complete old or new content version, never a mixture created by a concurrent writer.

## Canonical Write and Truncate Flow

```text
1. Resolve the session, fid/open state, principal, export, and logical operation ID.
2. Read the inode, permissions, current ContentRef, and authoritative metadata revision.
3. Prepare new immutable content objects in target storage.
4. Prepare and durably store the immutable file-content manifest.
5. Begin one metadata transaction.
6. Revalidate authorization, inode/base ContentRef, writer/session fencing, and operation identity.
7. Atomically publish ContentRef, size, timestamps, inode/data generation, and mutation result.
8. Commit the metadata transaction durably.
9. Persist/queue the exact terminal session completion and response.
10. Send the 9P response in session order.
```

The non-negotiable dependency order is:

```text
immutable target data
  -> immutable target manifest
  -> authoritative metadata DB transaction
  -> durable operation/session result
  -> 9P response
```

There is intentionally no distributed transaction spanning S3 and the metadata database. Failure before metadata publication may leave unreachable immutable objects for later GC, but it must never expose incomplete file content. Metadata must never reference a target object that was not already acknowledged durable.

On a metadata revision conflict, re-read the current inode and base content, revalidate the complete semantic operation, and prepare again if necessary. Never blindly last-write-wins a partial write, append, truncate, rename, permission change, or link-count mutation.

## Namespace and Concurrency Semantics

The metadata transaction owns filesystem atomicity:

- `create`: allocate the inode and insert the directory entry together;
- `rename`: update source/destination directories, parent metadata, overwritten targets, and link counts together;
- `link`/`unlink`: update directory entries, link counts, orphan state, and timestamps together;
- `setattr`: validate and apply all selected fields as one mutation;
- `write`/`truncate`: publish content, size, times, and data generation together;
- append: determine EOF and publish the write within one serialized semantic operation;
- lock operations: use shared ownership records and fencing, not process-local mutexes alone.

Permission and policy checks that protect a mutation must be revalidated against the same database revision or inside the same transaction that commits it.

## Idempotency, Fencing, and Failure Recovery

Every retryable mutation needs a globally stable identity distinct from a reusable 9P tag. Persist at least:

```text
filesystem ID
mutation ID
request fingerprint
session/client incarnation
writer epoch
terminal result or committed metadata revision
retention horizon
```

A delayed retry returns the recorded result only when its fingerprint matches. Reusing one mutation ID for different bytes or operands is a hard error.

Immutable preparation identities must be collision-safe across nodes. Bind them to the filesystem/file ID, mutation ID, base content identity, and logical operation fingerprint, or use an equivalently safe content identity. Do not use a process-local retry counter as the only discriminator. When `put_if_absent` reports an existing object, fetch or otherwise verify exact identity before treating it as the same preparation.

Leases are not fences by themselves. Every owner capable of mutation must carry a monotonically increasing epoch/fencing token, and every authoritative commit must reject a stale token.

Session input, effect dispatch, completion, and response emission require durable or replicated inbox/outbox behavior if any node may resume the session. A node crash must not lose whether an operation was dispatched, committed, completed, or replied.

## Handle and Cache Rules

- `ObjectHandle`, `OpenHandle`, `XattrHandle`, and `AuthHandle` must be cluster-resolvable identifiers or keys into shared leased state when work can move between nodes.
- Never encode raw pointers, process-local indexes, SDK handles, or cache addresses into portable handles.
- Immutable content may be cached by immutable key plus verified digest.
- Mutable metadata, session records, leases, and root references must be revalidated by database revision.
- Local caches are always optional and rebuildable. No correctness property may depend on node affinity or cache survival.
- GC must use durable reachability, epochs, leases, or retained roots; it must not assume no other node is reading an old immutable object.

## Crate Boundaries

### `w9pt`

- Remains a runtime-free Sans-I/O 9P protocol and session core.
- Contains no database client, S3 SDK, transport, executor, or storage-layout implementation.
- Emits high-level filesystem/policy effects and accepts exact terminal completions.
- Never exposes target object keys, blocks, manifests, transactions, or database details on the wire.

### `w9pt-storage`

- Implements backend-neutral raw and block-split content preparation and reads.
- Is generic over a target-object interface and does not depend on a particular S3 SDK.
- Returns portable immutable content references for publication by the filesystem metadata layer.
- May include an object-head publisher for standalone tests or non-clustered use, but that publisher is not authoritative in clustered filesystem mode.

### Future filesystem metadata/runtime crates

- Implement inode/directory semantics, transactions, authorization, locks, opens, orphan handling, idempotency, fencing, and capability selection.
- Adapt one or more strongly consistent databases without weakening the required semantics.
- Route stateful session transitions and filesystem work across disposable nodes.

### Target adapters

- Implement S3-compatible, local, memory, or future target stores.
- State and test their exact durability, consistency, conditional-write, range-read, size, and retry guarantees.
- Never cause the core crates to depend on an SDK or async runtime.

## Capability Rules

Capabilities are promises, not feature flags. Advertise a capability only when every layer provides its complete semantics.

- `PositionedIo` requires exact offset behavior under concurrency.
- `AtomicSetattr` requires content and selected inode attributes to publish together.
- `AtomicNamespace` requires transactional directory/inode updates.
- `DurableData` requires all referenced content to be durable.
- `DurableMetadata` requires the authoritative metadata commit to be durable.
- `OpenUnlinked` requires shared orphan/open-pin coordination.
- `CrossSessionLocks` requires cluster-wide lock ownership and fencing.
- `Cancellation` remains best effort and never promises rollback after commit.

If a backend or deployment cannot prove a required guarantee, omit the capability and return the documented unsupported error.

## Implementation Rules

1. Keep protocol, filesystem semantics, metadata coordination, content layout, and target SDKs in separate layers.
2. Use immutable S3 objects; publish current state only through the authoritative metadata transaction.
3. Do not use visible paths as target object keys.
4. Do not put bulk file content in etcd, Redis, or another metadata database.
5. Do not make Redis or a local cache authoritative merely because it is fast.
6. Do not add a second per-file publication authority beside the inode metadata record/root.
7. Do not acknowledge a mutation before immutable dependencies and authoritative metadata are durable according to advertised semantics.
8. Use checked arithmetic before every allocation, range calculation, index conversion, and encoded-length calculation.
9. Bound requests, retries, manifests, blocks, session state, queues, transactions, locks, and retained mutation results.
10. Accept clocks, random IDs, credentials, runtimes, and clients through explicit caller-owned interfaces.
11. Preserve one terminal completion per emitted operation even when cancellation or node failure races with commitment.
12. Keep all persistent formats versioned, bounded, canonical, and independently testable.
13. Do not copy, translate, or closely adapt AGPL ZeroFS implementation code, tests, comments, formats, or distinctive structure.

## Required Testing

Every implementation layer must include deterministic tests for its failure boundaries.

- Compare raw and block-split operations against a byte-vector model.
- Test aligned, unaligned, sparse, EOF, overflow, shrink, re-extension, and corruption cases.
- Test two independent repository/metadata clients with no shared RAM.
- Inject failure before and after content upload, manifest upload, metadata commit, result-ledger commit, effect dispatch, completion, and response enqueue.
- Prove recovery exposes either the complete old or complete new state, never a mixture.
- Test concurrent overlapping writes, disjoint writes, append, truncate, rename, link/unlink, and permission changes.
- Test stale writer/session epochs and expired leases.
- Test duplicate mutation IDs with matching and mismatching fingerprints.
- Recreate nodes from empty local state and prove authoritative behavior is unchanged.
- Run adapter conformance tests against each supported database and target store.

## Project References

- `.dev/project.md` defines the wider project scope and ownership rules.
- `.dev/changes/complete-sans-io-core/` documents the implemented protocol/session foundation.
- `.dev/changes/add-storage-methods/` is the draft proposal for raw and block-split content storage.
- `.dev/research/9p-s3-filesystem.md` and `.dev/research/steampipe-content-system.md` are research evidence, not normative requirements.

