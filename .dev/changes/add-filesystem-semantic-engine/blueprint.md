# Implementation Blueprint: Add Filesystem Semantic Engine

## Design Approach

Create `w9pt-fs`, a runtime-neutral semantic coordinator over the
existing protocol, authoritative state, and immutable content contracts.

```text
host transport/policy/runtime
  -> w9pt::Session
       -> Effect::Filesystem
            -> w9pt-fs::FilesystemEngine
                 -> w9pt-fs-state::FilesystemStateStore
                 -> w9pt-fs-storage::ContentRepository<TargetStore>
       <- Completion::Filesystem
```

The engine uses static dispatch and owned request values. It performs no socket,
database-driver, S3 SDK, clock, random, thread, or executor work by itself. It
does not call `ObjectHeadPublisher` in clustered mode: the inode record in the
state store remains the sole current-content authority.

The first change delivers a useful regular-file/directory/symlink slice. It
implements `Walk`, `Open`, `Create`, `Mkdir`, `Symlink`, `Read`, `Write`,
`ReadDir`, `Fsync`, `Getattr`, atomic `Setattr`, `Readlink`, `RenameAt`,
`UnlinkAt`, `Link`, and `Release`. Other existing variants are consumed as
unsupported and excluded from the derived capability set until separate slices
provide their complete semantics.

Automatic atime mutation is also deferred. The first slice has explicit
no-atime reads; creation, content mutations, and `setattr` still use the frozen
caller timestamp required by their semantics.

## Public Contract

The exact Rust shape may change during implementation, but the ownership and
error boundaries should follow this model:

```rust
pub struct FilesystemEngine<S, T, P, I> {
    state: S,
    content: w9pt_fs_storage::ContentRepository<T>,
    policy: P,
    identities: I,
    limits: EngineLimits,
}

pub struct EngineRequest {
    pub operation_id: w9pt::OperationId,
    pub request: w9pt::FilesystemRequest,
    pub execution: ExecutionContext,
}

pub struct ExecutionContext {
    pub client_incarnation: w9pt_fs_state::ClientIncarnationId,
    pub mutation_id: Option<w9pt_fs_storage::MutationId>,
    pub retention: w9pt_fs_state::MutationRetention,
    pub fence: Option<w9pt_fs_state::WriterFence>,
    pub timestamp: w9pt_fs_state::UnixTimestamp,
}

impl<S, T, P, I> FilesystemEngine<S, T, P, I> {
    pub fn execute(
        &self,
        request: EngineRequest,
    ) -> impl Future<Output = EngineOutcome<S::Error, T::Error, P::Error>> + Send;
}
```

`EngineOutcome` separates:

- a client-safe terminal `Result<FilesystemResult, FilesystemError>` that may be
  supplied to `Session::complete`;
- a typed infrastructure or unresolved-authority failure that the host must log
  and retry/resolve before producing a terminal response;
- the original `OperationId`, preventing completion misrouting.

Mutating operations require the stable mutation identity and exact current
writer fence. Read-only operations may omit them. A 9P tag or session-local
operation number is never treated as the durable mutation identity.

## Caller-Owned Providers

### Export and access policy

Define a runtime-neutral policy interface that resolves an attached export and
principal into an owned grant:

```text
filesystem ID and root inode
canonical principal and group identities
bidirectional canonical-to-numeric and numeric-to-canonical uid/gid mapping
numeric uid/gid view and supplementary groups
privileged/read-only flags
policy generation
requested operation authorization decision
maximum capability subset
```

The engine implements standard inode mode, directory search/write, sticky-bit,
setgid-directory, ownership, and read-only checks using the grant. A mutation
includes the observed policy generation and all authorization-relevant record
revisions as commit preconditions. Every conflict reruns resolution and
authorization.

### Stable identity source

Define an explicit deterministic source for new `InodeId`, `OpenId`, content
`FileId`, and other first-slice identities. Given the same filesystem, mutation,
and allocation slot, a retry returns the same identity. The source does not read
a hidden RNG or clock. Collision/record-present outcomes are hard conflicts or
bounded retries under a new caller-supplied mutation, never silent reuse.

### Writer authority

The host acquires, renews, and releases writer leases through the state store and
passes the exact `WriterFence` to mutating engine calls. The engine validates the
fence in every commit but does not run a background renewal task.

## Prerequisite State and Storage Extensions

### Stable QID paths

Add a checked nonzero `QidPath(u64)` to every inode and a monotonically
increasing, non-reused allocation high-water mark to `FilesystemRecord`.
Creation advances the high-water mark and inserts the inode in the same commit.
Add an indexed `ReadQuery::InodeByQidPath` and enforce per-filesystem uniqueness
in the memory authority, PostgreSQL schema, and conformance suite.

`ObjectHandle` round-trips the full 128-bit `InodeId`; `Qid::path` comes only
from the persisted `QidPath`. QID version may be zero unless a separately proven
monotonic 32-bit mapping is selected.

### Directory ancestry and pages

Persist the authoritative parent inode on directory data; the root points to
itself. Directory rename updates the parent atomically. Commit validation rejects
non-directory parents, cycles, and inconsistent root ancestry.

Add one bounded one-revision directory-page query, or add checked immutable
child kind/QID summaries to `DirectoryEntryRecord`. `ReadDir` must not assemble
a page from child lookups across unrelated revisions.

### Open-pin count

Add `ReadQuery::OpenPinCount(InodeId)` and a fixed-size result. Last-link unlink
uses the count to decide whether to insert an orphan and protects the decision
with `Precondition::OpenPinCount` in the commit.

### Policy-generation precondition

Add `Precondition::FilesystemPolicyGeneration`. It fences an external policy
decision without forcing unrelated filesystem-header changes to conflict.
Policy mutations must bump the generation.

### Content plus attributes

Extend checked content publication, or add an equivalent dedicated transition,
so one size-changing `Setattr` can publish the prepared content and every
selected mode/owner/group/time change on the inode together. A second change on
the same inode is not permitted as a workaround.

### Initial sparse content

Add `ContentRepository` operations that prepare a positioned first write and a
first truncate against `BaseContentIdentity::NEW_FILE`. Block-split preparation
keeps gaps sparse. Raw preparation obeys the configured materialization limit.
The resulting generation-one `PreparedContent` is published through the normal
metadata transaction.

All additions receive memory reference behavior, reusable conformance, checked
PostgreSQL representation, indexes, migrations, and independently opened-client
tests before the engine relies on them.

The PostgreSQL representation is unreleased. Checksum drift fails closed,
development databases must be reset by their operator, and the adapter never
drops state automatically. No earlier schema is detected, imported, or upgraded;
the current v1 source and normalized catalog may be replaced directly until an
explicit release establishes a compatibility baseline.

## Execution Pipeline

### Read-only operation

```text
1. Resolve export, principal, and portable handles.
2. Build one bounded authoritative read request.
3. Validate record kinds, generations, and export ownership.
4. Evaluate permissions against the observed policy/state revision.
5. Fetch immutable content only when the operation requires bytes.
6. Verify content/manifests and clamp positioned reads to logical EOF.
7. Convert to the exact FilesystemResult variant.
```

### Mutating operation

```text
1. Validate execution identity, limits, export, handles, and request shape.
2. Probe the mutation ledger before allocating IDs or uploading objects.
3. Return the decoded retained result for an exact replay; reject a mismatch.
4. Read all semantic operands at one authoritative revision.
5. Evaluate permissions and the complete operation.
6. Prepare immutable payloads and manifest when file content changes.
7. Build one CommitRequest with exact fence, policy/record/content/open-pin
   preconditions, all state changes, and the encoded exact terminal result.
8. Validate that the planned request preserves the runner-owned filesystem,
   mutation context, and exact writer fence, then commit durably.
9. On definitive semantic conflict, reread, reauthorize, recompute, and
   reprepare within the configured retry bound.
10. On ambiguous commit, retry only the identical CommitRequest until the ledger
    proves committed/not committed or the unresolved-authority bound is reached.
11. Decode the committed/replayed result and return a client-safe terminal value.
```

The engine never sends a response. The host supplies the returned terminal value
to the originating `w9pt::Session`, which then encodes and orders the 9P frame.

## Operation Families

### Walk and lookup

- Resolve ordinary components with execute/search permission on each directory.
- Treat `.` as the current inode and `..` through authoritative directory-parent
  state, clamping the export root to itself.
- Preserve stock non-empty partial-walk behavior.
- Enforce export-root confinement, component bounds, directory kinds, maximum
  depth, and stable QID construction.

### Open and release

- Convert open flags into checked access/append/truncate semantics.
- Insert a portable `OpenRecord` and `OpenPinRecord` atomically with a retained
  exact handle result.
- Apply `O_TRUNC` as content preparation plus the open transition in one metadata
  commit.
- On release, remove the open/pin and atomically retire an unlinked orphan after
  the last pin disappears.

### Create and namespace

- `Create`, `Mkdir`, and `Symlink` allocate stable IDs/QID paths and update the
  parent dentry, cookie high-water mark, directory generation, parent timestamps,
  and result in one commit.
- New regular files begin as the state model's explicit unpublished empty
  content state.
- `RenameAt` updates source/destination entries, overwritten target link/orphan
  state, moved-directory parent, directory generations, and timestamps together.
- `UnlinkAt` handles directory emptiness, link count, orphan creation, and parent
  updates together.
- `Link` rejects directories and cross-export links and inserts one new dentry
  with link count/timestamp updates atomically.

### Regular-file I/O and attributes

- `Read` resolves the portable open and one immutable `ContentRef`; an
  unpublished empty file returns EOF.
- `Write` verifies open access and prepares against the exact base. Append uses
  authoritative EOF and repeats the full operation after conflict.
- `Setattr` applies all selected fields atomically; a size change uses prepared
  truncate and the combined content/attribute transition.
- `Getattr` and `Readlink` convert checked state without exposing adapter values.
- `Fsync` validates the open/content, calls `sync_content`, and reports full
  success only when the state contract proves the requested metadata durability.

### Directory I/O

- `ReadDir` consumes persistent cookies, obtains one bounded complete semantic
  page at one state revision, and returns only whole entries within the count
  bound.
- Cookie zero starts enumeration; returned offsets are stable resume cookies.

## Result Fingerprint and Codec

Create canonical versioned encodings for:

- the complete semantic mutation fingerprint, including export, principal,
  handles, operands, and timestamp-dependent choices available before any
  identity or state-high-water allocation;
- every successful first-slice mutation result stored in `MutationResult`;
- stable handles, QIDs, counts, and result-kind/version tags.

Allocated identities, QID paths, and directory cookies are result values rather
than request identity. They are retained by the result codec and are not inputs
required to reconstruct the ledger-probe fingerprint after restart.

Decoders validate bounds, known tags, exact lengths, and trailing data before
returning a result. Golden vectors must be independent rather than only
encode/decode round trips.

## Capability Derivation

The engine computes a maximum export capability set from:

- operations implemented by this change;
- export policy restrictions;
- state-store topology and proven durability/atomicity;
- target guarantees and selected storage limits;
- availability of open-unlinked and content publication semantics.

The first change never advertises fid-based rename/remove, mknod, statfs, xattr,
lock, cancellation, or transparent session migration. Memory state may exercise
semantics but cannot justify `DurableMetadata`.

## Files to Create or Modify

```text
Cargo.toml
Cargo.lock
README.md
.dev/project.md

crates/w9pt-fs-storage/
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
    authorization.rs
    handles.rs
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
  tests/
    session_integration.rs
    semantic_matrix.rs
    content_model.rs
    concurrency.rs
    failure_recovery.rs
    capability_derivation.rs

crates/w9pt-fs-state/src/
  ids.rs
  records.rs
  read.rs
  commit.rs
  limits.rs
  testing/conformance.rs
  testing/memory.rs

crates/w9pt-fs-state-postgres/src/
  schema/...
  row_codec.rs
  read.rs
  commit.rs
  testing.rs
```

Targeted `w9pt-fs-storage` repository/layout/model-test changes add initial
sparse preparation. `w9pt` should require no source change unless implementation
proves an existing result/handle contract cannot represent the scoped
operations.

## Implementation Phases

### Phase 1: Supporting semantic primitives

Use only the current `w9pt-fs-storage` fingerprint domains and golden fixtures.
Add QID paths, directory ancestry, directory pages, open-pin counts,
policy-generation preconditions, atomic content-plus-attribute publication, and
new-file sparse preparation. Complete memory and PostgreSQL conformance first.

### Phase 2: Engine crate and execution boundary

Add package policy, checked limits, engine request/outcome types, export/policy
interfaces, deterministic identity inputs, handle conversions, error separation,
and capability derivation scaffolding.

### Phase 3: Idempotency and conflict runner

Implement canonical fingerprints, golden result codecs, ledger-first replay,
exact ambiguous retry, definitive-conflict re-evaluation, and retry bounds.

### Phase 4: Lookup and read-only semantics

Implement walk, QIDs, getattr, readlink, file reads, directory pages, permission
evaluation, and error conversion.

### Phase 5: Opens and lifecycle

Implement open flag validation, truncate-on-open, portable open records/pins,
regular-file create/open, release, orphan lifetime, and replayed handle results.

### Phase 6: Content mutation

Implement first publication, positioned writes, append, truncate, full atomic
setattr, and fsync over both raw and block-split methods.

### Phase 7: Namespace mutation

Implement mkdir, symlink, renameat, unlinkat, hard link, directory ancestry,
cycle checks, replacement, link counts, cookies, generations, and timestamps.

### Phase 8: Session integration and hardening

Drive real Session effects through the engine, verify terminal result routing and
unsupported variants, run independent-engine schedules and every injected
failure boundary, document connection-affine deployment, and complete workspace
validation.

## Testing Strategy

- Golden vectors for handle, QID, mutation fingerprint, and terminal-result
  encodings.
- Table tests for permission bits, supplementary groups, privilege, sticky/setgid
  behavior, read-only exports, invalid flags, and error mapping.
- Byte-vector comparison for raw and block-split reads/writes/truncates, including
  sparse first writes, append, EOF, overflow, and corruption.
- Namespace-model tests for partial walk, `.`, `..`, root confinement, stable
  cookies/QIDs, hard links, rename replacement, directory cycles, emptiness,
  unlink, and open-unlinked lifetime.
- Two independently constructed engines with no shared correctness-bearing RAM
  for overlapping/disjoint writes, append, truncate, renameat, link/unlink, and
  permission-policy races.
- Failure injection before/after payload, manifest, metadata commit, result
  publication, completion handoff, and response enqueue.
- Exact retry after ambiguity, mutation-fingerprint mismatch, stale/expired
  fences, bounded conflict exhaustion, and engine reconstruction from empty
  local state.
- Full `w9pt::Session` traces proving exact result kinds, error replies, partial
  walk behavior, flush races without rollback, and ordered response effects.
- Workspace tests, rustfmt, Clippy with warnings denied, dependency-policy checks,
  and PostgreSQL version-matrix conformance.
