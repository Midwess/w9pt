# Tasks: Add Filesystem Semantic Engine

## Progress: [58/58]

PostgreSQL development policy: schemas have no backward-compatibility guarantee.
Checksum drift fails closed; operators reset development databases explicitly,
and migration/open never detect, import, upgrade, or perform that reset.

### 1. Foundations and package boundary

- [x] 1.1 Establish `w9pt-fs-storage` / `crates/w9pt-fs-storage` / `w9pt_fs_storage` as the only content crate identity, update its fingerprint domains and golden fixtures, and provide no alternate alias, reader, or compatibility migration.
- [x] 1.2 Add `crates/w9pt-fs` to the workspace with Rust 2024/Rust 1.94.1 metadata, Apache-2.0 licensing, public-documentation lints, `unsafe_code = "forbid"`, and dependencies only on `w9pt`, `w9pt-fs-state`, and `w9pt-fs-storage`.
- [x] 1.3 Add checked `EngineLimitValues`/`EngineLimits` for semantic retries, ambiguity resolution, walk/ancestor depth, groups, directory pages, result codecs, query counts, and retained bytes.
- [x] 1.4 Define the crate module boundary for execution, exports, identity, authorization, handles/QIDs, conversions, result codecs, errors, operation handlers, and deterministic testing.
- [x] 1.5 Add dependency-policy tests proving the engine core has no database adapter, SeaORM/SQLx, S3 SDK, socket, Tokio, clock, RNG, executor, or platform-global dependency.

### 2. Stable state identity and bounded lookup prerequisites

- [x] 2.1 Add checked `QidPath`, persist it on every inode, add a non-reused per-filesystem QID-path high-water mark, and provide atomic checked allocation movement.
- [x] 2.2 Add indexed `InodeByQidPath` lookup plus memory-store and PostgreSQL uniqueness enforcement; reject zero, duplicate, reused, wrapped, or cross-filesystem paths.
- [x] 2.3 Add authoritative directory-parent identity with root self-parent semantics and atomic parent updates for directory moves.
- [x] 2.4 Extend record-set and commit validation for parent kind/existence, root ancestry, directory hard-link rejection, and bounded cycle-safe transitions.
- [x] 2.5 Add a bounded one-revision directory-page query or validated dentry child-kind/QID summaries sufficient to construct `DirectoryEntry` results without cross-revision joins.
- [x] 2.6 Add a fixed-size `OpenPinCount(InodeId)` read result and validate `Precondition::OpenPinCount` against the same authoritative count at commit.
- [x] 2.7 Update state limits, memory authority, reusable conformance, PostgreSQL schema/migration/indexes, row codecs, query plans, and adapter tests for all identity/lookup additions.

### 3. State publication and policy prerequisites

- [x] 3.1 Add `Precondition::FilesystemPolicyGeneration` and require policy changes to advance the generation monotonically.
- [x] 3.2 Extend content publication, or add an equivalent dedicated transition, to atomically apply a prepared size change with every selected mode, owner, group, and timestamp update.
- [x] 3.3 Validate content-plus-attribute publication against mutation identity, exact new-file/current base, content file ID, size, data/inode generations, selected-field rules, and record limits.
- [x] 3.4 Add deterministic memory tests for policy races, content-plus-attribute conflicts, stale fences, result replay, and ambiguous commit outcomes.
- [x] 3.5 Implement the prerequisite transitions in PostgreSQL and extend independent-client conformance across supported server versions.

### 4. Initial sparse content preparation

- [x] 4.1 Add `ContentRepository::prepare_write_from_new` bound to `BaseContentIdentity::NEW_FILE`, with generation one, stable preparation identity, and no object-head publication.
- [x] 4.2 Implement raw first-write zero-gap materialization with checked raw limits and block-split first-write sparse gaps without untouched block uploads.
- [x] 4.3 Add `prepare_truncate_from_new` so zero and nonzero initial logical sizes produce canonical generation-one manifests without resurrecting or inventing bytes.
- [x] 4.4 Add byte-model, deterministic replay, collision verification, corruption, limit, and failure-ordering tests for both new-file preparation paths.

### 5. Engine execution, identity, policy, and replay core

- [x] 5.1 Define owned `EngineRequest`, `ExecutionContext`, and `EngineOutcome` types that retain `OperationId` and separate client-safe terminal values from unresolved infrastructure/authority failures.
- [x] 5.2 Define caller-owned export/access policy and deterministic identity-source interfaces with owned futures or synchronous values and no hidden runtime/global state.
- [x] 5.3 Implement portable lossless `ObjectHandle`/`InodeId` and `OpenHandle`/`OpenId` conversion, persisted QID construction, export validation, and malformed-handle errors.
- [x] 5.4 Implement standard mode-bit, directory search/write, sticky-directory, setgid-directory, ownership, privileged, and read-only export checks over resolved credentials.
- [x] 5.5 Define canonical pre-allocation full-intent fingerprints and independently versioned/bounded mutation-result codecs that retain allocation outputs, with golden fixtures for every successful first-slice mutation result.
- [x] 5.6 Implement ledger-first exact replay/mismatch handling before identity allocation or content upload, including exact reconstruction of handles, QIDs, counts, and result kinds.
- [x] 5.7 Implement the bounded mutation runner: one-revision read, authorization, optional preparation, commit, exact ambiguity replay, full conflict re-evaluation, stable error mapping, and retry exhaustion.

### 6. Walk and read-only semantics

- [x] 6.1 Implement export-root resolution and a helper that returns the stable root handle/QID plus the engine-derived capability set for an authenticated attach policy.
- [x] 6.2 Implement `Walk` for ordinary components, `.`, `..`, root confinement, non-empty partial success, directory/search checks, and bounded depth/name handling.
- [x] 6.3 Implement `Getattr` conversion for inode kind, permissions, caller-owned numeric identity mapping, size, link count, timestamps, generations, allocation estimates, and requested masks.
- [x] 6.4 Implement `Readlink` with kind, permission, target-length, UTF-8/wire, and configured result bounds.
- [x] 6.5 Implement `ReadDir` from persistent cookies and one-revision bounded pages, returning only complete entries whose semantic encoding fits the requested count.

### 7. Open, create, and lifetime semantics

- [x] 7.1 Implement open-flag validation and conversion to portable access/append/directory state, rejecting unsupported combinations before mutation.
- [x] 7.2 Implement `Open` with permission revalidation, deterministic `OpenId`, atomic open/pin insertion, retained exact result, and replay.
- [x] 7.3 Implement `Create` as one atomic empty-inode/dentry/open/pin/QID/cookie/parent/result mutation with exclusive/truncate/read-only/setgid rules.
- [x] 7.4 Implement `O_TRUNC` on existing regular files through immutable preparation and one commit containing the open/pin plus content/timestamp/generation publication.
- [x] 7.5 Implement `Release` with open ownership validation, pin removal, orphan-count adjustment, last-pin orphan/inode retirement, replay safety, and no synchronous content deletion.

### 8. Regular-file data, attributes, and durability

- [x] 8.1 Implement explicit no-atime `Read` through portable open resolution and one immutable `ContentRef`, including unpublished-empty EOF, exact positioned range behavior, and corruption-to-`EIO` mapping without publication.
- [x] 8.2 Implement positioned `Write` for published and new-file bases with access checks, checked counts, immutable preparation, atomic inode publication, and exact written-count replay.
- [x] 8.3 Implement append by choosing authoritative EOF, then recomputing EOF and repreparing after every content-base conflict without lost or overlapping acknowledgments.
- [x] 8.4 Implement `Setattr` for every selected non-size field with ownership/privilege checks and one atomic inode transition.
- [x] 8.5 Implement size-changing `Setattr` through prepared truncate plus the content-and-selected-attributes transition, including sparse initial extension, shrink, timestamp rules, and conflict retry.
- [x] 8.6 Implement data-only/full `Fsync` with open validation, content verification, target/state durability checks, and capability-safe rejection when the requested guarantee is unavailable.

### 9. Directory and namespace mutations

- [x] 9.1 Implement `Mkdir` with deterministic inode/QID/cookie allocation, parent identity, setgid inheritance, permissions, parent generation/times, and one retained-result commit.
- [x] 9.2 Implement `Symlink` with bounded target, deterministic allocation, ownership/mode rules, parent updates, and exact replay.
- [x] 9.3 Implement `Link` with directory/cross-export rejection, destination absence, stable new cookie, link-count/timestamp updates, and atomic parent mutation.
- [x] 9.4 Implement `UnlinkAt` for files and directories with flag/kind checks, directory emptiness, sticky policy, link count, authoritative open-pin count, orphan creation, or atomic retirement.
- [x] 9.5 Implement `RenameAt` for same/cross-directory moves, replacement rules, preserved source cookie, overwritten target links/orphans, moved-directory parent, generations, and timestamps.
- [x] 9.6 Implement bounded ancestor validation and concurrent cycle/parent-change conflict tests for directory rename.

### 10. Capability honesty, integration, and completion

- [x] 10.1 Implement capability derivation as the intersection of completed engine operations, export policy, state-store guarantees/topology, target guarantees, and configured providers.
- [x] 10.2 Prove fid-based rename/remove, mknod, statfs, xattr, lock, cancellation, and migration capabilities are omitted, deferred operations return `EOPNOTSUPP` before state/target mutation, and reads preserve explicit no-atime behavior.
- [x] 10.3 Build a deterministic full-stack harness using real `w9pt::Session` frames/effects/completions, independent memory state clients, memory target, policy, identities, timestamps, and fences.
- [x] 10.4 Add raw/block-split byte-vector and namespace-model suites covering aligned/unaligned/sparse/EOF/overflow I/O, walk, QID/cookie stability, hard links, rename replacement, unlink, and open-unlinked lifetime.
- [x] 10.5 Add two-engine concurrency schedules for overlapping/disjoint writes, append, truncate, renameat, link/unlink, permissions, policy changes, stale fences, and bounded retry exhaustion.
- [x] 10.6 Inject failure before/after payload, manifest, metadata commit/result publication, completion handoff, and response enqueue; reopen without engine-local state and prove complete old/new visibility plus exact replay.
- [x] 10.7 Document host driving, writer authority, connection-affine session limits, error/diagnostic handling, capability selection, unsupported slices, and the required future S3/transport/session work in crate/root documentation.
- [x] 10.8 Run `cargo test --workspace --all-targets`, PostgreSQL feature/version conformance, rustfmt, Clippy with warnings denied, dependency/license checks, and update `.dev/project.md` with the finalized semantic-engine boundary.

### Notes

- 2026-09-09: `cargo test --workspace --all-targets --all-features --locked`,
  PostgreSQL all-feature conformance, rustfmt, Clippy with warnings denied, and
  rustdoc with warnings denied passed.
- License metadata and the exact `w9pt-fs` depth-one dependency boundary passed.
  RustSec passed under the repository policy after the all-feature/all-target
  reachability guard proved the locked `rkyv 0.7.46` and `rsa 0.9.10` advisory
  packages unreachable; only the existing CI-enforced exceptions were used.
