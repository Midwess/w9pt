# Design: Filesystem Semantic Engine

## 1. Boundary and Ownership

`w9pt-fs` owns filesystem operation meaning. It sits below the 9P
session and above the authoritative metadata and immutable content layers:

```text
w9pt session                       owns frames, tags, fids, flush, response order
  -> w9pt-fs                       owns filesystem semantics and orchestration
       -> w9pt-fs-state            owns authoritative mutable metadata
       -> w9pt-fs-storage          owns immutable content layout and verification
            -> caller TargetStore  owns target-object I/O
```

The package naming is intentional: `w9pt` remains the protocol/session crate,
`w9pt-fs` is the semantic engine, `w9pt-fs-state` is authoritative mutable
metadata, and `w9pt-fs-storage` is immutable content. Renaming the current
development package is a direct breaking cut: old package/import names,
fingerprint domains, object prefixes, and persisted data receive no compatibility
support.

The engine does not own transport connections, parse frames, encode responses,
manage credentials, choose an executor, open a database, or issue provider SDK
calls. It does not make a cache authoritative. It may retain bounded disposable
derived data, but every correctness decision is recoverable from the operation
input, caller-owned execution context, authoritative state, and immutable
content.

The first engine is compatible with a connection-affine session processor. It
does not claim transparent recovery after loss of the node holding
`w9pt::Session`; durable session inbox/outbox and response recovery require a
separate proposal.

## 2. First Vertical Slice

Version 1 implements:

```text
Walk Open Create Mkdir Symlink Read Write ReadDir Fsync
Getattr Setattr Readlink RenameAt UnlinkAt Link Release
```

It consumes but rejects these operations with `EOPNOTSUPP` and never advertises
their capabilities:

```text
Rename Remove Mknod Statfs
XattrWalk XattrCreate XattrRead XattrWrite XattrCommit
Lock Getlock
```

Fid-based rename/remove are deferred because `ObjectHandle` currently identifies
an object but does not retain the exact walked dentry provenance required to
distinguish hard links. Xattrs require portable readable-stream ownership;
locks require complete wire-visible owner state and inode-scoped queries;
`Statfs` requires a quota/capacity policy; special files require explicit backend
behavior. Each can be added later as a capability-complete slice.

Reads use explicit no-atime behavior in this slice. Strict or relative atime
would turn reads into replayable metadata mutations and needs a separate policy
and execution-identity contract; creation, write, and `setattr` timestamps
remain fully implemented.

## 3. Engine Request and Outcome

An `EngineRequest` owns the `OperationId`, `FilesystemRequest`, and an
`ExecutionContext`. The execution context is caller-created and contains:

- globally stable client/session incarnation;
- stable mutation ID and retention horizon for state-changing work;
- exact current writer fence;
- one frozen timestamp used by the logical operation;
- any deterministic allocation scope required by the identity source.

The engine validates that mutating operations have all required fields and that
read-only operations cannot accidentally publish state without an identity.
Mutation identity is not derived from a 9P tag. `OperationId` remains routing
metadata and is included in the returned completion envelope.

The engine exposes a two-level outcome:

1. A client-safe terminal result/error which may be passed to
   `Session::complete`.
2. A typed unresolved execution failure containing the state/target/policy
   source and whether exact retry is required.

An ambiguous metadata commit never becomes `EIO` merely because a response is
needed: the engine first resolves it through exact mutation-ledger replay. If
the configured resolution bound is exhausted, the host must retain/retry the
operation; guessing could acknowledge the wrong state or apply it twice.
Definitive non-commit infrastructure failures may be mapped to a stable error
after the engine proves no authoritative mutation occurred.

The bounded runner owns the selected filesystem, mutation context, and writer
fence. Every planned `CommitRequest`, including an identical ambiguity retry,
is checked against that tuple before dispatch so a planner cannot redirect or
substitute authoritative work.

## 4. Stable Identities, Handles, and QIDs

`InodeId` and `OpenId` are already stable 128-bit values. For the first slice:

- `ObjectHandle` encodes the canonical 128 bits of `InodeId`;
- `OpenHandle` encodes the canonical 128 bits of `OpenId`;
- every resolution verifies the request export/filesystem and record ownership;
- no pointer, local array index, SDK handle, or cache address is encoded.

Wire QID path is only 64-bit. Add a distinct checked `QidPath` allocated from a
per-filesystem, monotonically increasing, non-reused high-water mark. Store it
on the inode and enforce uniqueness in every state adapter. Never hash or
truncate `InodeId` to obtain a QID path.

QID type comes from immutable inode kind. QID version is zero in the first slice
unless implementation supplies a proven non-wrapping monotonic mapping; the
protocol permits zero when the backend does not track a compatible version.

New identities are supplied by a deterministic caller-owned source. Repeating
one logical operation after conflict or ambiguity produces the same identities.
IDs are domain-separated by filesystem, mutation, identity kind, and allocation
slot. An existing unrelated record at an allocated identity is a collision/error,
not permission to reuse it.

## 5. Export and Authorization Model

The host's authentication/policy layer binds an attach to an export. The engine
receives a resolver/policy object rather than authentication secrets. Resolution
returns:

- `FilesystemId` and root `InodeId`;
- canonical state principal and primary/supplementary groups;
- bidirectional numeric uid/gid and canonical principal/group mappings used for
  9P result encoding and numeric mutation operands;
- privileged and read-only flags;
- exact filesystem policy generation;
- an allowed capability ceiling.

The engine implements standard mode-bit checks:

- directory search for each walk component;
- directory write/search for create, link, unlink, and rename;
- file read/write checks at open and mutation time;
- sticky-directory ownership restrictions;
- setgid directory inheritance;
- owner/privileged rules for chmod, chown, and timestamps;
- read-only export rejection before any target mutation.

For a mutation, the authorization request includes the full logical intent and
the one-revision authoritative records used by the decision. The commit carries
record/generation preconditions and `FilesystemPolicyGeneration`. A conflict
invalidates the decision and causes the engine to reread and reauthorize.

## 6. Authoritative Directory Model

Directory data gains an authoritative parent inode. The root's parent is itself.
Only directories have parent pointers because hard links to directories are
rejected. A cross-directory directory rename changes the pointer in the same
transaction as both dentries and parent directory metadata.

Cycle prevention walks the destination's bounded ancestor chain to the root,
reading each directory and retaining exact preconditions. The commit rejects a
concurrent ancestor change; the engine then restarts. Depth is bounded before
allocation or unbounded state reads.

Persistent directory cookies remain nonzero and never reused. Creation/link use
the current high-water mark and advance it atomically. Rename preserves the
source entry's cookie so an ongoing enumeration has a stable resume identity.
Overwrite removes the destination entry and retains the moved source cookie.

`ReadDir` needs entry name, cookie, child type, and child QID at one authoritative
revision. The state layer must either expose a semantic joined directory-page
query or retain immutable child kind/QID-path summaries in each entry with
commit-time cross-record validation. The adapter cannot assemble a page using
unrelated later reads.

## 7. Walk

Walk begins from the inode encoded by `ObjectHandle` and resolves components in
order. It validates export confinement and execute/search permission at every
directory.

- `.` returns the current object/QID.
- `..` reads authoritative parent state; root remains root.
- a normal component reads the exact dentry and child inode.
- a non-directory intermediate component fails with `ENOTDIR`.
- a missing first component fails with `ENOENT`.
- a missing later component returns the non-empty successfully walked prefix,
  matching the `w9pt` partial-walk contract.

The engine bounds component count, encoded names, state reads, and depth. A walk
is not a namespace transaction; each step is linearizable and permission-checked
according to normal path-walk semantics.

## 8. Open, Create, and Release

Open flag parsing determines read/write access, append, directory-read behavior,
and truncate intent. Unsupported or contradictory flags fail before mutation.
`O_DSYNC` and `O_SYNC` remain unsupported until their guarantees can be retained
in portable open state and enforced by every write.

An open operation inserts an `OpenRecord` and matching `OpenPinRecord` in one
transaction, bound to the client incarnation. It records the stable open result
in the mutation ledger. `O_TRUNC` prepares new immutable content first and
publishes it atomically with the open/pin and inode changes. Writable or
truncating opens fail with `EROFS` before target access on a read-only export.

`Create` allocates inode, content-file, open, QID-path, and cookie identities. It
inserts an explicit empty unpublished regular inode together with its dentry,
open, pin, parent generation/times, allocation movements, and exact result. No
empty S3 payload or manifest is required before the file contains data.

Release validates ownership of the open, deletes the open and pin, and updates
the orphan count when present. When the last pin of a zero-link orphan is
released, orphan and inode retirement happen atomically. Immutable content is
not synchronously deleted; later GC uses durable reachability. Empty open
directories use the same orphan lifetime and cannot receive new namespace
entries after their last name is removed.

## 9. Immutable File I/O

### Read

Read resolves the portable open and inode from one state revision, checks access,
and takes one cloned `ContentRef`. An unpublished empty regular inode returns
EOF. Published content is read through `ContentRepository::read`, which validates
the manifest, payload identity, digests, logical range, and EOF.

The read observes one complete immutable version. A concurrent writer may
publish before or after it, but cannot cause a mixture of versions.

### Write

Write validates the open, permissions, bounds, and exact inode content base.
For append opens, the engine ignores the client offset and chooses the observed
authoritative EOF. It prepares immutable content, then commits with exact open,
inode, data-generation, base-content, policy, and fence preconditions.

If the inode has no published content, the engine uses
`prepare_write_from_new`; block-split preserves a sparse gap and raw enforces its
materialization bound. Existing content uses `prepare_write`.

Zero-length writes still consume and retain their stable mutation identity as a
validated semantic no-op, so reuse for different bytes is a hard mismatch.

The commit publishes `ContentRef`, size, data/inode generation, modification and
change times, and the exact written-count result together. A base conflict
causes complete reread, permission validation, append-offset recomputation, and
new preparation. It never blindly applies prepared partial data to a new base.

### Truncate and setattr

`Setattr` validates all selected fields as one logical operation. Without a size
change it publishes one complete checked inode replacement. With a size change,
it uses `prepare_truncate` or `prepare_truncate_from_new` and one dedicated
content-plus-attributes transition. Mode, owner/group, times, content, size, and
generations cannot become visible separately. Any selected size is rejected for
non-regular inodes, and a size change advances modification time unless the
request supplies an explicit selected value.

## 10. Namespace Transactions

Each namespace mutation reads every operand at one revision and commits all
changes together:

- `Mkdir`: parent, directory inode with authoritative parent, dentry, cookie/QID
  allocation, parent generation/times, result.
- `Symlink`: parent, target inode, dentry, cookie/QID allocation, parent
  generation/times, result.
- `RenameAt`: source/destination dentries and parents, overwritten inode link and
  orphan state, moved-directory parent, cookies, generations, and times.
- `UnlinkAt`: dentry removal, parent generation/times, target link count, and
  either inode retirement or orphan creation based on exact open-pin count.
- `Link`: new dentry/cookie, target link count/times, and parent generation/times;
  directories and cross-export targets are rejected.

Permission checks and sticky-directory rules are preconditioned on the same
records. Rename cycle/emptiness/replacement rules are checked before the commit
and revalidated by exact revisions/generations inside it.

## 11. Idempotency and Result Replay

Before allocating an ID or uploading content, a mutating execution probes the
mutation ledger using its stable identity:

- absent: evaluate normally;
- exact fingerprint/client/retention match: decode and return the retained
  result;
- mismatch: return a hard internal/protocol error without state or target work.

The engine defines a canonical fingerprint over the complete stable request
context, resolved export, operation operands, and frozen timestamp choices. It
is reconstructible before identity generation and before reading QID/cookie
high-water marks. Deterministic identity outputs and state-allocated QID paths or
cookies are not request identity; a separate versioned result encoding retains
those exact values for every successful mutating result in the first slice.
Every policy resolution performed while planning must equal the grant captured
by that fingerprint; a changed grant stops the attempt rather than committing
under a different authorization context.

The result codec validates result kind, version, lengths, counts, handle/QID
fields, and trailing data. Replayed `Create` and `Open` return the same handles
and QIDs. Codecs have independent golden fixtures and bounded decoders.

## 12. Conflict, Ambiguity, and Cancellation

A definitive `CommitOutcome::Conflict` means the prior semantic plan did not
commit. The engine discards the plan, rereads authoritative state, reruns policy,
and reprepares content if necessary. Retries are bounded and return `EAGAIN` only
after non-commit is known.

`CommitOutcome::Ambiguous` means the plan may have committed. The engine retains
the exact `CommitRequest` and retries it unchanged through the ledger. It never
re-reads and rebases until the ledger proves the original request did not
commit. Unreachable immutable objects left by a losing conflict are harmless GC
candidates.

This change does not advertise cancellation. When `w9pt` emits `Effect::Cancel`,
the host may record the request but must continue driving the original engine
execution to a terminal completion. A later cancellation proposal may add
explicit checkpoints; it still cannot roll back an authoritative commit.

## 13. Fsync and Durability

Version 1 is write-through. Before a write commit, immutable payloads and the
manifest are already durable under the target contract. A successful metadata
commit is durable under the state-store contract.

For `Fsync`:

- data-only validates the open/inode and `sync_content` for published content;
- full sync additionally requires the state contract to prove durable metadata;
- an unpublished empty file has no content object to flush;
- the engine never converts primary-only durability into a stronger failover
  promise.

Capability derivation omits `DurableData` or `DurableMetadata` when the selected
target/state deployment cannot prove them.

## 14. Limits and Error Mapping

`EngineLimits` bounds semantic retries, ambiguity resolution attempts, walk and
ancestor depth, supplementary groups, read/commit query counts, directory-page
entries/bytes, result/fingerprint bytes, and retained execution data. Existing
session, state, and storage limits remain independently enforced.

Checked arithmetic precedes all offset/count conversions, page sizing, identity
allocation, generation/cookie movement, and result encoding.

Semantic failures map deterministically to project-owned Linux errno values.
Examples include `ENOENT`, `ENOTDIR`, `EISDIR`, `EACCES`, `EROFS`, `EEXIST`,
`ENOTEMPTY`, `EXDEV`, `ELOOP`, `EBADF`, `EOVERFLOW`, `ENOMEM`, `EAGAIN`,
`EOPNOTSUPP`, and `EIO`. Adapter diagnostics remain available to the host but
are never placed on the wire.

## 15. Capability Honesty

The engine's capability builder intersects:

```text
implemented semantic slice
  ∩ export policy ceiling
  ∩ state-store guarantees/topology
  ∩ target guarantees
  ∩ configured provider availability
```

No host may request `CapabilitySet::ALL` from the engine without validation.
Memory reference stores can test atomic behavior but do not prove production
durability. Open-unlinked, atomic namespace, atomic setattr, positioned I/O, and
durability capabilities are advertised only when their full paths pass
conformance.

## 16. Deterministic Integration Model

The test harness combines:

- a real `w9pt::Session` and model trace;
- two independently opened memory state clients;
- a shared deterministic `MemoryTarget` with raw and block-split repositories;
- deterministic policy, timestamp, identity, and writer-fence inputs;
- state and target failure injection at every persistence boundary.

Tests feed real 9P frames, execute emitted effects, complete the session, and
inspect encoded responses. Restart tests discard engine-local state and reopen
from authoritative state/target objects. They prove either the complete old or
complete new state, exact ledger replay, one terminal result, and no dependence
on shared process-local correctness state.
