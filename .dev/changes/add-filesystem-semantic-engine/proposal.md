# Add Filesystem Semantic Engine

Status: approved

## Approved Revisions

- 2026-09-05: align the crate with the workspace's approved Rust 1.94.1 baseline and clarify
  that durable request fingerprints are reconstructed from stable pre-allocation intent;
  allocation outputs are retained in the terminal result rather than required to probe it.

## Summary

Add a runtime-neutral `w9pt-fs` crate that executes the backend-neutral
`FilesystemOperation` values emitted by `w9pt`. The engine will turn the normal
9P regular-file, directory, symlink, open, namespace, data, attribute, and
durability operations
into checked reads and serializable commits through `w9pt-fs-state`, while using
`w9pt-fs-storage` to prepare and verify immutable file content.

The engine is the semantic coordinator between the state and content layers. It
will enforce filesystem rules, authorization, portable handle resolution,
idempotency, fencing, conflict retries, exact result replay, and stable error
mapping. It will never parse 9P frames, issue S3 requests directly, own a
database schema, or publish an object-store file head as a second authority.

This proposal assumes the first implementation is an embeddable library. A host
still owns transport I/O, authentication, task scheduling, credentials, clocks,
stable execution identities, writer-lease lifecycle, and concrete state/target
clients. A concrete S3 `TargetStore`, transport gateway, and durable migratable
9P session processor remain separate changes.

## Motivation

The workspace now contains the three boundaries needed around a semantic
engine:

- `w9pt` parses and validates stock `9P2000.L`, owns connection state, and emits
  typed filesystem effects;
- `w9pt-fs-state` owns authoritative inode, namespace, open, orphan, lock,
  xattr, mutation-ledger, lease, and fencing records;
- `w9pt-fs-storage` prepares immutable raw or block-split content and verifies it
  on reads.

No crate currently combines those contracts into filesystem behavior. An
embedding application would otherwise have to invent permission checks,
directory traversal, stable QIDs, open lifetime, append serialization,
content-publication ordering, conflict handling, and errno mapping. Different
hosts would likely implement incompatible or unsafe semantics.

A dedicated engine creates one conformance-tested implementation and preserves
the required dependency order:

```text
immutable target data
  -> immutable target manifest
  -> authoritative metadata transaction and mutation result
  -> exact filesystem completion
  -> 9P response
```

## Goals

- Introduce `crates/w9pt-fs` with dependencies on `w9pt`, `w9pt-fs-state`, and
  `w9pt-fs-storage`, but no database client, S3 SDK, transport,
  executor, or process-global runtime.
- Consume an emitted operation together with caller-supplied durable execution
  identity, client incarnation, writer fence, timestamp, and deterministic
  allocation inputs, then return the exact `FilesystemResult` or stable
  `FilesystemError` expected by `w9pt`.
- Resolve exports and opaque handles without raw pointers, process-local SDK
  handles, or ambiguous inode-only identities.
- Persist stable non-reused QID paths and directory-parent relationships so hard
  links, `..`, directory moves, cycle checks, and open-unlinked files remain
  correct.
- Implement standard mode-bit authorization with explicit caller-owned identity
  and export policy, and revalidate every mutation-protecting decision against
  the committed state revision.
- Implement a capability-complete first vertical slice covering regular files,
  directories, symlinks, portable opens, `*at` namespace mutations, attributes,
  and durability. Consume deferred variants by omitting their capability and
  returning the documented unsupported error.
- Keep a newly created empty file explicit in metadata, then prepare the first
  sparse write/truncate safely from the distinguished new-file content base.
  Make writes, append, truncate, and size-changing `setattr` atomic with inode
  summaries and retained terminal results.
- Re-read, reauthorize, and reprepare after a semantic conflict, while retrying
  an ambiguous commit only with the identical mutation request.
- Provide a versioned bounded codec for mutation results so ledger replay returns
  the same handles, QIDs, counts, and terminal result after retry.
- Derive advertised capabilities from the engine implementation, export policy,
  authoritative state contract, target guarantees, and optional providers.
- Supply deterministic end-to-end tests that drive real `w9pt::Session` effects
  through the engine over independent memory state clients and a memory target.

## Scope

### In scope

- Workspace rename and package/import metadata for `w9pt-fs-storage`.
- Root workspace membership and package metadata for `w9pt-fs`.
- A statically dispatched, runtime-neutral engine API and typed execution
  context.
- Checked engine limits for walk depth, retries, directory pages, permission
  groups, handle state, result encoding, and retained operation data.
- Export-to-filesystem resolution and validated capability derivation.
- Stable conversion between engine/state identities and `ObjectHandle`,
  `OpenHandle`, QID, attributes, directory entries, and client-visible errors.
- Minimal state-model and adapter extensions required for stable QID allocation,
  parent traversal, bounded open-pin counts, complete one-revision directory
  pages, atomic content-plus-attribute publication, and policy-generation
  preconditions.
- Walk, open, release, create, mkdir, symlink, read, write, readdir, fsync,
  getattr, setattr, readlink, renameat, unlinkat, and link semantics.
- Sparse block-split and raw content use through the existing persisted
  `ContentRef`; storage method selection remains a creation policy.
- Standard permission, sticky-directory, setgid-directory, ownership, and
  read-only export checks driven by explicit resolved credentials.
- Open-unlinked pins, orphan retirement, append serialization, and exact
  directory cookies.
- Bounded definitive-conflict retry, ambiguous-commit replay, stable result
  encoding, and one-terminal-completion integration.
- Deterministic semantic, concurrency, recovery, capability, and full session
  integration tests.

### Out of scope

- A concrete S3 SDK adapter, credentials, HTTP client, multipart upload policy,
  or provider-specific conditional-request implementation.
- TCP, Unix socket, WebSocket, virtio, FUSE, listener, daemon, CLI, or deployment
  configuration.
- Authentication protocol implementation or secret verification; the engine
  receives an already authenticated principal and caller-resolved credentials.
- Durable 9P session inboxes/outboxes, reconnect, transparent gateway loss,
  session migration, or cross-node recovery of an in-flight response.
- A second mutable per-file object-store head in clustered operation.
- Fid-based `Rename` and `Remove`, which need exact walked-name provenance in
  durable session/fid state before they are safe with hard links.
- `Mknod`, special-file behavior, `Statfs`, xattrs, byte-range locks, and the
  corresponding capabilities. These remain follow-up vertical slices over the
  same engine contract.
- Automatic access-time updates; the first slice uses explicit no-atime read
  behavior while retaining caller-selected timestamps for creation, writes, and
  `setattr`.
- Caching, read-ahead, range coalescing, packed blocks, paged manifests,
  compression, encryption, garbage collection, compaction, snapshots, quotas,
  or object-store capacity accounting.
- Operations outside the existing stock `9P2000.L` operation contract, including
  `renameat2`, reflink, `fallocate`, arbitrary `ioctl`, and universal POSIX
  emulation.
- Background lease renewal or clocks hidden inside the engine.

## Affected Areas

| Area | Expected impact |
|---|---|
| `Cargo.toml` / `Cargo.lock` | Rename the storage member, add the semantic engine, and update path dependencies |
| `crates/w9pt-fs` | New engine, conversions, policy contracts, operation handlers, result codec, and test harness |
| `crates/w9pt-fs-state` | Add the minimum semantic records/queries/transitions required by the engine |
| `crates/w9pt-fs-state-postgres` | Persist and index the state-model additions and extend adapter conformance |
| `crates/w9pt-fs-storage` | Use the current identity domains and add integration gaps required by initial sparse preparation |
| `crates/w9pt` | Preserve the Sans-I/O boundary; make only narrowly required handle/lifecycle contract adjustments if conformance proves them necessary |
| `README.md` / `.dev/project.md` | Document the implemented engine boundary, honest deployment mode, and remaining adapters/runtime work |

## Acceptance Criteria

- The active workspace exposes `w9pt` for protocol/session behavior, `w9pt-fs`
  for filesystem semantics, `w9pt-fs-state` for authoritative metadata, and
  `w9pt-fs-storage` for immutable content; active Rust imports use
  `w9pt_fs`, `w9pt_fs_state`, and `w9pt_fs_storage` respectively.
- Current fingerprint domains and golden fixtures use `w9pt-fs-storage`; no
  alternate crate identity or persistent-data compatibility path exists.
- A host can pass every emitted `Effect::Filesystem` to the engine and construct
  exactly one matching terminal `Completion::Filesystem` without interpreting
  database records or storage manifests itself.
- All advertised operations pass deterministic semantic tests; every unprovided
  optional dependency removes the corresponding capability and yields
  `EOPNOTSUPP` before effects with stronger promises are accepted.
- Stable QID paths never change or reuse within one filesystem, and object
  handles resolve portable inode identities without truncation or hashing.
- Walk handles `.`, `..`, partial success, root confinement, non-directory
  components, rename races, and configured depth/name limits.
- Create atomically inserts an explicit empty unpublished inode, directory
  entry, open, open pin, allocation counters, timestamps, and retained result.
  The first non-empty write or extension prepares generation-one immutable
  content against the distinguished new-file base before publishing it.
- Reads observe one immutable `ContentRef`; writes, append, truncate, and
  size-changing `setattr` expose either the complete old or complete new content
  with matching size, times, and generations.
- Rename, link, unlink, and create operations publish all involved directory and
  inode changes as one serializable transaction and revalidate permissions at
  that commit boundary.
- Open-unlinked files remain usable through portable opens until the last pin is
  released, after which retirement is atomic.
- Duplicate mutation identities replay the exact recorded result only for an
  identical fingerprint and client incarnation; mismatches are hard errors.
- Stale or expired fences never commit, and conflicts cause bounded full
  semantic revalidation rather than blind last-write-wins.
- Injected failures before/after content upload, manifest upload, metadata
  commit, result publication, engine completion, and session response expose
  only complete old/new states and preserve one terminal result.
- Two independently constructed engine instances sharing no correctness-bearing
  RAM pass overlapping write, append, truncate, renameat, link/unlink,
  permission, and open-unlinked schedules.
- `cargo test --workspace --all-targets`, formatting, Clippy with warnings
  denied, dependency checks, and PostgreSQL adapter conformance pass.

## PostgreSQL development format policy

The PostgreSQL schema is unreleased and has no compatibility baseline. The
current version-1 catalog has 16 prefixed tables, 130 columns, 172 constraints,
and 23 indexes, but may be replaced directly while development continues.

Migration and open validation remain fail-closed. A development database with a
different checksum is rejected; its operator must drop all
`public.w9pt_fs_state_*` relations, including the migration ledger, and run the
current migration again. The adapter never detects, imports, upgrades, or resets
an earlier development schema automatically.

## Risks

| Risk | Mitigation |
|---|---|
| The full operation matrix is too broad for one undifferentiated implementation | Use ordered phases and capability-complete vertical slices; no partial feature is advertised |
| QID hashes or truncated inode IDs collide | Allocate and persist a checked non-reused 64-bit QID path; never derive it by truncation |
| A first sparse write to an unpublished file would materialize the gap or require two publications | Add preparation APIs bound directly to `BaseContentIdentity::NEW_FILE` for sparse initial write/truncate |
| Permission checks race with chmod/chown/rename | Carry exact record/policy revisions into serializable commit preconditions and recompute after conflicts |
| Append or partial writes lose concurrent updates | Prepare against an exact base and re-read/reprepare after content-base conflict |
| Ambiguous commit is mistaken for a conflict | Retry the identical request through the mutation ledger before any rebase |
| Numeric uid/gid interpretation drifts during a mutation | Resolve it through caller-owned export policy and fence the decision with policy generation |
| Readdir needs child QID/kind data not present in one scan | Add a bounded one-revision semantic directory-page query or validated immutable summaries |
| Replay cannot reproduce handles or results | Retain exact allocated values in a versioned bounded terminal-result codec and use deterministic identity derivation only when planning an absent mutation |
| Deferred fid-based operations are accidentally advertised | Derive capabilities from the implemented slice and test `EOPNOTSUPP` before backend work |
| Existing development PostgreSQL data uses another schema checksum | Fail closed and require an operator-driven reset; provide no compatibility migration |
| Block-split object-per-block behavior limits throughput | Keep performance optimization out of semantic correctness; benchmark and propose packing/coalescing separately |
| Adapter or target failures leak private diagnostics to clients | Map to stable errno while retaining typed internal sources for host logging |

## Dependencies

- The implemented `w9pt` Sans-I/O request/effect/completion contract.
- The current `w9pt-fs-storage` immutable content preparation and read contract.
- The implemented `w9pt-fs-state` semantic state contract and its deterministic
  memory authority.
- The in-progress PostgreSQL adapter revisions must settle before its schema
  extension is implemented.
- Caller-provided export configuration, authenticated credential resolution,
  stable mutation/client/writer identities, timestamps, writer fences, and
  deterministic allocation values.
- A `TargetStore` implementation satisfying the existing writable target
  guarantees. A production S3 implementation is a separate proposal.
- Rust 2024 and the workspace Rust 1.94.1 baseline.
