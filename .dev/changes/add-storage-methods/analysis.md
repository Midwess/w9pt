# Analysis: Add Pluggable File Storage Methods

## Current State

- The workspace contains only `crates/w9pt`, a Rust 2024 Sans-I/O `9P2000.L` protocol/session crate.
- `w9pt` already emits owned, backend-neutral `Read`, `Write`, `Setattr`, and `Fsync` operations with opaque handles and explicit offsets.
- The core validates request sizes and offset overflow, accepts out-of-order completions, exposes cancellation, and enforces attached-export capabilities.
- There is no backend crate, content layout, object-store contract, persistent format, metadata engine, hash implementation, target adapter, cache, or storage conformance suite.
- `.dev/project.md` exists. There is no archived `.dev/specs/` baseline yet; the implemented Sans-I/O requirements remain in the approved `complete-sans-io-core` change.

## Request Interpretation

The requested “storage method” is interpreted as the physical layout of one logical file's bytes on a target backend:

- configuration name `raw`, Rust method name `Raw`: one complete file payload object;
- configuration name `block-split`, Rust method name `BlockSplit`: fixed file-relative blocks, with version 1 fixed at 32 KiB.

This layer receives an opaque file identity from a future filesystem engine. It does not decide visible paths, directory membership, inode identity, permissions, or namespace transactions.

The term `raw` is intentionally limited to the whole-file layout. Payload compression and encryption are representation properties independent of layout. Version 1 persists representation tags but implements only identity/no-encryption.

## Similar Features and Evidence

### Existing w9pt boundary

`crates/w9pt/src/filesystem/request.rs` already preserves positioned reads and writes without exposing target layout. `SetAttributes` carries size changes, and `Fsync` distinguishes data-only from full durability. These are sufficient inputs for a future semantic engine to call the content repository; no protocol type needs a storage-method field.

### Object-backed extent systems

The local S3 research supports fixed logical extents, immutable data objects, data-before-metadata ordering, conditional publication, sparse holes, and later packing into large segments. Those are functional design inputs only. ZeroFS is AGPL-licensed and must not supply implementation expression, tests, or persistent-format structure.

### SteamPipe-style manifests

The SteamPipe research supports a useful separation between immutable payloads, versioned recipes, and an atomically selected active version. Its snapshot/update abstraction is not a live filesystem, but immutable preparation followed by a small publication step transfers cleanly to this content repository.

## Architectural Findings

### The repository belongs outside `w9pt`

The protocol crate must remain dependency-free and unaware of target keys, blocks, manifests, hashes, compression, encryption, and SDKs. A new `w9pt-fs-storage` crate can use associated futures or another runtime-neutral asynchronous interface without introducing a mandatory executor into the protocol core.

### Content preparation and filesystem publication are different operations

A content repository can safely upload immutable data and an immutable manifest, but it cannot by itself atomically change inode size, timestamps, generation, and content reference. The primary API should therefore return a prepared `ContentRef` without making it visible.

An `ObjectHeadPublisher` is still useful for a standalone content-store MVP and concurrency tests. It stores one mutable, compare-and-swap-controlled head per opaque file. A future filesystem metadata transaction can replace this publisher while reusing both layouts unchanged.

### The target is the authoritative store

All file heads, manifests, and payload objects needed to reopen content live under the configured private target prefix. RAM indexes and the deterministic in-memory test target are accelerators or test tools, not additional authorities. Normal reads do not depend on target `LIST` behavior.

### Immutable data plus conditional publication bounds failure states

The safe order is:

```text
new immutable payload object(s)
  -> new immutable manifest
  -> compare-and-swap file head or future inode root
```

A failure before the final publication creates unreachable objects but leaves the old content current. A successful head update makes the complete new manifest visible. Published metadata never points at an object that has not already been acknowledged durable by the target.

### Layout is persisted, not inferred from current configuration

The selected method, method parameters, logical size, representation identifiers, content references, and format version belong in the manifest. Changing the configured default only affects newly created files. Existing files dispatch from their persisted manifest, which allows mixed layouts in one target prefix.

### Raw is deliberately bounded

The simple raw method fetches and verifies the complete prior object for a partial write, constructs the new logical file, and uploads a new complete object. Full materialization allows end-to-end digest verification but requires a strict `max_raw_file_bytes` limit. It is appropriate for small or replace-oriented files, not large random-write workloads.

### Block split is sparse and canonical

Version 1 uses 32 KiB logical plaintext blocks scoped to one file. Each materialized block decodes to exactly 32 KiB. The manifest's logical size defines EOF; absent entries and omitted all-zero blocks read as zeroes. The final materialized block is zero-padded before hashing so truncate and later extension cannot expose stale data.

### Hashes have two distinct roles

A BLAKE3-256 plaintext digest supports integrity verification and lets a prepared mutation reuse an unchanged reference. A mismatch while reading is corruption. Hashes do not order writers, repair corrupted bytes, authorize access, or prove manifest authenticity.

### Concurrency remains explicit

The provisional file-head publisher reads a target version token, prepares a new version, and conditionally replaces the head. A conflict either rebases the logical operation on the new head within a configured retry bound or returns a typed retryable conflict. The future filesystem engine remains responsible for append-position resolution, per-inode ordering, authorization, and stronger transaction semantics.

## Proposed Persistent Objects

```text
<private-prefix>/v1/
  format
  refs/files/<file-id>                         # mutable only through CAS
  manifests/<file-id>/<mutation-id>/<attempt>  # immutable
  data/<file-id>/<mutation-id>/<attempt>/raw
  data/<file-id>/<mutation-id>/<attempt>/blocks/<index>
```

Visible file and directory names never appear in target keys. File and mutation identities are caller supplied. The retry attempt distinguishes rebased preparations that share one logical mutation identity but produce different bytes.

The file head identifies the current generation, manifest key/hash, and mutation identity. The manifest identifies the file, generation, logical size, method, method parameters, representation, and immutable payload references.

## Affected Files

| Path | Change |
| --- | --- |
| `Cargo.toml` | Add `crates/w9pt-fs-storage` as a workspace member. |
| `Cargo.lock` | Record the separately justified storage-crate hash dependency. |
| `crates/w9pt-fs-storage/Cargo.toml` | Add package metadata, compatible dependencies, and workspace lint policy. |
| `crates/w9pt-fs-storage/src/lib.rs` | Document and export the content-repository API. |
| `crates/w9pt-fs-storage/src/config.rs` | Persisted method selection and validated defaults/limits. |
| `crates/w9pt-fs-storage/src/error.rs` | Typed format, range, corruption, limit, conflict, and target errors. |
| `crates/w9pt-fs-storage/src/ids.rs` | Strong file, mutation, object-key, version, and content-reference types. |
| `crates/w9pt-fs-storage/src/limits.rs` | Checked bounds for raw materialization, manifests, objects, ranges, and retries. |
| `crates/w9pt-fs-storage/src/object_store.rs` | Runtime-neutral target operations and semantic guarantees. |
| `crates/w9pt-fs-storage/src/publisher.rs` | File-head load, conditional publication, and bounded rebasing. |
| `crates/w9pt-fs-storage/src/repository.rs` | Shared create/read/write/truncate dispatch and preparation rules. |
| `crates/w9pt-fs-storage/src/format/*` | Checked envelope, head, manifest, blob-reference, and canonical codec. |
| `crates/w9pt-fs-storage/src/layout/*` | Raw, block-split, and checked range-planning implementations. |
| `crates/w9pt-fs-storage/src/testing/*` | Deterministic memory target, failure injection, and reusable conformance helpers. |
| `crates/w9pt-fs-storage/tests/*` | Golden format, byte-model, publication, crash, corruption, and target-contract tests. |
| `README.md` | Document the new storage-method crate and its deliberately limited status. |

`crates/w9pt` is not modified by this change.

## Conventions to Follow

- Keep 9P protocol/session types independent of storage layout.
- Use strong identifier newtypes and owned public values.
- Check every offset, length, index, allocation, and encoded-size calculation before mutation.
- Reject unknown major formats and non-canonical or over-limit manifests without partial state changes.
- Keep public error domains typed; target-private diagnostics are not persisted or exposed as semantic errors.
- Use no hidden I/O in constructors or destructors.
- Accept time, randomness, mutation identity, scheduling, and target clients from the caller.
- Treat cancellation as advisory and never promise rollback after publication.
- Make capabilities enforceable promises. This content layer alone cannot claim namespace, stable-identity, open-unlinked, metadata-durability, or cross-session-lock guarantees.
- Build tests from this proposal and generic filesystem behavior, not from AGPL implementation source.

## Risks and Dependencies

| Risk or dependency | Required response |
| --- | --- |
| Target lacks atomic CAS or durable puts | Reject writable publisher configuration or limit it to an explicitly weaker future mode. |
| Raw file exceeds memory bound | Return a typed limit error before target allocation or download. |
| Flat block manifest exceeds configured bound | Reject the mutation; add paged maps in a later format change. |
| Concurrent partial writes conflict | Rebase from the newly published manifest or return a typed conflict; never last-write blindly. |
| Target returns corrupted/truncated bytes | Verify envelope and plaintext digest and return corruption. |
| Publication outcome is ambiguous | Read back the head and compare generation, manifest, and mutation identity before retrying. |
| Immutable staging leaks objects | Leave them unreachable and safe; later GC traces published roots and retained pins. |
| Future codecs change physical length | Keep logical layout and representation metadata separate. |
| Future packed segments change physical location | Introduce a new representation/index layer rather than changing 9P operations. |

