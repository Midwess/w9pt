# Codebase Analysis: add-paged-block-manifests

Generated: 2026-09-06
Scope: S3 BlockSplit mapping pages; proposal only.

## Project Context

The repository is a Rust 1.94.1 workspace with a runtime-neutral protocol core,
content repository, filesystem-state contract, and semantic-engine foundation.
The S3 adapter uses a caller-owned AWS SDK client; the PostgreSQL adapter uses
a caller-owned SeaORM connection. The separate `test/` workspace demonstrates
TCP/WebSocket forwarding through a minimal direct-SDK filesystem.

`AGENTS.md` and `.dev/project.md` require immutable target content followed by
one authoritative metadata publication. APIs and stored formats are unreleased:
superseded representations must be removed without compatibility readers.

## Similar Features and Execution Paths

| Location | Existing behavior | Paging implication |
| --- | --- | --- |
| `src/format/manifest.rs` in `w9pt-fs-storage` | `ManifestLayout::BlockSplit` owns a sorted `Vec<BlockEntry>` | Replace the whole-map field with one bounded root reference |
| `src/repository.rs::load_manifest` | Fetches and decodes the entire manifest and validates all inline keys | Keep compact root verification; check descendants when accessed |
| `src/repository.rs::{read,prepare_write,prepare_truncate}` | Load manifest before layout dispatch | Public preparation/publication split can remain intact |
| `src/layout/block_split.rs::read` | Downloads only touched payloads, sequentially, using `BlockSpans` | Payload read pattern already fits bounded paging |
| `src/layout/block_split.rs::write` | Clones every reference into a `BTreeMap` and stages every changed block | Replace with bounded preflight and ordered copy-on-write frontier |
| `src/layout/block_split.rs::{create,write_from_new}` | Build complete map vectors and block staging lists | New-file paths also need the bounded builder |
| `src/layout/block_split.rs::truncate` | Copies whole map and checks the old partial EOF payload before filtering | Prune by subtree ranges; specify touched-data validation precisely |
| `src/layout/range.rs` | Checked logical ranges and block spans | Reuse offset/EOF logic; extend checked routing in block-index units |
| `src/publisher.rs::{publish_head,load_content}` | Standalone CAS publisher; has an additional manifest loading path | Apply the same compact-root checks without recursive full-map validation |

Paths in this table are relative to `crates/w9pt-fs-storage/`.

## Current Limits and Memory Costs

`StorageLimitValues` defaults to an 8 MiB manifest, 131,072 flat entries,
8 MiB per read/write request, 16 MiB Raw materialization, 64 MiB per object,
and 1,024 bytes per target key. The block-count ceiling is not an effective
dense-file capacity guarantee because the encoded manifest usually reaches
its byte ceiling first.

The read output, full manifest bytes, decoded references and keys, mutation
maps, and staged blocks are separate allocations. `load_payload` briefly holds
encoded object bytes and a plaintext copy. `put_immutable` retains comparison
bytes while supplying owned bytes to the target, and `prepare_manifest` also
copies its encoding. Page limits must account for this amplification, not just
the on-target page size.

## Persistent Format and Identity

- `format/envelope.rs` currently accepts only major/minor `1/0`, with kinds
  Head, Manifest, and Payload. Mapping pages need distinct checked kinds and
  an explicit incompatible current-version boundary.
- `config.rs` defines 32 KiB blocks and identity/BLAKE3/no-encryption
  representation. Add persisted map profile parameters without changing the
  logical block behavior or letting process defaults reinterpret roots.
- `keys.rs` binds file, mutation, base generation/digest, operation fingerprint,
  and attempt. New page keys also bind page kind/location/level. Referenced
  pages from earlier preparations must remain reusable in later file versions.
- `ContentRef` in `ids.rs` already holds file ID, generation, logical size,
  root key/digest, and method. These bounded fields can point to the new compact
  manifest without a variable-length list or new PostgreSQL block table.
- `validate_content` currently verifies a manifest, not every payload. Its
  paged counterpart must not turn ordinary open/sync/publication into a tree
  scrub; describe its narrower root validation accurately.

## Failure and Preflight Contracts

The deterministic tests
`block_count_and_manifest_size_fail_before_payload_upload` in
`src/layout/block_split.rs` and
`limits_and_failure_ordering_stop_before_manifest_publication` in
`tests/new_file_preparation.rs` explicitly guard early limit rejection. Preserve
that behavior for known structural limits using a bounded preflight pass;
avoid an accidental change to upload-first limit checking.

`put_immutable` and `resolve_ambiguous_immutable` already reconcile existing or
uncertain object creation by exact readback. Pages must use the same path,
with page-specific bounds. Dependent parents and roots cannot be acknowledged
until child creation is confirmed. A changed root is still not publication.

The current truncate implementation reads the old partial final block even
when shrinking far below it. The proposed lazy contract deliberately verifies
only accessed data: a shrink verifies the new retained tail and prunes fully
discarded subtrees; extension still verifies old partial EOF padding before
making new bytes visible. This changes when corruption in discarded content
is detected, not the bytes retained by a successful truncate.

## PostgreSQL and Application Boundaries

The user clarified that paging must not modify `w9pt-fs-state`, including its
test fixtures. Its production code already uses method-independent publication
validation; the PostgreSQL row codec persists the storage method tag as part
of `ContentRef`, and the storage repository performs method dispatch. Page-
boundary publication coverage therefore belongs in
`crates/w9pt-fs-state-postgres/tests/paged_content.rs` rather than in the shared
state conformance fixture.

`crates/w9pt-fs-state-postgres/src/read.rs::read_scan` already implements
continuation cursors with item/byte limits. This is filesystem-record paging,
not a block index. Its inode entity and `row_codec.rs` persist the existing
`ContentRef` summary; the state-store `PublishContent` commit owns root, size,
generation, and mutation-result atomicity.

The TCP/WebSocket fixture does not yet compose the production state and content
layers. Its live tests must not be described as proof of paged
PostgreSQL/BlockSplit filesystem integration. The state handoff can instead be
tested directly with the existing adapter API.

The SeaweedFS repository matrix currently covers four logical blocks. Extend
it with sparse data at real leaf/branch boundaries so pages are exercised
without allocating enormous files or filling its tmpfs storage. Continue to
construct independent clients and run qualification probes before the private
compatibility wrapper; SeaweedFS remains unsupported in production.

## Conventions and Tests to Reuse

- Typed configuration, limit, range, format, corruption, target, conflict, and
  ambiguity errors; extend missing-object classification for map pages.
- Checked arithmetic and precomputed encoded sizes before allocation.
- Canonical little-endian encodings with explicit tags, bounded collections,
  sorted unique entries, and independent golden vectors.
- `MemoryTarget` traces and before/after failure injection, with independent
  repository instances sharing only authoritative target backing.
- Byte-vector models for bounded files and sparse interval models for huge
  logical ranges. Test traversal work with synthetic maps that do not hold a
  huge target dataset in the same heap under measurement.
- Existing format, new-file, range, publication, deterministic replay,
  crash-recovery, and repository conformance suites.

## Spec Integration

There is no `.dev/specs/` directory yet; no bootstrap is needed because
`.dev/project.md` and the active change workspace already exist. Baseline
requirements are the approved `add-storage-methods` and
`add-seaweedfs-storage-integration-tests` deltas. This proposal records
modifications under the existing `storage-methods` domain and adds live paging
requirements under `seaweedfs-storage-integration`.

The older storage requirement `Target-Authoritative State` describes the
standalone publisher. Its scope must not be expanded into a second clustered
authority; the later state-store contract and `AGENTS.md` remain explicit.

## Confidence and Remaining Evidence

Code exploration and architecture analysis agree on the whole-map allocation
paths, unchanged PostgreSQL handoff, required failure semantics, and feasible
bounded tree structure. Exact buffer accounting and operation traces remain
implementation acceptance work. This proposal contains no benchmark, capacity,
or measured process-memory claim.
