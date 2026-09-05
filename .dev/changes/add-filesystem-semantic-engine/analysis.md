# Analysis: Add Filesystem Semantic Engine

## Current State

- `w9pt` is a dependency-free Sans-I/O session state machine. It emits owned
  `Effect::Filesystem { operation_id, request }` values and requires one exact
  terminal completion for every emitted operation.
- `FilesystemOperation` already covers the declared stock `9P2000.L` matrix and
  `FilesystemResult` defines the exact success variant for each operation.
- `w9pt-fs-storage::ContentRepository<S>` implements runtime-neutral immutable
  content create/read/write/truncate preparation for raw and block-split layouts.
  The former development crate name and fingerprint domain have no compatibility
  alias or reader.
- `w9pt-fs-state::FilesystemStateStore` implements the runtime-neutral contract
  for one-revision reads, serializable declarative commits, retained mutation
  results, leases/fences, and revision invalidation.
- The current checkout includes a PostgreSQL state adapter, but transport,
  filesystem semantic execution, a concrete S3 target adapter, and durable
  session state are still explicitly listed as future work.
- `.dev/project.md` already exists, so no dev-workflow bootstrap is required.
  There is no archived/current `.dev/specs` tree; the existing normative deltas
  remain under their approved change directories.
- The worktree contains unrelated active state/PostgreSQL changes. Proposal
  creation must add only `.dev/changes/add-filesystem-semantic-engine/` and must
  not rewrite or normalize those files.

## Existing Interfaces to Reuse

### Sans-I/O request boundary

`w9pt::FilesystemRequest` owns the attach-bound session, principal, and export
context plus one `FilesystemOperation`. The outer `Effect` carries a
session-local `OperationId`. Session code already:

- validates the attached export's advertised capabilities before emitting work;
- owns tags, fids, framing, flush ordering, and response encoding;
- accepts unrelated completions out of order;
- rejects wrong-kind, duplicate, stale, and unknown completions;
- treats cancellation as best effort rather than transaction rollback.

The semantic engine should not duplicate those responsibilities. It should
accept the owned effect payload, execute semantic work, and return the exact
typed result/error for completion.

### Immutable content boundary

`ContentRepository<S>` is generic over `TargetStore` and exposes:

- `prepare_create` using the configured method for a new file;
- `read` using the persisted method in the `ContentRef`;
- `prepare_write` against an exact immutable base;
- `prepare_truncate` against an exact immutable base;
- `sync_content` for the version-1 content-only durability barrier.

`PreparedContent` already binds the mutation ID, base content identity,
operation fingerprint, and attempt. Payload objects are stored before the
manifest. The semantic engine must publish the returned `ContentRef` only
through the metadata transaction, not through `ObjectHeadPublisher` in
clustered operation.

### Authoritative state boundary

`FilesystemStateStore` exposes owned associated-future methods for bounded
consistent reads, serializable commits, writer leases, and change polling.
Existing records cover filesystems, inodes, directory entries, opens, open pins,
orphans, locks, xattrs, xattr staging, mutation results, and writer leases.

`CommitRequest` already carries the mutation context, exact writer fence,
preconditions, changes, and an opaque versioned terminal result. It distinguishes
committed, replayed, conflicted, rejected, and ambiguous outcomes.
`StateChange::PublishContent` validates an existing inode's exact base,
preparation identity, content file ID, size, and generation movement.

## Material Integration Gaps

These gaps must be resolved before the engine can honestly claim the complete
filesystem semantics exposed by `w9pt`.

### Stable QID and portable inode handles

`w9pt::Qid::path` is 64-bit, while `InodeId` is 128-bit and the state model has
no persistent QID path. Hashing or truncating an inode ID cannot prove collision
freedom.

For the first vertical slice, `ObjectHandle` can round-trip the complete
`InodeId`; `OpenHandle` can round-trip `OpenId`. The engine must never derive the
wire QID path by hashing or truncating either handle. State adapters need an
indexed stable QID path field and allocation contract. Directories also need an
authoritative parent inode for `..` and cycle prevention, with the root pointing
to itself.

Fid-based `Rename` and `Remove` additionally require the exact directory entry
through which a fid was walked. An inode-only handle cannot distinguish two hard
links. Those variants are deliberately deferred until durable session/fid state
can retain portable name-binding provenance; the first slice implements
`RenameAt` and `UnlinkAt` and does not advertise the fid-based capabilities.

### First publication from a new empty file

The state model intentionally permits a new regular inode with zero size,
generation zero, and no `ContentRef`. `Create` can publish that empty inode with
its dentry, open, pin, and retained result in one metadata transaction without
creating an unnecessary target object.

The storage repository cannot currently prepare a positioned sparse first write
or nonzero truncate directly from `BaseContentIdentity::NEW_FILE`. Add explicit
new-file preparation APIs so a large sparse gap is not materialized and the
generation-one manifest is bound to the correct base and mutation identity.

### Atomic size-changing setattr

`Setattr` may select size together with mode, owner, group, and timestamps.
Existing content publication targets the inode and cannot be combined with a
second replacement of the same inode in one commit. The publication transition
therefore needs a checked selected-attribute update, or an equivalent dedicated
atomic content-and-attributes transition.

### Identity and authorization

The protocol surfaces numeric uid/gid values, state records currently retain
opaque principal/group values, and `RequestContext` does not contain
supplementary groups or privilege information. The engine needs an explicit
caller-owned resolver that maps in both directions between canonical
principal/group identities and numeric uid/gid values, and produces
supplementary groups, privilege flags, and policy generation.
The mapping remains export policy rather than a hard-coded string/number
conversion. Mutation commits protect the resolved decision with an exact policy
generation precondition.

### Query shape and reply completeness

- Directory scans return dentry records but not child QID/kind summaries, so a
  complete directory page cannot currently be returned from one authoritative
  revision without a joined query or validated denormalized fields.
- Last-link unlink needs a bounded authoritative open-pin count before deciding
  whether to create an orphan.
- Lock and xattr scans are filesystem-wide, lock records cannot reconstruct the
  full wire owner, and readable xattr handles have no portable owner.
- `Statfs` has no capacity source because neither state nor target contracts
  expose physical capacity or quota information.

The first change adds a bounded directory-page query/summary and open-pin count.
Locks, xattrs, `Statfs`, and their extra state/provider contracts are deferred
and omitted from the derived capability set.

### Durable execution identity

`OperationId` is session-local and a 9P tag is reusable. Neither is a sufficient
filesystem mutation identity. Execution must receive a globally stable
`MutationId`, `ClientIncarnationId`, current `WriterFence`, timestamp, retention
horizon, and deterministic allocation inputs. The engine computes a canonical
fingerprint of the full stable semantic intent before consulting state
allocation high-water marks or producing identities. Allocation outputs are not
request identity: exact handles, QIDs, cookies, and counts are recorded in the
versioned terminal result and reconstructed from that result on replay.

Until durable session inbox/outbox state is implemented, the host must keep the
connection and engine execution pinned. Node loss ends that session; the engine
must describe this as connection-affine or durability-stateless operation, not
transparent session migration.

## Architectural Conclusions

- The new semantic crate is named `w9pt-fs`; the state and content support crates
  are named `w9pt-fs-state` and `w9pt-fs-storage`. The protocol/session crate
  remains `w9pt`.
- Dependency direction is one way:

  ```text
  w9pt-fs
    -> w9pt
    -> w9pt-fs-state
    -> w9pt-fs-storage
  ```

- `w9pt`, `w9pt-fs-state`, and `w9pt-fs-storage` must remain independent of the new
  engine. Supporting model changes belong in their existing semantic contracts,
  not as engine-private database or object representations.
- Use static dispatch and return-position futures, following both existing store
  traits. Do not add Tokio or an object-safe boxed service requirement.
- Keep authoritative mutable state in the state store. Engine-local caches and
  binding tables are optional, bounded, revision-validated, and disposable.
- Every mutation follows read/authorize/prepare/commit/complete. Conflict means
  restart semantic evaluation. Ambiguity means replay the exact commit request.
- Every advertised capability must be supported through the complete stack.

## Expected Files

### New crate

```text
crates/w9pt-fs/
  Cargo.toml
  README.md
  src/
    lib.rs
    config.rs
    limits.rs
    engine.rs
    execution.rs
    export.rs
    identity.rs
    handles.rs
    authorization.rs
    conversion.rs
    result_codec.rs
    error.rs
    operations/
      mod.rs
      walk.rs
      open.rs
      namespace.rs
      io.rs
      attributes.rs
      sync.rs
    testing/
      mod.rs
      harness.rs
      identities.rs
      policy.rs
```

### Existing areas

- `Cargo.toml`, `Cargo.lock`
- `crates/w9pt-fs-state/src/{ids,records,read,commit,limits,testing}.rs`
- `crates/w9pt-fs-state-postgres/src/{schema,row_codec,read,commit}.rs`
- PostgreSQL migrations and adapter tests
- current `crates/w9pt-fs-storage` identity domains and imports
- narrowly proven integration changes in `w9pt` or `w9pt-fs-storage`
- `README.md`, `.dev/project.md`

## Conventions to Follow

- Rust 2024, Rust 1.94.1, Apache-2.0, `unsafe_code = "forbid"`, complete public
  documentation, and workspace Clippy policy.
- Checked arithmetic before range calculations, allocation, index conversion,
  counter movement, and encoded-length calculation.
- Typed bounded values and errors instead of unchecked strings, integers, or
  database-specific errors in public semantics.
- Caller-owned clocks, identities, policies, clients, runtimes, and credentials.
- Immutable object keys never contain visible paths.
- Deterministic memory references and reusable conformance tests.
- No copying or close adaptation of AGPL ZeroFS implementation expression.

## Risks and Dependencies

- This is a large cross-crate change. Supporting state-model work should land
  before individual operation handlers.
- PostgreSQL schema changes overlap an active dirty worktree and must be rebased
  deliberately during implementation.
- Directory ancestry, stable handles, initial prepared creation, and terminal
  replay are correctness prerequisites, not optional cleanup.
- Object-per-32-KiB block performance is intentionally outside this engine; the
  engine must not weaken atomicity to hide storage latency.
- Production readiness still depends on a separately validated S3 target
  adapter, deployment durability evidence, and a host runtime.
