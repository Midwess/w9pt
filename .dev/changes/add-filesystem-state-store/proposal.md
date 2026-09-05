# Add Filesystem State Store Contract

Status: approved

## Summary

Add a runtime-neutral `w9pt-fs-state` crate defining the authoritative filesystem metadata and coordination contract used by future filesystem engines and local or distributed state adapters. The crate will provide strongly typed state records, consistent bounded reads, declarative serializable commits, durable mutation-result replay, writer leases with monotonic fencing, bounded revision polling, a deterministic memory reference implementation, and a reusable conformance suite.

The public contract is filesystem-semantic rather than a generic key/value abstraction. Future SQLite, PostgreSQL, etcd, SlateDB, or other adapters must implement the same normative guarantees without weakening atomic namespace, content publication, idempotency, or fencing behavior.

## Motivation

`w9pt` emits backend-neutral filesystem operations and `w9pt-fs-storage` prepares immutable file content, but the workspace has no authoritative owner for inodes, directories, open-unlinked lifetime, locks, mutation results, or writer fencing. Keeping those records only in a processing node would make correctness depend on node affinity and would prevent a distributed 9P server from recovering safely.

A trait-first state crate establishes the semantic boundary before selecting a database product. It allows:

- a SQLite database file to serve a single authoritative server;
- PostgreSQL to serve independently disposable processing nodes through serializable transactions;
- etcd to be evaluated for bounded small-metadata deployments;
- SlateDB or another object-backed LSM to serve a single-fenced-writer topology;
- deterministic conformance tests to reject adapters that cannot prove the advertised guarantees.

The trait must not be designed to the weakest shared operations of these databases. It must express the atomic filesystem transition required by the caller and require each adapter to map that transition faithfully or reject construction.

## Goals

- Introduce `crates/w9pt-fs-state` without modifying the `w9pt` protocol/session API.
- Depend on `w9pt-fs-storage` for portable `ContentRef` and `PreparedContent` values, but not on `w9pt` wire or session types.
- Define stable filesystem, inode, open, lock, lease, writer, and client identities.
- Define bounded authoritative records for inodes, directory entries and cookies, opens, orphans, locks, xattrs, staged xattrs, mutations, and writer leases.
- Define consistent batched reads at one authoritative revision with bounded point and ordered-range queries.
- Define one declarative commit request containing mutation identity, request fingerprint, client incarnation, exact writer fence, typed preconditions, typed changes, and a retained terminal result.
- Require serializable all-or-nothing multi-record commits and durable acknowledgment.
- Publish `PreparedContent` with inode size, generations, and the mutation result in the same authoritative transaction.
- Return a previously committed result only for an identical retained mutation identity and fingerprint.
- Define idempotent lease operations and monotonically increasing fencing tokens.
- Define bounded revision polling for cache invalidation without making caches authoritative.
- Provide a deterministic memory authority, manual time source, operation tracing, failure injection, and reusable adapter conformance tests.

## Scope

### In scope

- Root workspace membership for `crates/w9pt-fs-state`.
- Rust 2024, Rust 1.85, Apache-2.0 package metadata, documentation lints, and `unsafe_code = "forbid"`.
- Strong IDs, revisions, cookies, fingerprints, fence tokens, bounded names, checked time/duration values, limits, errors, and outcomes.
- Versioned semantic state records with checked constructors and cross-field validation.
- `StateStoreContract` and explicit `WriterTopology::{SerializableMultiWriter, SingleFencedWriter}`.
- Runtime-neutral associated-future methods for reads, commits, writer leases, and revision polling.
- Typed read queries/results and stable ordered scan cursors.
- Typed commit preconditions and state changes rather than public database keys or SQL statements.
- Atomic mutation-result persistence and replay semantics.
- Special prepared-content publication validation against the authoritative inode base.
- Portable open pins, orphan records, byte-range locks, and xattr staging records.
- Deterministic in-memory reference behavior and reusable conformance tests.
- Documentation of requirements for future SQLite, PostgreSQL, etcd, and SlateDB adapters.

### Out of scope

- SQLite, PostgreSQL, etcd, SlateDB, Redis, FoundationDB, or other production adapters.
- SQL schemas, migrations, database clients, connection pools, credentials, SDKs, or deployment configuration.
- A filesystem semantic engine that executes `w9pt::FilesystemOperation`.
- Changes to `w9pt`, 9P messages, fids, tags, effects, completions, or capability negotiation.
- Durable 9P session state, effect inboxes/outboxes, reconnect, or gateway migration.
- Target-object storage, raw/block-split layout changes, packed segments, or S3 adapters.
- Authentication policy, permission evaluation, append-offset selection, or errno mapping.
- Background GC, mutation-ledger compaction, schema migration tooling, or cache implementations.
- A public dynamically dispatched boxed state-store interface in version 1.

## Affected Areas

| Area | Impact |
|---|---|
| `Cargo.toml` / `Cargo.lock` | Add the new workspace crate and its path dependency |
| `crates/w9pt-fs-state` | New authoritative state model, trait, memory reference, and conformance suite |
| `w9pt-fs-storage` | Reuse public content references and preparation identities; no storage-layout change |
| `w9pt` | No source or dependency changes |
| `README.md` | Document the state layer and deferred adapters |
| `.dev/project.md` | Record the new state-store boundary and conventions |

## Dependencies

- The completed `w9pt` Sans-I/O filesystem-operation contract.
- The completed `w9pt-fs-storage` portable `ContentRef`, `PreparedContent`, `FileId`, and mutation identity types.
- Rust 2024 with the workspace Rust 1.85 baseline.
- Caller-provided clocks/time authorities and identities for deterministic lease behavior.
- Future adapter proposals that prove database-specific durability, isolation, failover, transaction-size, lease, fencing, and revision-notification behavior.

## Risks

| Risk | Mitigation |
|---|---|
| A generic CRUD/KV trait permits raceable filesystem mutations | Expose typed consistent reads and one declarative serializable commit |
| State records duplicate future semantic-engine policy | Store validates structure, revisions, fences, and idempotency; the future engine owns authorization and operation meaning |
| A memory implementation hides distributed failure modes | Use factory-based conformance, independent clients, manual time, deterministic schedules, and ambiguous-commit injection |
| Lease expiry is mistaken for fencing | Allocate a monotonic token on every grant and validate the exact token on every commit |
| Mutation replay grows without bound | Persist an explicit retention horizon and defer deletion until a caller-proven safe horizon |
| Change-log compaction silently leaves stale caches | Return an explicit compacted-revision outcome requiring cache discard and authoritative reload |
| Adapters claim guarantees they do not provide | Validate `StateStoreContract` and require the shared conformance suite for every writable adapter |
| Large scans or transaction plans amplify resources | Validate every count, name, result, range, encoded length, and aggregate transaction size before allocation or execution |
| SlateDB single-writer behavior is hidden behind a multi-writer-looking API | Persist and expose writer topology while keeping atomicity and fencing requirements normative |
| Database schemas diverge from the public model | Require versioned adapter schemas and exact lossless model round trips in adapter-specific conformance |
| AGPL implementation expression contaminates the project | Use public architectural evidence only and independently implement all code, tests, schemas, formats, and comments |
