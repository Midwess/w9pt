# Tasks: add-paged-block-manifests

## Progress: [31/31]

## 1. Contract and resource model

- [x] 1.1 Finalize the v2 root/leaf/branch byte contracts, 128-slot profile, seven-level routing, subtree summaries, and canonical root normalization.
- [x] 1.2 Define checked root/page/count/work/memory limits, two-pass preflight accounting, and typed errors; replace the flat `max_blocks` allocation policy with a u64 materialized-count quota.
- [x] 1.3 Map all flat-manifest consumers and current-version fixtures; record the no-compatibility reset policy and unchanged PostgreSQL publication boundary.

## 2. Formats, keys, and page I/O

- [x] 2.1 Implement compact manifest, PageRef, leaf, and branch codecs with independent golden fixtures and pre-allocation size/count checks.
- [x] 2.2 Introduce the v2 envelope/keyspace and location-bound immutable page keys; update longest-key validation and reject v1 data without fallback.
- [x] 2.3 Implement exact bounded page GET/PUT and page-specific missing/corruption classification using existing immutable collision/ambiguity reconciliation.
- [x] 2.4 Validate file/range/level/profile, exact bytes/digest, sorted slots, child summaries and EOF relationships locally before following references.

## 3. Lazy read traversal

- [x] 3.1 Implement checked sparse radix routing and a bounded ordered cursor that retains only its active path and skips absent subtrees.
- [x] 3.2 Replace flat-map read lookup with the cursor; fetch/verify/copy/drop touched payloads sequentially and reuse paths for neighboring blocks.
- [x] 3.3 Adapt load_manifest, validate_content, sync_content, and standalone publisher loading to compact-root validation without a recursive map scan.
- [x] 3.4 Test empty/EOF reads, hole versus missing-page behavior, unrelated-subtree isolation, exact page-work limits, and accessed-page corruption.

## 4. Bounded create and write preparation

- [x] 4.1 Implement a read-only preflight pass computing resulting page/root sizes, counts, identities, no-op state, and conservative resource/work bounds before PUTs.
- [x] 4.2 Implement ordered create and write-from-new preparation, including sparse high offsets and initial all-zero content.
- [x] 4.3 Implement existing-file partial/full writes with exact old-reference reuse, no old-payload fetch for full overwrite, deterministic second-pass recomputation, and old partial-tail verification when a distant write extends EOF.
- [x] 4.4 Implement the bottom-up mutation frontier: upload and release changed payload buffers, finalize each leaf/ancestor location once, and store the root only after its dependencies.
- [x] 4.5 Add operation-local accounting for encoded/decoded maps, key capacities, frontier, encoding and PUT/readback copies; prove bounded payload staging and reject known limit violations before upload.

## 5. Truncation and sparse extension

- [x] 5.1 Implement two-pass truncate preflight/preparation, suffix pruning through subtree summaries, bounded retained-boundary/normalization-spine traversal, empty-page removal, and root collapse without rewriting unchanged retained pages.
- [x] 5.2 Implement verified final retained-block tail zeroing and truncate-to-zero without reading completely discarded subtrees; update the prior eager old-tail expectation explicitly.
- [x] 5.3 Implement sparse extension and old partial-tail verification; prove shrink/re-extension cannot resurrect removed references or padding.

## 6. Deterministic conformance and failure boundaries

- [x] 6.1 Extend malformed/golden tests across compact roots and both page kinds, including unsupported versions, wrong identities/ranges, cycle-like references, summaries, and exact allocation boundaries.
- [x] 6.2 Extend byte-vector and sparse interval models across leaf/branch boundaries, root growth/collapse, zero omission, no-op reuse, and the highest valid u64 offsets.
- [x] 6.3 Add an on-demand synthetic large-map target and working-set instrumentation; prove small-range reads/writes and suffix pruning do not scale with total materialized entries.
- [x] 6.4 Inject before/after failures at payload, leaf, branch, root, and publication operations; cover exact AlreadyExists and ambiguous readback without publishing incomplete dependencies.
- [x] 6.5 Test independently constructed clients, old/new root visibility, matching/mismatching preparation identities, disjoint/overlapping page writes, and conflict re-read/reprepare.

## 7. Consumers and live integration

- [x] 7.1 Update current format/key fixtures and downstream ContentRef consumers, remove superseded flat helpers/aliases, and preserve Raw method behavior under the current format.
- [x] 7.2 Add PostgreSQL adapter integration tests that publish/reopen a paged root through the unchanged state API, retaining atomic size/generation/result publication and replay semantics; leave all w9pt-fs-state sources and tests unchanged.
- [x] 7.3 Extend reusable repository conformance with sparse cases crossing actual 128-slot leaves and higher branch ranges; use bounded map inspection instead of flattening helpers.
- [x] 7.4 Extend real S3 coverage and run the required SeaweedFS compatibility bridge; execute live AWS tests only when their existing opt-in configuration is supplied, record unconfigured live checks accurately, and retain qualification guards, bounded objects, scoped prefixes, and cleanup.

## 8. Documentation and required gates

- [x] 8.1 Update storage/root/project guidance at implementation time for paged validation, immutable dependency ordering, new limits, data reset, and remaining global memory/GC limitations.
- [x] 8.2 Run root and standalone formatting, relevant and workspace tests, Clippy with warnings denied, rustdoc, dependency-isolation/license checks, and existing audit gates.
- [x] 8.3 Run the bounded live Compose suite with page-boundary coverage; verify shell/Compose configuration and scoped teardown, then record actual resource/test evidence.

## Notes

- User boundary refinement: remove the page-specific offset from shared state
  conformance and move coverage into the PostgreSQL adapter's external test.
  Method dispatch remains in the content repository; the state API and SQL
  schema are unchanged. `tests/paged_content.rs` publishes and reopens a
  level-two root with atomic size/attributes/result checks and exact replay,
  then verifies its bytes through a fresh repository using the opposite
  creation default. The live PostgreSQL test and unchanged adapter conformance
  passed in isolated Compose project `w9pt-paged-handoff-1257200`, with verified
  teardown. All 65 state tests and PostgreSQL all-target/all-feature Clippy
  with warnings denied passed; the state crate has no diff from its baseline.
- Post-implementation code review fixes validate the true longest v2 key,
  use authenticated page lengths as target GET bounds, reject impossible page
  lengths and out-of-domain page ranges, check public summary arithmetic, and
  release first-pass update maps before replay. Mapping-memory accounting now
  reserves B-tree entry overhead plus four page-sized representations per
  active rewrite level; focused regressions and the locked all-feature workspace
  test, Clippy, and rustdoc gates pass after these corrections.
- Implemented current format v2 with compact roots, leaf/branch pages, v2-only
  keys/envelopes, range-local reads, bounded two-pass mutations, suffix pruning,
  root normalization, and unchanged PostgreSQL `ContentRef` publication fields.
- The on-demand 131,073-materialized-block fixture (one entry beyond the former
  flat limit) read one selected byte with exactly three map GETs and one payload
  GET. Its small write used 14 map GETs across both bounded passes and three map
  PUTs; suffix pruning stayed below the asserted 16 map GET bound. Fixture backing
  generation is excluded from repository working-byte accounting.
- Default operation bounds are a 4 KiB compact manifest, 256 KiB map page,
  8 MiB map working budget, 4,096 page reads, 2,048 page writes, and a `2^49`
  materialized-block quota. These are deterministic operation-local accounting
  bounds, not a process RSS or global admission claim.
- Root and standalone formatting, locked all-feature workspace tests, Clippy and
  rustdoc with warnings denied, dependency/source isolation, license metadata,
  and RustSec policies passed. The root audit ignores RUSTSEC-2026-0235 and
  RUSTSEC-2023-0071 only after the enforced all-target tree check proves their
  locked packages unreachable; the standalone lockfile audits cleanly.
- `test/run-integration.sh` passed against digest-pinned SeaweedFS 4.42 and
  PostgreSQL 18.6. The live repository matrix crossed 127/128 and 16383/16384,
  PostgreSQL state conformance passed, TCP and
  WebSocket profiles passed, and the post-review rerun under Compose project
  `w9pt-integration-1209141`
  left no labeled containers, networks, or volumes. Live AWS was unconfigured
  and therefore not executed; SeaweedFS remains unqualified compatibility evidence.
- Scope remains S3 block-map paging. PostgreSQL record pagination, authority
  ownership, public file-handle adapters, production GC, shared caches, and
  global concurrency admission remain separate work.
