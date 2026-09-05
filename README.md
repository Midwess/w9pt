# w9pt

An embeddable 9P filesystem framework for pluggable storage backends.

`w9pt` translates stock `9P2000.L` requests into owned, backend-neutral filesystem and policy effects. `w9pt-fs-storage` prepares immutable file content for caller-provided object stores, and `w9pt-fs-state` defines the authoritative metadata, transaction, mutation-replay, lease, fencing, and cache-invalidation contract that publishes those references with inode and namespace state.

## Status

> **Active development:** `w9pt` is unreleased and has no backward-compatibility
> guarantee. Rust APIs, crate boundaries, configuration, PostgreSQL schemas,
> private object keys, fingerprints, and persisted formats may change directly.
> Old development builds and their data are unsupported: there are no legacy
> aliases, readers, or migration paths. Rebuild dependents and recreate
> development databases/object prefixes after an incompatible change.

The Sans-I/O core implements framing, negotiation, the declared base/Linux operation matrix, tags, fids, out-of-order completions, cancellation, explicit shutdown, capabilities, and stable Linux errno replies. The storage crate implements bounded `raw` and sparse 32 KiB `block-split` content layouts over a runtime-neutral target contract. `w9pt-fs-storage-s3` implements that target contract for caller-configured Amazon S3 general-purpose buckets. The state crate provides checked portable records, bounded consistent reads, declarative serializable commits, deterministic memory authority, and reusable adapter conformance. `w9pt-fs-state-postgres` implements that contract on PostgreSQL 15–18. Transport, filesystem semantic-engine integration, durable session state, and production SQLite/etcd/SlateDB adapters remain future work.

## Goals

- Make the `w9pt` crate a Sans-I/O protocol and filesystem core with no sockets, async runtime, threads, or storage SDK.
- Provide a library-first 9P server framework that applications embed in their own process.
- Keep transports replaceable: TCP, Unix sockets, WebSocket, virtio, or application-provided byte/message streams.
- Separate 9P framing and session state from filesystem semantics and persistent storage.
- Define a backend contract that can support S3 first and other storage systems later.
- Preserve real filesystem behavior—stable object identity, random I/O, atomic namespace operations, metadata, locks, and durability—where backend capabilities allow it.
- Negotiate capabilities and return explicit unsupported-operation errors instead of silently weakening semantics.

## Non-goals

- An opinionated standalone daemon, appliance, or control-plane runtime like ZeroFS.
- A mandatory CLI, configuration-file format, listener, scheduler, cache directory, or deployment topology.
- A direct `file path = S3 object key` mapping presented as a full filesystem.
- Claiming complete POSIX/Linux compatibility before the operation and failure semantics are tested.

## Architecture

```text
embedding application
  ├── feeds transport bytes/messages into w9pt
  ├── executes storage effects requested by w9pt
  └── sends frames emitted by w9pt
                  │ events/completions
                  ▼
          w9pt Sans-I/O core crate
       9P codec + session state machine
       filesystem semantics + typed effects
                  │ effects/results
          ┌───────┴──────────┐
          ▼                  ▼
 transport adapters    backend adapters
 TCP/Unix/WebSocket    S3 first, then others
```

The core performs no I/O. It consumes input and completion events, then emits transport frames, storage requests, cancellation requests, and lifecycle events for the embedding application to execute.

The host application decides whether a session is driven synchronously, by Tokio or another executor, through WebSocket, or behind an OS filesystem adapter. `w9pt` does not require a particular executable architecture.

## Driving a session

Create one independently owned `Session` per connection. The host supplies a stable `SessionId`, feeds arbitrary stream chunks with `receive_bytes` or one owned frame with `receive_frame`, and polls effects until quiescent.

```rust,no_run
use w9pt::{Effect, Session, SessionConfig, SessionContext, SessionId};

let mut session = Session::new(
    SessionConfig::default(),
    SessionContext::new(SessionId::new(42)),
)?;

session.receive_bytes(&[/* bytes supplied by the transport */])?;
while let Some(effect) = session.poll_effect() {
    match effect {
        Effect::SendFrame { bytes } => {
            // Write this complete frame after every earlier SendFrame from this session.
            let _ = bytes;
        }
        Effect::Filesystem { operation_id, request } => {
            // Execute owned semantic work, then call session.complete(...) exactly once.
            let _ = (operation_id, request);
        }
        Effect::Policy { operation_id, request } => {
            // Resolve authentication/export policy, then complete exactly once.
            let _ = (operation_id, request);
        }
        Effect::Cancel { operation_id, kind } => {
            // Best effort only. The original operation still needs one terminal completion.
            let _ = (operation_id, kind);
        }
        Effect::CloseSession { reason } => {
            // Stop transport input/output as appropriate for the host.
            let _ = reason;
        }
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Effects own all their payloads. A host may schedule filesystem and policy work in any order and may return unrelated completions in any order. Route work globally by `(SessionId, OperationId)` and return each emitted filesystem/policy operation exactly one matching terminal result or terminal cancellation. Unknown, duplicate, stale, and wrong-kind completions are errors and do not mutate another request.

`SendFrame` effects are complete response frames. They are emitted in the only transport order the host must preserve for that session; unrelated tags may finish in completion order rather than request-arrival order.

The standalone [TCP SeaweedFS integration application](test/README.md) demonstrates
this boundary with a raw 9P client: TCP bytes enter `Session`, policy/filesystem
effects are forwarded to application code, file bytes are handled through the
SeaweedFS S3 API, and completions produce TCP response frames. It is a minimal
development fixture with an in-memory namespace, not the production semantic
engine. The same Docker Compose stack runs the PostgreSQL adapter's live
migration and state-store conformance suite independently. Before the TCP test,
the stack also runs digest-pinned SeaweedFS target probes and a private
compatibility wrapper through the full Raw/BlockSplit repository lifecycle and
standalone publication-boundary matrix; that evidence does not qualify
SeaweedFS for production use.

## Cancellation and shutdown

`Tflush` suppresses the old protocol reply and requests best-effort cancellation. `Rflush` waits for the old external work to become terminal, ensuring no old reply follows it. A successful mutation returned after a flush may already be committed: flush is never transaction rollback.

Call `Session::begin_close` when the host closes a connection. New ordinary input is rejected; the session emits cancellation and fid-release work. Continue polling and completing it until `SessionStatus::Drained`. `Drop` performs no hidden I/O. Dropping before drain is an explicit choice to abandon remaining backend live state.

## Export capabilities

Attach policy returns the principal, export ID, root object/QID, and `CapabilitySet`. Capabilities are enforced promises, not hints. Operations are rejected with `EOPNOTSUPP` before backend work unless the export declares both the operation and required guarantees such as atomic authorization/namespace mutation, stable identity, positioned I/O, open-unlinked lifetime, durability, xattrs, or cross-session locks.

The backend must coordinate stable identities, namespace transactions, open handles, permissions, and locks across independently owned sessions. Protocol code never exposes S3 keys, extents, transactions, or other storage-engine details.

## Supported protocol surface

The core intentionally handles `version`, `auth`, `attach`, `flush`, `walk`, `clunk`, `lopen`, `lcreate`, `mkdir`, `mknod`, `symlink`, `read`, `write`, `readdir`, `fsync`, `statfs`, `getattr`, `setattr`, `readlink`, `rename`, `renameat`, `remove`, `unlinkat`, `link`, `xattrwalk`, `xattrcreate`, `lock`, and `getlock`, plus `Rlerror`.

Legacy dialect negotiation, obsolete base `open`/`create`/`stat`/`wstat`, private reconnect/session migration, universal POSIX emulation, transports, runtimes, clocks, authentication mechanisms, and storage implementations are intentionally outside this crate.

## Rust I/O compatibility

Separate adapters can expose opened w9pt files through `std::io::Read`, `Write`, and `Seek`, allowing generic byte-stream libraries to work normally. Tokio/futures traits can be implemented in optional runtime crates.

Concrete `std::fs` functions are not pluggable: `std::fs::File`, `Metadata`, and `ReadDir` represent the host operating system filesystem. Code using `std::fs::read("path")` can access w9pt only after a FUSE/9P-style OS mount. In-process users will use w9pt's own filesystem API and optional `std::io` handle adapters.

## Immutable content repository

`w9pt-fs-storage` is generic over bounded exact object reads, atomic immutable creation, and opaque-version compare-and-swap. It does not depend on an S3 SDK or async runtime. Its persisted manifests record the selected layout independently from representation: version 1 uses BLAKE3-256 over canonical plaintext, identity encoding, and no encryption.

The `raw` method stores one complete bounded file payload. A partial mutation verifies and rewrites that complete payload. The `block-split` method stores sparse, file-relative 32 KiB canonical blocks; missing and all-zero blocks are holes, the manifest owns logical EOF, and a partial final block is zero-padded before hashing.

Preparation and publication are deliberately separate. Payloads become durable first, then the immutable manifest; `PreparedContent` binds the caller's mutation ID, base content identity, deterministic operation fingerprint, and attempt into collision-safe immutable keys. Its `ContentRef` can be reconstructed and validated by a future authoritative metadata adapter after restart. `ObjectHeadPublisher` is a standalone single-key CAS publisher for tests and non-clustered use, not a second authority in clustered filesystem operation. Ambiguous CAS readback never rebases unless failure to commit is definitive.

Version 1 is write-through. Successful target puts and publication CAS operations are durable under the target contract, so `sync_content` has no write-back queue to flush. This provides content durability only: it does not claim durable inode or namespace metadata, atomic namespace operations, open-unlinked handling, cross-session locks, append serialization, garbage collection, compression, or encryption. Packed objects, paged manifests, codecs, ciphers, and GC require later versioned changes.

## PostgreSQL filesystem-state adapter

`w9pt-fs-state-postgres` is the clustered authoritative metadata adapter. It accepts a caller-created SeaORM `DatabaseConnection`, implements serializable multi-writer state transitions, and stores normalized filesystem records plus immutable `ContentRef` values in collision-resistant `public.w9pt_fs_state_*` tables. It never stores file payloads, blocks, target credentials, or 9P session state.

Construction is deliberately split:

```rust,no_run
use sea_orm::{ConnectOptions, Database};
use w9pt_fs_state_postgres::{PostgresStateConfig, PostgresStateStore};

# async fn open() -> Result<(), Box<dyn std::error::Error>> {
let mut options = ConnectOptions::new(
    "postgres://user:password@primary.example/w9pt".to_owned(),
);
options.max_connections(16).min_connections(0).sqlx_logging(false);
let database = Database::connect(options).await?;

// Run explicitly with a migration-capable role. `open` never applies DDL.
PostgresStateStore::migrate(&database).await?;
let store = PostgresStateStore::open(database, PostgresStateConfig::default()).await?;
# let _ = store;
# Ok(())
# }
```

The application owns `ConnectOptions`, DSN parsing, credentials, TLS roots/provider, pool sizing, connection routing, and Tokio lifecycle. The adapter pins SeaORM exactly at 1.1.20 with default features disabled and enables `sqlx-postgres`, `runtime-tokio`, `with-chrono`, and `with-rust_decimal`; the last two support the checked-in database-generated schema entities. Applications that need TLS select the compatible SeaORM `runtime-tokio-rustls` or `runtime-tokio-native-tls` feature through Cargo feature unification. The adapter has no direct SQLx dependency or SQLx API surface. SeaORM 1.1.20 still uses SQLx 0.8 transitively for its PostgreSQL backend; “no SQLx” here means no direct dependency, imports, public types, or test APIs.

Use separately constructed SeaORM connections for migration and runtime roles where practical. The migration role uses one explicit `READ COMMITTED READ WRITE` transaction and the fixed `pg_advisory_xact_lock` before it validates the bounded custom ledger and invokes the SeaORM 1.1.20 CLI-scaffolded code-first `MigrationTrait`, so schema construction and ledger publication commit or roll back together. `migrate` uses the default 30-second statement and 5-second lock bounds; `migrate_with_config` accepts explicit checked bounds. The migration role needs permission to create the prefixed public tables, indexes, constraints, and migration ledger. The runtime role needs `USAGE` on `public` and only the table-specific `SELECT`, `INSERT`, `UPDATE`, and `DELETE` privileges validated by `PostgresStateStore::open`; it does not need production DDL privileges. `open` also rejects disconnected/non-PostgreSQL SeaORM connections, missing/checksum-drifted migrations, unsupported server versions, standby/default-read-only routes, unlogged tables, insufficient privileges, or weakened durability settings.

The initial migration comes from the reviewed final database schema using exact `sea-orm-cli 1.1.20`. The checked-in expanded entities supply table, column, type, nullability, and key metadata to the code-first migration; the CLI-generated migration scaffold wraps them. Reviewed SeaQuery overlays restore defaults, stable primary/unique/index names, and narrow PostgreSQL DDL for named CHECK constraints and `DEFERRABLE INITIALLY DEFERRED` foreign keys that SeaORM/SeaQuery 0.32 cannot regenerate faithfully. The migration checksum covers both the wrapper and every generated entity source. Earlier development schemas are unsupported and have no detection, import, upgrade, or compatibility path.

While the project remains unreleased, the public version-1 schema may be replaced directly. The current catalog has 16 prefixed tables, 130 columns, 172 constraints, and 23 indexes. Any development database whose migration checksum differs is rejected and must be reset explicitly; the adapter never attempts compatibility migration. Migration-source immutability and additive upgrade paths begin only when a compatibility-bearing release explicitly adopts them.

Version 1 defines one lease tick as one Unix-epoch microsecond. Production transactions capture PostgreSQL `clock_timestamp()` exactly once and persist deadlines as checked `NUMERIC(20,0)`. The `test-support` feature exposes a separate `w9pt_fs_state_test_v1` database clock and deterministic failure controls for conformance; production construction cannot select them.

Successful commits are acknowledged after the writable primary accepts `COMMIT` with transaction-local `synchronous_commit=on`. The adapter validates `fsync=on`, `full_page_writes=on`, and permanent logged tables. This is primary-WAL durability only: version 1 does not promise synchronous-standby survival, replica reads, multi-primary routing, cross-region active/active operation, deployment/provisioning, backups, monitoring, or `LISTEN/NOTIFY` correctness.

Live tests use caller-provided dedicated test databases and never provision testcontainers. Fixtures use scoped filesystem and clock cleanup so ordinary suites can repeat, while migration tests intentionally recreate all `public.w9pt_fs_state_*` relations; test DSNs must never target production data. A single optional target uses `W9PT_POSTGRES_TEST_DSN`. The required compatibility matrix uses `W9PT_POSTGRES_15_DSN`, `W9PT_POSTGRES_16_DSN`, `W9PT_POSTGRES_17_DSN`, and `W9PT_POSTGRES_18_DSN`; setting `W9PT_POSTGRES_TEST_REQUIRED=1` makes any missing matrix DSN fail.

```text
cargo test -p w9pt-fs-state-postgres
cargo test -p w9pt-fs-state-postgres --all-features
W9PT_POSTGRES_TEST_REQUIRED=1 cargo test \
  -p w9pt-fs-state-postgres --all-features \
  --test postgres_conformance
```

## S3 content target

`w9pt-fs-storage-s3` is the concrete runtime adapter for immutable content
objects. It accepts a caller-created AWS SDK client, forwards private repository
keys verbatim, validates bounded HEAD/GET/range responses and opaque ETags, and
uses retry-disabled conditional single-part PUTs. New targets advertise no
writable guarantees until two independent clients pass live qualification. The host owns credentials,
region, endpoint, verified TLS/SigV4, Tokio runtime, bucket/IAM/lifecycle policy,
timeouts, and deployment.

The writable production profile is an Amazon S3 general-purpose bucket. Other
S3-compatible providers are unsupported until their exact version and
configuration pass concurrency, range, failure, consistency, durability, and
checksum qualification. The current raw and sparse 32 KiB block-split layouts
favor correctness over request count; packing, caching, coalescing, multipart
uploads, read-ahead, and garbage collection remain future work.

The required Compose suite exercises both layouts against exact SeaweedFS 4.42
through two independent clients after target probes succeed. It verifies
multi-block sparse behavior, independent reopen, immutable reuse, abandoned
preparation, discarded publication results, and controlled stale-CAS ordering.
SeaweedFS remains unqualified behavioral evidence rather than a supported
durable provider.

## Research

- [9P with an S3 backend](.dev/research/9p-s3-filesystem.md)
- [Sans-I/O core and Rust filesystem interoperability](.dev/research/sans-io-core.md)
- [File transfer versus filesystem protocols](.dev/research/file-transfer-vs-filesystem-protocols.md)
- [How NFS works](.dev/research/nfs.md)
- [SteamPipe chunked content distribution](.dev/research/steampipe-content-system.md)

## License

Apache-2.0. Any implementation inspired by AGPL projects must be independently written or used under separately compatible terms.
