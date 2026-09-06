# Project context

## Identity

- Name: `w9pt`
- License: Apache-2.0
- Stage: active unreleased development; APIs and persisted formats are not compatibility-stable

## Development Compatibility Policy

- No released version or supported persistent deployment exists yet.
- Rust APIs, crate names, configuration, database schemas, private object keys,
  fingerprints, and persistent encodings may be changed in place.
- Do not add legacy aliases, dual readers/writers, old-schema detection/import,
  compatibility shims, deprecation periods, or migration paths for development
  builds.
- After an incompatible change, rebuild dependents and recreate development
  databases and private object prefixes from empty state.
- Version tags, checksums, and migration ledgers validate the current build's
  data; they do not promise acceptance of data written by earlier builds.
- Backward compatibility begins only after an explicit release proposal defines
  the supported API and persistent-format baseline.

## Purpose

`w9pt` is an embeddable library/framework that translates 9P filesystem requests into operations on pluggable storage backends. The first backend is S3-compatible object storage.

The framework should let a host application expose a backend through 9P without adopting an opinionated daemon, CLI, deployment topology, or process model.

## Scope

The core project owns:

- a Sans-I/O state-machine API that consumes input/completion events and emits typed effects;
- 9P2000.L framing, negotiation, request dispatch, tags, fids, cancellation, and errors;
- connection/session state as embeddable library objects;
- backend-neutral filesystem semantics such as stable inodes, directories, attributes, random I/O, links, rename, truncate, sparse ranges, locks, and durability barriers;
- capability discovery and explicit unsupported-operation behavior;
- backend interfaces and reusable conformance tests;
- an S3 backend implementing the required persistent metadata and data model.

The embedding application owns:

- process startup and shutdown;
- listener and transport selection;
- authentication and principal mapping;
- authorization/export policy supplied to the framework;
- configuration source and secret management;
- task scheduling/runtime integration;
- metrics, tracing, logs, and operational control planes;
- deployment, load balancing, and high-availability topology.

## Non-goals

- Shipping an opinionated standalone filesystem service like ZeroFS.
- Requiring a specific async runtime, network listener, WebSocket server, CLI, configuration file, or cache directory in the core API.
- Performing network, filesystem, object-store, clock, thread, or executor I/O inside the core crate.
- Treating a thin path-to-object adapter as a full filesystem.
- Copying AGPL implementation code into the Apache-2.0 project.
- Promising all Linux/POSIX operations in the first release.

## Architectural boundaries

```text
host application
    │ supplies input events and completes emitted effects
    ▼
w9pt Sans-I/O core
    ├── 9P codec and session state
    ├── filesystem semantic state
    └── typed transport/storage effects
    │
    ├──────────────┬─────────────────┐
    ▼              ▼                 ▼
transport      S3 backend       future backends
adapter        adapter
```

The internal logical layering remains:

```text
9P protocol/session layer
    │ backend-neutral semantic requests
    ▼
filesystem semantic core
    │ transactional storage operations
    ▼
storage backend interface
    ├── S3-compatible object storage (first)
    └── future backends
```

Rules:

1. Protocol code must not contain S3-specific behavior.
2. Backend code must not parse or emit 9P messages.
3. Filesystem semantics belong in the semantic core, not in transport adapters.
4. Host-runtime concerns enter through interfaces rather than global configuration.
5. Backends advertise capabilities; the semantic core must not invent stronger guarantees.
6. Durable metadata must never reference unavailable data.
7. Unsupported semantics fail explicitly.
8. Correctness and crash recovery take priority over cache and throughput optimization.
9. The core must not depend on Tokio, async-std, smol, a WebSocket implementation, or an S3 SDK.
10. Time, randomness, authentication decisions, and cancellation outcomes enter as caller-supplied values/events.

## S3 direction

The first concrete target adapter uses an opaque private prefix, immutable raw
or sparse 32 KiB block-split content objects, exact range reads, and conditional
single-key writes. Stable inode/directory metadata and current `ContentRef`
publication remain in the authoritative state store rather than S3 keys.
Packing, bounded caches, read coalescing, garbage collection, and compaction are
future optimization/maintenance proposals.

Direct external mutation of the private S3 prefix is out of scope. Import/export interoperability can be layered separately.

## Library-first API direction

The eventual API should allow an application to:

1. construct a backend;
2. construct the filesystem semantic core over that backend;
3. configure dialect/capability and policy hooks;
4. create independent 9P session objects for accepted connections;
5. feed received bytes or complete binary frames into a session;
6. poll typed effects until the session becomes quiescent;
7. execute those effects using application-owned transports, storage clients, clocks, and executors;
8. return effect completions to the session in any permitted order;
9. flush and shut down explicitly.

No mandatory background process or singleton global state should be required.

## Standard Rust interoperability

- Optional blocking handle adapters should implement `std::io::Read`, `Write`, and `Seek` so generic stream-oriented libraries can consume w9pt files.
- Optional async adapters may implement Tokio/futures I/O traits outside the core crate.
- The project should expose its own path, metadata, directory-entry, open-options, and filesystem APIs because `std::fs` has no provider trait.
- Transparent calls through concrete `std::fs` functions require an operating-system mount adapter such as FUSE or a native 9P mount.
- The core must not fabricate `std::fs::File` or raw file descriptors for in-process objects.

## Research sources

Research notes under `.dev/research/` are exploratory evidence, not product requirements. Convert accepted decisions into `.dev/specs/` or an approved change proposal before implementation.

## Latest Analysis

### Filesystem state store contract (`add-filesystem-state-store`, 2026-09-02)

- The authoritative filesystem-state boundary belongs in `w9pt-fs-state`, which depends on portable `w9pt-fs-storage` content values but not on `w9pt` protocol/session types.
- The public store interface uses consistent bounded reads plus one typed declarative serializable commit rather than generic KV operations or a database transaction callback.
- Authoritative records cover stable inodes and namespace entries, persistent directory cookies, content roots, opens, open-unlinked pins, orphans, locks, xattrs, mutation results, leases, and fencing.
- Mutation replay checks the durable result ledger before adapter-limit/current-fence validation and uses the finalized mutation ID, fingerprint, client incarnation, and retention identity to return the exact recorded result.
- Writer topology is explicit: future PostgreSQL-style adapters may support serializable multi-writer commits, while SQLite- or SlateDB-style adapters may use one fenced writer without weakening atomicity or durability semantics.
- Clocks, IDs, runtimes, clients, and adapter schemas remain explicit caller/adapter dependencies. No SDK, executor, SQL schema, or target layout belongs in the contract crate.
- The first implementation includes a deterministic memory authority and reusable conformance suite; the PostgreSQL adapter is implemented separately, while SQLite, etcd, SlateDB, the filesystem engine, and durable session state remain future proposals.

### PostgreSQL state adapter (`add-postgres-state-adapter`; SeaORM/public code-first revisions 2026-09-05)

- The first clustered adapter is `w9pt-fs-state-postgres`, implementing the finalized state-store contract over a caller-owned SeaORM `DatabaseConnection` and advertising serializable multi-writer topology.
- Callers construct connections and own DSNs, credentials, TLS, pool sizing, routing, and Tokio lifecycle; the adapter rejects disconnected or non-PostgreSQL connections.
- Pin SeaORM exactly `1.1.20` with default features disabled and `sqlx-postgres` plus `runtime-tokio`; this originally preserved Rust 1.85 and remains compatible after the explicitly approved workspace move to Rust 1.94.1. The adapter has no direct SQLx dependency, imports, types, or test APIs, while SeaORM's PostgreSQL backend remains transitively SQLx 0.8-backed.
- Support PostgreSQL 15–18 current minors, primary-only authoritative transactions, fixed fully qualified `public.w9pt_fs_state_*` relations, CLI-scaffolded code-first migrations, and checked full-range unsigned numeric conversion.
- A private per-filesystem authority head owns the revision-one empty baseline and revision allocation independently of the optional public `FilesystemRecord`, allowing lease acquisition before filesystem bootstrap.
- Read batches use one SeaORM `SERIALIZABLE READ ONLY` transaction and every finalized keyset cursor; state commits use a short primary serializable fixed-size ledger probe before adapter preflight, then ledger-first short `SERIALIZABLE READ WRITE` transactions and deterministic semantic record locking.
- A failed `COMMIT` response is potentially committed and is resolved only by bounded retry of the identical mutation through the authoritative result ledger.
- Version-1 lease ticks are exact Unix-epoch microseconds stored as `NUMERIC(20,0)`; production time is captured once from PostgreSQL per transaction, deterministic conformance uses a test-only database clock row, and permanently retained fencing tokens provide safety.
- Change polling returns the finalized `ChangePollOutcome` variants and `ChangeBatch::{next,current_revision}` semantics without an adapter-specific `has_more` field.
- Exact SeaORM CLI 1.1.20 supplied database-introspection evidence and the initial `MigrationTrait` scaffold. SeaQuery builders define tables, columns, keys, and indexes; reviewed PostgreSQL DDL retains named checks and deferred foreign keys that SeaQuery 0.32 cannot model.
- Explicit migrations validate a bounded custom source-checksum ledger and invoke code-first migrations inside one bounded `READ COMMITTED READ WRITE` SeaORM transaction protected by the fixed `pg_advisory_xact_lock`; schema changes and the migration ledger commit or roll back together regardless of caller session defaults.
- Earlier development SQL layouts are unsupported and have no detection, import,
  relocation, upgrade, or dual-schema behavior.
- Adapter conformance runs through independently constructed SeaORM connections and is required on PostgreSQL 15, 16, 17, and 18.
- The current unreleased version-1 schema has 17 prefixed tables, 188 named
  constraints, and 27 indexes. It may be replaced directly during
  development; checksum drift fails closed and requires an operator-driven reset,
  with no compatibility migration. Per-block PostgreSQL mappings, file payloads,
  session state, replica reads, synchronous-standby durability, and deployment
  automation remain separate proposals.

### S3 target adapter (`add-s3-target-store`, 2026-09-05)

- `w9pt-fs-storage-s3` implements the backend-neutral `TargetStore` over a
  caller-created AWS SDK S3 client; no AWS, Tokio, HTTP, TLS, credential, or S3
  dependency enters the protocol, semantic, state, or content-layout crates.
- The security-gate decision raised the workspace MSRV to Rust 1.94.1 and pins
  `aws-sdk-s3` exactly at 1.145.0 with the current default HTTPS client and
  retry-disabled conditional mutations.
- The supported writable profile is an Amazon S3 general-purpose bucket.
  Compatible providers fail closed until their exact deployment passes the full
  qualification suite.
- Construction produces an unqualified target with no writable guarantees; two
  independently configured clients must pass the checked live target and
  concurrency probes before `TargetGuarantees::REQUIRED` is advertised.
- Complete reads use HEAD plus `GET If-Match`; ranges require exact `206` and
  `Content-Range`; ETags are bounded opaque concurrency tokens rather than
  content hashes.
- Immutable and mutable conditional writes preserve response-loss ambiguity for
  exact upper-layer readback. Clustered filesystem metadata remains the sole
  publisher of current `ContentRef`.
- The host owns credentials, region, endpoint/addressing, verified TLS/SigV4,
  Tokio runtime, bucket provisioning, IAM, lifecycle, versioning, cost, and
  deployment policy.

### SeaweedFS repository integration (`add-seaweedfs-storage-integration-tests`, 2026-09-06)

- The digest-pinned SeaweedFS 4.42 Compose job builds two independent S3
  clients, runs target behavior probes, then creates a private external-test
  guarantee wrapper solely for `ContentRepository` composition.
- The live matrix covers Raw and paged BlockSplit lifecycles across actual leaf
  and branch boundaries, sparse/zero blocks, boundary reads and writes, truncate/
  re-extension, immutable reuse, independent reopen, abandoned preparations,
  discarded publication results, and stale-CAS/reprepare ordering.
- Ordinary SeaweedFS targets remain unqualified with `TargetGuarantees::NONE`,
  and the compatible-provider profile remains explicitly unsupported.
- Results are behavioral evidence only. The single-node tmpfs job makes no
  durability, restart, response-loss, multi-node, TLS/SigV4, lifecycle, or
  production-support claim; deterministic SDK replay remains authoritative for
  transport ambiguity.

### Paged BlockSplit manifests (`add-paged-block-manifests`, 2026-09-06)

- Storage format v2 introduced one compact
  manifest root and immutable sparse 128-way mapping pages, up to seven levels.
- Range reads retain a bounded active path and verify only accessed pages and
  payloads; root validation and content sync do not perform a whole-tree scrub.
- Create, write, and truncate use bounded read-only preflight followed by
  child-before-parent immutable preparation, reusing untouched subtrees exactly.
- Shrink prunes complete suffix subtrees by authenticated summaries and root
  normalization; re-extension cannot resolve detached data through listing.
- Limits separately cover compact roots, page bytes, materialized count, page
  work, and operation-local mapping memory. They are not process RSS or global
  admission guarantees. PostgreSQL continues to publish the same bounded
  `ContentRef`; mapping rows are not added.
- The paged tree remains the current layout inside v3 representation objects.

### Content compression and encryption (`add-content-compression-encryption`, 2026-09-06)

- Current storage format v3 separates Raw/BlockSplit layout from actual Identity/LZ4 payload encoding and None/AES-256-SIV object protection. LZ4 uses a fixed block profile and a deterministic 64-byte savings threshold.
- Each managed file has one authoritative bounded context record containing owner/file/context identity, exact opaque policy, an optional DEK commitment, and an optional wrapped per-file DEK. State and PostgreSQL do not parse algorithms or retain plaintext keys.
- Callers own secure entropy and one externally bootstrapped master KEK. Only the durable winning context is unwrapped for S3 preparation; changed defaults and losing create candidates cannot reinterpret or rekey an existing file.
- Protected names use DEK-derived tokens. Payloads, mapping pages, and manifests authenticate exact keys, provenance, policy, context, lengths, and canonical digests. Rewrap changes only PostgreSQL envelope bytes and preserves the DEK, S3 objects, names, and `ContentRef`.
- Context rows remain after inode retirement until a future reachability-aware key-GC contract exists. Operators must retain old masters until every referenced envelope is explicitly rewrapped; deletion does not imply cryptographic erasure from WAL, backups, process memory, or storage history.
- The default representation scratch budget is 64 MiB per operation and combines with existing request, Raw, page, map-frontier, target-object, and retry limits. No global plaintext file-key cache is introduced.
- V1/v2 data is incompatible. Operators recreate the development database and private object prefix; there is no legacy reader, schema upgrade, dual writer, or automatic re-encryption path.

### WebSocket 9P integration profile (`add-websocket-9p-integration-profile`, 2026-09-06)

- The standalone test workspace includes sibling TCP and HTTP/WebSocket
  application profiles over the same minimal SeaweedFS-backed fixture; no
  runtime or transport dependency enters a root workspace crate.
- The HTTP server exposes `GET /healthz` and upgrades `/9p` only for clients
  offering the `9p` WebSocket subprotocol.
- Each bounded binary WebSocket message maps to one complete
  `Session::receive_frame` input and each `SendFrame` effect maps to one binary
  response message. Text and malformed frame inputs close deterministically.
- Every upgraded connection owns an independent checked `SessionId` and
  in-memory `Session`; gateway loss ends the connection and no migration or
  recovery guarantee is claimed.
- The required SeaweedFS job executes the same 9P2000.L file lifecycle through
  TCP and WebSocket, then verifies the payload directly through S3. HTTP/WSS
  deployment policy, TLS, authentication, Origin checks, proxies, persistence,
  and production gateway behavior remain future work.
