# Project context

## Identity

- Name: `w9pt`
- License: Apache-2.0
- Stage: research, architecture, and initial Rust crate scaffold

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

## Initial S3 direction

The S3 backend is expected to use:

- an opaque private prefix;
- immutable packed data segments;
- fixed-size logical extents and copy-on-write updates;
- stable inode/directory metadata separate from S3 key names;
- an object-backed transactional metadata structure or explicitly selected metadata service;
- ranged reads, bounded local caches, and read coalescing;
- conditional writes for immutable creation/publication/fencing;
- data-before-metadata flush ordering;
- crash-safe garbage collection and compaction.

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

- The authoritative filesystem-state boundary belongs in a new `w9pt-fs-state` crate that depends on portable `w9pt-storage` content values but not on `w9pt` protocol/session types.
- The public store interface uses consistent bounded reads plus one typed declarative serializable commit rather than generic KV operations or a database transaction callback.
- Authoritative records cover stable inodes and namespace entries, persistent directory cookies, content roots, opens, open-unlinked pins, orphans, locks, xattrs, mutation results, leases, and fencing.
- Mutation replay checks the durable result ledger before current-fence validation, returning an exact recorded result only for an identical fingerprint and client incarnation.
- Writer topology is explicit: future PostgreSQL-style adapters may support serializable multi-writer commits, while SQLite- or SlateDB-style adapters may use one fenced writer without weakening atomicity or durability semantics.
- Clocks, IDs, runtimes, clients, and adapter schemas remain explicit caller/adapter dependencies. No SDK, executor, SQL schema, or target layout belongs in the contract crate.
- The first implementation includes a deterministic memory authority and reusable conformance suite; SQLite, PostgreSQL, etcd, SlateDB, the filesystem engine, and durable session state remain separate proposals.

### PostgreSQL state adapter (`add-postgres-state-adapter`, 2026-09-03)

- The first clustered adapter is `w9pt-fs-state-postgres`, implementing the finalized state-store contract over a caller-owned SQLx `PgPool` and advertising serializable multi-writer topology.
- Implementation is blocked until `add-filesystem-state-store` is approved and complete; adapter schema must follow the finalized public records and conformance API.
- Use SQLx exactly `0.8.6` with default features disabled because the workspace remains on Rust 1.85 and current SQLx 0.9 requires Rust 1.86.
- Support PostgreSQL 15–18 current minors, primary-only authoritative transactions, a fixed fully qualified schema, explicit embedded migrations, and checked full-range unsigned numeric conversion.
- Read batches use one primary `SERIALIZABLE READ ONLY` snapshot; state commits use ledger-first short `SERIALIZABLE READ WRITE` transactions and deterministic semantic record locking.
- A failed `COMMIT` response is potentially committed and is resolved only by bounded retry of the identical mutation through the authoritative result ledger.
- PostgreSQL database time owns lease expiry, while permanently retained monotonically increasing fencing tokens provide safety.
- Version 1 stores normalized filesystem state and inode `ContentRef` fields only. Per-block PostgreSQL mappings, file payloads, session state, replica reads, synchronous-standby durability, and deployment automation remain separate proposals.
