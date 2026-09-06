# Architecture Blueprint: add-paged-block-manifests

Generated: 2026-09-06
Based on: `analysis.md` and the repository's current storage/state contracts.

## Design Approach

Replace the full BlockSplit vector with a compact immutable manifest that points
to a sparse radix tree. Each immutable leaf or branch has at most 128 slots;
tree depth is at most seven pages. A minimal root keeps small files shallow.
Use exact authenticated references and ordered traversal so memory depends on
the request and bounded frontier rather than the entire file map.

Use the two-pass preflight/preparation algorithm in `design.md`: preserve early
structural-limit rejection, then incrementally upload each changed block/page
once its dependencies are ready. The tradeoff is bounded repeated reads of
immutable inputs. Keep the existing content-preparation/metadata-publication
split and the current `ContentRef` fields.

The `w9pt-fs-state` crate, including its tests, is outside the edit scope.
Keep storage-method interpretation in `w9pt-fs-storage` and SQL serialization
in `w9pt-fs-state-postgres`. Add layout-specific handoff coverage under the
PostgreSQL adapter's external integration tests.

## Design Decisions

| Concern | Choice and reason |
| --- | --- |
| Root structure | One optional page reference; avoids a flat list of all pages |
| Tree | Fixed 128-slot sparse radix profile; seven levels cover all valid block indexes |
| Small files | Minimum root level; avoid seven page fetches for a single low-index block |
| Page references | Exact key, encoded length, digest, level/range, count and highest index |
| Payload references | Retain current BlobRef/provenance rules; no filename/hash redesign |
| Updates | Ordered bottom-up frontier; no repeated immutable ancestor key within an attempt |
| Limit preflight | Read-only bounded first pass, followed by deterministic preparation |
| Shrink | Range-based suffix pruning and summary updates; no discarded-subtree scan |
| Validation | Root and accessed objects; whole-tree audit remains a separate operation |
| Compatibility | One v2 current format; reject prior development data and remove flat APIs |
| State database | Existing PostgreSQL schema/ContentRef ownership; no mapping rows |

## Components and Interfaces

### Checked root and page codecs

`format/manifest.rs` holds compact file metadata; new
`format/block_map.rs` defines checked `PageRef`, leaf, branch, and slot records.
Codecs precompute encoded lengths, bound allocations, and validate local
structure. Page-kind and envelope changes live in `format/envelope.rs`.

`ContentRef` remains a portable reference to one manifest, with the same
bounded summary fields. `load_manifest` returns that compact root. Remove
production APIs requiring all block entries at once; tests must use bounded
page inspection or content reads instead of reintroducing a flattening API.

### Exact page storage

`repository.rs` gains bounded internal page-load/store helpers. They check the
expected encoded length and type-specific cap, verify the parent-selected
digest and page context, and reuse existing immutable readback reconciliation.
`keys.rs` builds and validates page keys from the complete preparation identity
plus level and aligned block start. Key construction checks the longest form.

No new `TargetStore` operation is needed. Retain its exact GET, immutable PUT,
and standalone CAS contract. Missing referenced pages have an explicit error
classification distinct from an absent sparse slot.

### Traversal and preparation frontier

New `layout/block_map.rs` owns checked routing, ordered cursor/path reuse,
subtree summaries, root growth/collapse, and suffix pruning. A bounded path
contains only the pages/frontier needed for the active range. A page is
finalized once all changed children in that range are known.

`layout/block_split.rs` uses this machinery for every current entry point:
create, read, write, first write, truncate, and first truncate. Retain the
existing checked `LogicalRange`/`BlockSpans` machinery. Payloads remain exactly
32 KiB, hash verified, sparse, and zero padded. A write extending EOF may also
need the old partial-tail verification path, and root collapse may read a
bounded spine in a retained subtree; account for both without enumerating or
rewriting unrelated pages. Truncate shares the bounded two-pass preflight.

### Resource limits

`limits.rs` and `error.rs` separate compact-root size, page size, materialized
count quota, logical page work, and resident map working bytes. Budget checks
cover both mutation passes and temporary clones. Test-visible operation
accounting must charge allocation capacity and key bytes, not merely the number
of loaded pages. Caller input/output and adapter buffers have separately
documented bounds; there is no process-wide admission system in this change.

## File Blueprint

### Create

| File | Purpose | Complexity |
| --- | --- | --- |
| `crates/w9pt-fs-storage/src/format/block_map.rs` | Page/ref codecs and canonical validation | High |
| `crates/w9pt-fs-storage/src/layout/block_map.rs` | Cursor, frontier, root normalization, pruning | High |
| `crates/w9pt-fs-storage/tests/paged_block_map.rs` | Model/routing/reuse/growth/truncate cases | High |
| `crates/w9pt-fs-storage/tests/paged_resource_bounds.rs` | On-demand large-map fixture and deterministic resource evidence | High |

Page-specific code may be split into focused modules during implementation if
needed. Keep that split internal; do not introduce a plugin framework.

### Modify

| File or group | Change | Complexity |
| --- | --- | --- |
| Storage `format/{manifest,envelope,mod}.rs` | Compact root, page kinds, v2-only codecs/exports | High |
| Storage `layout/{block_split,mod}.rs` | Replace flat maps/staging with traversal/builder | High |
| Storage `repository.rs`, `publisher.rs` | Page I/O and shared bounded root checks | High |
| Storage `keys.rs`, `config.rs`, `limits.rs`, `error.rs`, `lib.rs` | Profile, current version, keys, budgets and typed errors | Medium |
| Storage `testing/{memory_store,repository_conformance}.rs` | Traces/faults, bounded introspection, true page boundaries | Medium |
| Storage tests `format_golden`, `block_split_model`, `new_file_preparation`, `range_planning`, `publication`, `crash_recovery`, `deterministic_replay` | New format and page invariants | High |
| `crates/w9pt-fs-storage-s3/tests/s3_conformance.rs` | Multi-page provider evidence through existing bridge | Medium |
| `crates/w9pt-fs-state-postgres/tests/paged_content.rs` | Publish/reopen/replay a paged root using the unchanged state API | Medium |
| `README.md`, S3 README, `test/README.md`, `.dev/project.md`, `AGENTS.md` | Update current-format semantics when implemented | Medium |

### Review for Necessary Fixture/Type Updates

- Storage `ids.rs`: preparation fingerprint/version domain and `ContentRef`
  reconstruction must agree with v2 without increasing reference size.
- Storage `format/head.rs`, `layout/raw.rs`: current version updates only;
  preserve Raw and standalone publication semantics.
- Read-only review of state `commit.rs`, `records.rs`, and existing conformance:
  preserve their generic preparation/publication invariants without edits.
- PostgreSQL `row_codec.rs`, schema entities, and adapter conformance tests:
  existing root fields suffice. Do not alter SQL layout merely because the
  S3 manifest changed.
- Semantic-engine reference fixtures and format-specific assumptions: update
  only necessary consumers; do not implement its outstanding operation surface.
- Integration runner/CI: the existing required conformance calls may already
  cover added cases; change wiring only if a separate test target is necessary.

## Implementation Phases and Completion Criteria

1. **Contract:** settle current codec/profile, bounds, identity, and root/local
   validation semantics. All choices in `design.md` must have matching tests.
2. **Formats and I/O:** independent golden and corruption tests pass; exact
   page identity, size and immutable reconciliation work without flat decoding.
3. **Reads:** operation traces prove range-local page/payload access and path
   reuse, with explicit errors for missing referenced data.
4. **Mutations:** both passes remain bounded; known structural failures produce
   zero PUTs; unchanged operations reuse the base; each new page is stored once.
5. **Truncation:** prune without visiting discarded pages, normalize roots, and
   preserve correct tail/extension bytes.
6. **Deterministic evidence:** page/model/failure tests and synthetic large-map
   accounting demonstrate independence from total map size for fixed ranges.
7. **Integration:** independent target/state clients persist and reopen paged
   roots; real provider conformance crosses actual tree boundaries.
8. **Gates and docs:** required workspace checks, bounded live Compose run,
   scoped teardown, and current-format documentation pass together.

The ordered implementation checklist is maintained only in `tasks.md`.

## Testing Strategy

Use independent codec vectors, byte-vector differential models for small
files, and a sparse interval model for huge logical offsets. Exercise indexes
around `127/128`, `16383/16384`, and higher powers of 128 to force leaf and
branch changes without allocating the intervening holes.

Use target-operation traces and operation-local allocation accounting for
memory/work assertions. A virtual target should generate large valid maps on
demand so its authoritative backing storage does not dominate measured
repository memory. Test files beyond the old flat limit with fixed small
read/write ranges and at least two tree heights; assert explicit depth/frontier
bounds, not an unsupported constant-latency or total-RSS promise.

Inject failures before and after every dependency class: payload, leaf,
branch, root, and publication. Repeat exact preparations from independent
clients, force collisions and ambiguous readbacks, race stale publications,
and prove old and new roots never select mixed content. Include limit failures
in preflight and errors after earlier dependency uploads.

For live SeaweedFS, keep the digest pin and production qualification guards.
Cross real page boundaries with a handful of nonzero blocks and bounded range
reads; do not write a dense terabyte file to tmpfs. Existing metadata paging
and transport tests remain independent regression coverage.

Live AWS tests retain their existing opt-in bucket/region/prefix and credential
requirements. Run them when configured; otherwise record the external check
as not executed. Do not provision external resources merely to complete this
proposal's implementation checklist.

Run repository-required formatting, tests, Clippy, rustdoc, dependency/license
and existing audit gates. Run the live Compose suite only as bounded behavioral
evidence. Do not claim paging has completed until the recorded deterministic
resource and failure-boundary assertions pass.

## Spec Integration

`specs/storage-methods/spec.md` adds paged-root/traversal/validation requirements
and replaces the current flat-format, mutation, truncation and amplification
wording. `specs/seaweedfs-storage-integration/spec.md` adds required paged
behavior. Existing state/PostgreSQL ownership requirements remain unchanged.

## Open Decisions

No user decision blocks drafting. Defaults are explicit in `design.md` and
must be validated against the implemented encodings/frontier. If measurement
requires materially different defaults or representation, update this draft
before approval rather than silently broadening the implementation.
