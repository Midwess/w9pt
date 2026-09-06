# Design: Paged BlockSplit Manifests

## 1. Authority and Scope

The state-management crate is unchanged by this feature. Its existing
`ContentRef`/`PreparedContent` handoff carries storage-owned information and
checks file identity, base, size, generation, mutation, and fencing invariants
without executing method-specific logic. PostgreSQL serializes and publishes
the reference; only `w9pt-fs-storage` interprets Raw/BlockSplit or traverses pages.
Keep page-aware test fixtures outside `w9pt-fs-state` as well.

The selected layout is:

```text
PostgreSQL inode: ContentRef(root key, root digest, size, generation, method)
  -> immutable compact file manifest
       -> optional immutable map root
            -> bounded branch pages
                 -> bounded leaf pages
                      -> immutable 32 KiB data payloads
```

One metadata transaction continues to select the current file version. Every
read uses one captured `ContentRef` throughout traversal. No listing, mutable
per-block pointer, shared cache, or process-local state is needed to resolve
content. `ObjectHeadPublisher` remains available only for standalone use and
conformance; it is not an additional authority in clustered mode.

This proposal changes storage paging and its resource behavior. The repository
`read` API can continue returning a bounded `Vec<u8>` and mutation APIs continue
returning `PreparedContent`; file handles and caller-buffer I/O adapters are
separate work.

## 2. Version Boundary

Use current storage envelope major/minor `2/0` and a `v2` private keyspace for
heads, compact manifests, mapping pages, and payloads. The new implementation
accepts only this current format. Raw's byte semantics, the BLAKE3 algorithm,
identity representation, and BlockSplit's 32 KiB plaintext size remain the same;
their current encoding/version identifiers and fixtures are updated together.

Remove the flat-map variant, its whole-map constructors/helpers, obsolete
`max_blocks` allocation policy, and obsolete version-specific aliases. Do not
add a second `StorageMethod`, mixed-version reader, converter, dual writer, or
automatic migration. Unsupported keys/envelopes fail with typed errors.
Existing development databases and private object prefixes must be recreated
by the operator when adopting the new build; implementation/tests must not
delete arbitrary operator data to enforce this.

No PostgreSQL column or new table is needed if `ContentRef` keeps its current
fields. References from an incompatible build fail when their key/root is
validated. Versioning the content format does not itself imply a new SQL schema.

## 3. Fixed Sparse Radix Profile

The current profile persists and validates:

- Logical data block size: 32,768 bytes.
- Leaf capacity: 128 consecutive block slots.
- Branch capacity: 128 child slots.
- Seven index bits per page level.
- Page levels: leaf `0` through maximum branch `6`.

For level `L`, a page covers `2^(7 * (L + 1))` block indexes. Its first block
is aligned to that coverage. A branch's slot `s` covers the corresponding
aligned subrange at level `L - 1`; a leaf slot identifies `first_block + s`.
Store only occupied slots, in increasing order without duplicates. An absent
slot represents a hole; never persist an empty leaf or branch.

| Level | Maximum logical coverage |
| --- | --- |
| 0 | 4 MiB |
| 1 | 512 MiB |
| 2 | 64 GiB |
| 3 | 8 TiB |
| 4 | 1 PiB |
| 5 | 128 PiB |
| 6 | The full 49-bit block-index domain |

The file root starts at block zero and uses the smallest level covering its
highest materialized block. All-hole files have no map root, regardless of
logical size. Root growth adds ancestors only as needed. Root collapse removes
sole-slot-zero ancestors until the minimum level is reached. Internal single
child pages are otherwise valid because their level fixes index routing.

All routing uses checked block-index arithmetic. The top-level coverage ends
at `2^49` blocks, equivalent to an exclusive byte endpoint of `2^64`; never
calculate that endpoint in a `u64`. Use block units or checked wider temporary
arithmetic, and still enforce the existing `u64` logical size/range contract.

## 4. Root and Page Records

The following shapes describe contracts; exact Rust names follow the existing
checked-type conventions during implementation.

```text
FileManifest:
  file_id, content_generation, logical_size
  storage_method, representation
  Raw: optional BlobRef
  BlockSplit: persisted radix profile, optional PageRef

PageRef:
  exact private key
  digest of complete encoded page
  exact encoded byte length
  level, first_block
  materialized_block_count: u64
  highest_materialized_block: u64

LeafPage:
  file_id, first_block, profile
  sorted [(slot, BlobRef)]

BranchPage:
  file_id, first_block, level, profile
  sorted [(slot, PageRef)]
```

The optional root `PageRef` carries authenticated count and highest-index
summaries. Leaf summaries are derived from entries; branch summaries are
derived from immediate child summaries using checked sums/maxima. They support
file-count quotas, EOF checks, root normalization, and suffix pruning without
enumerating descendants. Positive counts must fit the page/subtree coverage.

Reuse current `BlobRef` fields and exact payload keys in leaves. Do not encode
the full logical filesystem path or require a global index-to-hash table.
File identity and block index remain bound to every payload reference.

Use distinct leaf/branch object kinds in the checked envelope. Validate:

- Root key/preparation provenance, root digest, file/generation/method/size.
- Object kind, supported version/profile, complete length, and checksum.
- Expected page digest/encoded length and matching file, level, and range.
- Entry count before allocation, actual bytes remaining, ordered unique slots.
- Canonical keys, prefix, file ownership, child range, and descending level.
- Count/highest-index summaries and consistency with the selected logical EOF.

Page identity is file + level + covered range; page creation is also bound to
its preparation key. A reused page may originate in an earlier file generation.
Requiring its creation generation to match the new root would defeat reuse.

Validate all entries and immediate references of a loaded page, but do not
fetch unrelated children to validate their contents. Summaries authenticate
what a parent asserts; they do not certify unvisited descendants. Missing
referenced pages/payloads are errors, never holes. Wrong levels/ranges and the
strict depth decrease reject cyclic or redirected traversal.

## 5. Keys and Exact Immutable Creation

Retain the existing preparation identity scheme under `v2` for roots and data.
Add mapping-page keys of the form:

```text
<prefix>/v2/maps/<file>/<mutation>/<base-generation>/<base-digest>/
  <operation-fingerprint>/<attempt>/<level>/<first-block>
```

The wrapped line above is one object key. Components use canonical fixed-width
lowercase hex, including a two-digit level and sixteen-digit block index.
Repository construction must account for the longest new key before accepting
the private prefix. Keys never depend solely on an attempt counter.

Within one preparation identity/attempt, finalize each changed page location
once after all its changed children are finalized. Uploading intermediate
versions of the same ancestor under one key would cause an immutable collision.
Use an ordered bottom-up frontier to avoid that case.

Page creation uses the repository's existing exact-byte reconciliation for
`AlreadyExists` and ambiguous immutable PUTs, with page-specific byte bounds.
Do not upload an ancestor or return a prepared root while any dependency's
durable outcome is unresolved. Reuse exact references for untouched pages.

## 6. Read Traversal

1. Validate the request limit and checked logical range; load the compact root
   and bind it to the caller's `ContentRef`.
2. Clamp to EOF. Empty or wholly beyond-EOF reads need no mapping/payload GET.
3. Route the first intersecting block through the sparse tree.
4. Maintain a bounded cursor stack. Reuse ancestors and the current leaf while
   traversing the request in increasing block order; release pages after
   leaving their range.
5. Synthesize zeros for an absent slot or subtree. Fetch each intersecting
   materialized payload with its exact block-object size bound, verify its
   hash/length and relevant final padding, and copy only requested bytes.
6. Release the temporary payload buffer and continue until the bounded output
   is complete. No hot-path full-map collection is permitted.

A cold one-block lookup requires one manifest plus at most seven page GETs,
then at most one payload GET. A contiguous multi-block read reuses its path;
it must not restart at the root for every block. No prefetch or shared cache is
required in this proposal.

`load_manifest`, `validate_content`, standalone root loading, and
`sync_content` validate the root and its immediate reference description. They
do not recursively scan the map or certify all file payloads. Durability comes
from ordered acknowledged preparation and authoritative publication; checking
every stored byte for later corruption is a different operation.

## 7. Bounded Create and Write Preparation

Preserve early structural rejection with two bounded passes over the same
immutable base and caller-owned request bytes.

### Preflight pass

- Check request/range/identity/generation limits first.
- Traverse affected paths and any necessary old-EOF verification or root-
  normalization spine, using a bounded frontier. For partial blocks,
  fetch/verify old data as needed to compute the replacement digest and zero
  status; full overwrites do not read the superseded payload.
- If a write extends EOF while retaining an old partial final block, verify
  that block's old padding before the write can expose it, even when the write
  starts several blocks later. A write replacing that entire old block needs
  no superseded-payload read. Account for this bounded extra path in both passes.
- Compute resulting leaf entries and page references one page at a time,
  including no-op reuse, root growth/collapse, subtree counts, exact encoded
  sizes/key lengths, and planned page work. Propagate summaries bottom-up.
- Validate the result's count quota, every new page/root encoding bound, and
  conservative peak working-set and planned-work bounds before any PUT.
- Retain only bounded operation/root summary information, not all resulting
  pages or payload buffers. A no-op returns the original content reference and
  performs no uploads.

### Preparation pass

- Traverse in the same order against the same immutable root; repeat necessary
  page/payload reads instead of retaining an entire file map or write plan.
- Construct one canonical replacement block at a time. Reuse matching old
  digests, omit zero blocks, and otherwise PUT its new immutable payload.
- After exact durable acknowledgement, retain only the payload reference and
  release the block buffer. Finalize a leaf once all request changes to that
  leaf are known, then finalize affected ancestors once their child updates
  are complete. References to untouched subtrees pass through unchanged.
- Create operations use the same builder with an absent base; sparse first
  writes never enumerate or allocate their leading hole.
- Confirm the final result agrees with preflight, upload the compact manifest,
  and return `PreparedContent` only after all reachable new dependencies are
  acknowledged.

The two passes may fetch the same immutable page or partial block twice. Work
budgets include both passes. This is a deliberate I/O tradeoff for preserving
existing early-limit behavior and bounding memory. Do not turn pass-one output
into an unbounded list to avoid the repeated reads.

Target failures, detected corruption, cancellation, and target-driven retry
exhaustion can still occur after uploads start. They return no successful new
preparation and leave only unreachable objects; they never change the current
inode or standalone head. A metadata conflict requires the existing fresh
base/revalidation/reprepare workflow.

## 8. Truncate and Extension

Truncate uses the same bounded preflight/preparation protocol. Check the new
tail, page/root encodings, count, and planned work/memory requirements before
any immutable PUT, then prepare the affected boundary incrementally.

Shrink traverses the new EOF boundary and retains subtrees wholly below it.
References to subtrees wholly above EOF are detached without fetching their
pages or payloads. Counts/highest-index summaries support bounded pruning and
root normalization. Empty pages disappear from the new tree.

Normalization may read a bounded sole-slot-zero spine inside a retained,
unchanged subtree to recover a lower root reference. Reuse the lower retained
reference without rewriting page contents; redundant ancestor wrappers are
simply absent from the new tree. This does not enumerate the retained subtree
or fetch discarded pages.

For a materialized final retained partial block, verify its bytes, zero the
discarded tail, and upload/reuse/omit the resulting canonical block. The new
root's logical size controls visible EOF. Shrink to zero needs no old payload
or discarded-page read.

Extension reuses all valid existing mappings and changes logical size without
materializing the gap. If the old EOF is inside a materialized block, verify
its padding before extension can expose those bytes. No-op truncate reuses
the existing reference. Re-extension after shrink must not rediscover any
detached subtree through listing or fallback to an older root.

This explicitly replaces the current shrink behavior that eagerly verifies
the old final payload even if that payload is completely discarded. Corruption
in retained/accessed data still fails; discarded data is not implicitly scrubbed.

## 9. Limits and Memory Accounting

Proposed defaults, enforced and checked during implementation:

| Limit | Default / hard profile rule |
| --- | --- |
| `max_manifest_bytes` | 4 KiB, now a compact root/head byte limit |
| `max_map_page_bytes` | 256 KiB including envelope |
| Page slots | 128; fixed persisted profile |
| Maximum page levels | 7; fixed structural ceiling |
| `max_materialized_blocks` | `2^49`, `u64` quota; no flat allocation implied |
| `max_map_working_bytes` | 8 MiB per operation |
| `max_map_page_reads` | 4,096 logical page GETs per operation |
| `max_map_page_writes` | 2,048 logical page PUTs per operation |
| Read/write request limits | Existing 8 MiB defaults |
| Raw/object/key/retry limits | Existing limits; revalidate relationships |

Counters cover both passes and repository-level page reconciliation; SDK
transport retries remain subject to the adapter's separate bounds. Check work
limits before dispatch. Preflight checks known planned limits; dynamic failures
during preparation still cannot publish an incomplete root.

Do not confuse the encoded page cap with heap usage. The operation budget must
charge or conservatively reserve for live encoded page/root buffers, decoded
entry vectors, key capacities, cursor/frontier frames, preflight state, encode
buffers, and immutable-PUT/readback copies. Bound payload working buffers to a
constant number of canonical blocks plus their envelopes. Request/output bytes
remain separately bounded and documented. No per-file retained page dictionary
or whole-request list of payload buffers is permitted.

Check configuration and allocation/encoding relationships before amplification.
With 128 entries and the current 1,024-byte key bound, a 256 KiB page provides
room for the full current reference records; derive and test the actual worst
case from the implemented codec. Smaller configured page budgets may reject
dense pages explicitly. Validate enough working budget for the selected
algorithm/profile or fail before crossing it.

The resource contract is independence from total materialized file blocks,
subject to fixed depth, pages, and request limits. It is not an allocator/RSS,
SDK-buffer, PostgreSQL-server, or process-wide concurrency guarantee. Global
admission control remains caller work.

## 10. Publication and Retention

The dependency order is a graph, enforced child-before-parent:

```text
changed payloads
  -> their leaf pages
  -> their branch ancestors
  -> compact immutable manifest
  -> authoritative metadata transaction
  -> durable operation/session result
  -> 9P response
```

Independent branches may be prepared incrementally; every particular page is
uploaded only after its referenced new children. There is no requirement to
hold all payloads or all leaves until every other branch is finished.

PostgreSQL publishes the same bounded `ContentRef` fields with inode size,
times/generations, and mutation result. The state adapter does not fetch pages
or become a content-layout engine. Standalone CAS tests use the same prepared
root handoff through their test publisher.

Old roots can share pages and payloads with new roots. Writes, truncates,
failed attempts, and cache eviction must not eagerly delete those objects.
No GC implementation is added; future GC must follow transitive page reachability
and reader/snapshot protection. Unreachable preparations are an existing,
explicit storage cost until that work exists.

## 11. Validation and Test Evidence

- Independent golden root/leaf/branch vectors; unsupported v1 keys/envelopes
  and malformed lengths, ordering, profile, summaries, identity, digest, and
  page references fail before unbounded work.
- Small byte-vector models plus sparse interval models at real 128-slot and
  higher-level boundaries, including highest valid `u64` positions.
- Instrumented exact GET/PUT traces: no unrelated subtree reads, ancestor reuse,
  full overwrite without old-payload GET, one finalization per changed page,
  deterministic retries, root growth/collapse, and no-op zero PUTs.
- Suffix pruning without reads of discarded pages, tail zeroing and extension,
  all-hole files, and corruption tests that distinguish holes from missing data.
- A write beyond EOF with an untouched old partial final block verifies its
  padding before exposing the gap; a full overwrite of that old block still
  performs no superseded-payload GET. Root collapse reads only its bounded
  normalization spine inside an unchanged retained subtree.
- A synthetic on-demand target for large dense maps beyond the old flat-map
  bound; compare small-range operation work and measured live repository
  accounting at increasing file sizes. Exclude backing-store fixture memory
  from working-set measurements and do not allocate a huge dense byte vector.
- Failure before/after block, leaf, branch, root, and publication operations;
  exact immutable reuse and ambiguity handling; independent client reads must
  observe a complete selected version.
- PostgreSQL adapter integration tests round-trip the paged root through existing
  fields and exercise conflict/reprepare, result replay, and reopen.
- Extend the real S3/SeaweedFS repository matrix using sparse indexes across
  leaf and branch boundaries, with a bounded number of payload objects. Existing
  qualification and deterministic SDK ambiguity tests remain authoritative.

SeaweedFS execution remains required in its Compose job. Live Amazon S3 tests
retain their existing opt-in environment and bucket/credential requirements;
record them as not executed if unconfigured rather than provisioning resources
or treating an opt-in skip as successful live qualification.

The acceptance gates are structural and deterministic; no numerical throughput
or whole-process memory promise is made before measurement.
