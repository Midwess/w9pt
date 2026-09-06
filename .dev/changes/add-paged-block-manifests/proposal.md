# Proposal: Paged BlockSplit Manifests

Status: approved

## Summary

Replace the inline BlockSplit block map with a small immutable root manifest
and a sparse tree of bounded immutable mapping pages. Positioned reads fetch
only the mapping paths and data blocks covering the requested range. Writes
and truncates prepare changed blocks and pages with a bounded working set,
reuse unaffected subtrees, and return one portable `ContentRef` for atomic
publication by the filesystem metadata transaction.

PostgreSQL continues to store filesystem metadata and the current content
reference. Its existing metadata pagination is unchanged. S3-compatible target
storage continues to hold immutable content and mappings under private keys.

`w9pt-fs-state` remains unchanged, including its reusable conformance suite.
It carries the existing content reference and enforces method-independent
publication invariants. The PostgreSQL adapter owns SQL persistence of that
reference; `w9pt-fs-storage` owns Raw/BlockSplit interpretation and map traversal.

## Motivation

BlockSplit already reads intersecting 32 KiB payloads sequentially, but every
read loads the entire flat manifest. Writes and truncates additionally copy
every block reference into a `BTreeMap`; writes stage all changed block buffers
before upload. Memory and mapping work therefore grow with the file's total
materialized block count, even for a small positioned operation.

The current 8 MiB encoded-manifest limit also caps dense files before the
nominal 131,072-block count ceiling for typical private key lengths. Paging
must remove this whole-map dependency, rather than merely move it to an
unbounded root containing a list of all leaf pages.

## Scope

### In Scope

- A compact root plus a sparse, immutable radix tree with 128 slots per page,
  32 KiB data blocks, and at most seven mapping-page levels.
- Persisted tree parameters, independently checked leaf/branch formats,
  authenticated child references, and exact bounded page reads.
- Lazy range traversal with a bounded path/frontier and no target listing.
- Bounded two-pass mutation preparation: preflight known structural limits,
  then upload blocks and pages incrementally in dependency order.
- Copy-on-write create, first write, positioned write, truncate, root growth,
  and root collapse; exact unchanged-reference reuse and sparse zero omission.
- Shrink by detaching whole suffix subtrees without loading discarded pages;
  verified tail handling that prevents discarded bytes from reappearing.
- Explicit per-operation mapping-memory and page-work limits, with tests that
  account for encoded buffers, decoded structures, keys, and temporary copies.
- One incompatible current storage format using a new `v2` keyspace and major
  version; remove the flat representation and reject earlier development data.
- Adapt storage, PostgreSQL handoff, and live S3/SeaweedFS conformance to cover
  actual page and tree-level boundaries.

### Out of Scope

- Source, test, or public-API changes in `crates/w9pt-fs-state`; page-specific
  handoff coverage belongs in PostgreSQL adapter integration tests.
- Moving block maps into PostgreSQL, changing PostgreSQL pagination, or adding
  a per-block metadata table.
- Changing namespace or metadata authority, adding mutable S3 block heads, or
  resolving current blocks through wildcard/prefix listing.
- Adopting `<index>_<hash>` payload keys; existing preparation identities remain
  sufficient. The new keys needed here identify immutable mapping pages.
- Changing 32 KiB logical blocks, Raw semantics, codecs, ciphers, block packing,
  cross-file deduplication, or configurable tree fanout in this format.
- A new file-handle/`Read`/`Write`/`Seek` adapter, caller-buffer read API,
  transport integration, or completion of the filesystem semantic engine.
- Shared page caches, prefetching, a process-wide memory admission controller,
  background write-back, production garbage collection, or an integrity-scrub
  API. These can build on the paged representation separately.
- Production qualification of SeaweedFS or capacity testing on tmpfs.

## Recorded Assumptions

- "The paging one" means paging S3 BlockSplit mappings, following the user's
  decision to retain the current PostgreSQL/S3 authority split.
- Existing positioned repository operations continue to accept bounded input
  buffers and return bounded output vectors. This change bounds internal map
  work and payload staging; it does not claim an entire process RSS ceiling.
- Raw and BlockSplit remain the only storage methods. Paging is BlockSplit's
  current representation, not a third method or a legacy compatibility mode.
- Only referenced objects actually accessed by an operation are verified.
  Root-only validation is not a claim that all descendants were scrubbed.

## Success Criteria

- A cold one-block read fetches one root and no more than seven mapping pages,
  plus at most its materialized payload. Unrelated mapping subtrees are not read.
- Within-page range reads reuse the loaded mapping path. Mapping retention
  depends on page size and bounded depth, not total file block count.
- A small write to a large map neither flattens the tree nor clones all
  references; preparation finalizes each changed page location once per attempt.
- Known request, result-count, root/page-size, and planned resource-limit
  failures are detected before immutable uploads. All failures prevent
  publication of incomplete content.
- Shrink detaches fully discarded subtrees without visiting their descendants,
  and subsequent extension reads zeroes in the discarded range.
- Prepared dependencies become durable before the new root, and the existing
  metadata transaction atomically publishes root, size, generations, and result.
- Byte-model, sparse high-index, resource-bound, corruption, retry, independent
  client, and failure-boundary tests pass, including live multi-page SeaweedFS
  behavior without changing qualification guarantees.
- Root references and PostgreSQL records stay bounded regardless of file size;
  no old-format reader, migration, or dual-write path is introduced.

## Affected Areas

| Area | Impact |
| --- | --- |
| `crates/w9pt-fs-storage/src/format/` | Compact root, page codecs, version boundary, golden fixtures |
| `crates/w9pt-fs-storage/src/layout/` | Lazy cursor, bounded mutation frontier, suffix pruning |
| `repository.rs`, `keys.rs`, `limits.rs`, `error.rs`, `publisher.rs` | Page I/O, identities, budgets, validation and standalone publication |
| Storage tests and reusable conformance | Multi-page model, isolation, failure and memory/work evidence |
| PostgreSQL adapter integration tests | Persist and reopen paged roots through the unchanged state handoff |
| S3 conformance and integration documentation | Live page-boundary coverage and revised format limitations |

## Dependencies

- Applied storage-method, state-store, PostgreSQL, S3-target, and SeaweedFS
  integration foundations. The unfinished semantic-engine proposal is not a
  prerequisite for implementing storage paging.
- Existing `TargetStore` exact bounded reads, immutable creation, and
  ambiguity reconciliation; no new SDK or runtime dependency is required.
- Live AWS checks retain existing opt-in configuration; required local provider
  evidence uses the bounded SeaweedFS Compose fixture.
- Existing BLAKE3 integrity and checked format/range primitives.
- `.dev/specs/` is currently absent. Modified requirements target the approved
  `add-storage-methods` delta; archive that baseline before merging this delta,
  or otherwise resolve the unarchived dependency order explicitly.

## Risks

| Risk | Mitigation |
| --- | --- |
| Root becomes another unbounded page directory | Root holds one optional page reference; branch fanout and depth are fixed |
| Encoded-size caps hide large heap copies | Account decoded keys/vectors, frontier, encoding and immutable-PUT copies |
| Streaming writes weaken existing preflight behavior | Bounded preflight pass precedes mutation pass; immutable input allows repeatable reads |
| Intermediate ancestors collide under one immutable key | Finalize each page location exactly once, bottom-up, within an attempt |
| Truncate traverses every removed block | Authenticate subtree counts/highest indexes and detach by covered range |
| Lazy validation is mistaken for complete integrity checking | Specify root validation separately from accessed-page/payload verification |
| Extra page round trips dominate small operations | Minimal root height and within-operation path reuse; measure traces before caching |
| Old and new development objects are mixed | New keyspace/envelope version; reject old data and require fresh development state |
| Failed preparation leaves more unreachable objects | Preserve old roots and readers; no eager deletion; GC remains separate |

## References

- [Codebase analysis](analysis.md)
- [Implementation blueprint](blueprint.md)
- [Detailed design](design.md)
- [Implementation tasks](tasks.md)
- [Storage requirement delta](specs/storage-methods/spec.md)
- [Live integration requirement delta](specs/seaweedfs-storage-integration/spec.md)
