# Codebase Analysis: add-content-compression-encryption

Generated: 2026-09-06
Revised for state-owned wrapped per-file keys; proposal only.

## Scope and Existing Work

The current worktree contains implemented v2 paging and unrelated uncommitted
changes. This proposal revision changes documents only. It replaces the former
caller-managed keyring design with one external master KEK, generated per-file
DEKs, and wrapped-key persistence in generic state metadata.

The earlier no-state-edit constraint is superseded for this feature. The
architectural separation is retained: state validates bounded records and
transaction relationships, PostgreSQL persists them, and storage/orchestration
performs cryptographic computation through explicit caller-owned inputs.

The [ZeroFS research](../../research/zerofs-compression-encryption.md) preceded
the initial draft. The general wrapping-key/data-key hierarchy is useful;
w9pt's byte format, preparation identity and authority model remain independent.

## Storage Representation Seams

| Source in `crates/w9pt-fs-storage/` | Current behavior / needed work |
| --- | --- |
| `src/config.rs`, `format/manifest.rs` | Identity/None only; separate file policy and actual encoded payload descriptor |
| `repository.rs::{load_payload,store_payload}` | Plain checked envelopes; central bounded transform seam |
| `expected_map_page`, `store_map_page`, `prepare_manifest` | Exact stored-object bytes/digests; encryption must be included in both passes |
| `layout/{raw,block_split}.rs` expected blob helpers | Stored size assumes plaintext; share exact transform planning |
| `layout/block_map.rs::MapBudget` | Charge multiple page/frontier representations; include codec/crypto scratch |
| `keys.rs`, `ids.rs` | Visible unkeyed operation fingerprint; derive protected naming token from the file DEK |
| `publisher.rs::load_content` | Additional root decoder and name-derived provenance; replace with common checked body decoding |

Paged preflight rejects a different second-pass root. Exact AlreadyExists and
ambiguous PUT reconciliation also require identical encoded bytes. Randomness
belongs in one-time file-key generation before authoritative selection, never
in independent object-encoding calls. Compression encoder reproducibility and
exact framing remain necessary even with deterministic SIV.

Blob digests identify canonical plaintext; root/page digests identify complete
stored objects. Preserve those distinctions and keep the ContentRef shape
bounded. PreparedContent needs an additional nonsecret context/policy/key
binding for authoritative equality checks.

## State Creation and Retirement Constraints

The existing state model supports a regular inode with `content: None`, size
zero and data generation zero. Generic inode insertion does not publish prepared
content, and duplicate changes cannot insert and publish the same inode in one
commit. This provides a clean boundary for atomic empty-file creation with its
pinned content metadata before any initial S3 content preparation.

Both memory and PostgreSQL adapters perform mutation-ledger lookup before
ordinary commit/fence validation. Random DEK and wrapper bytes are generated
allocation results, not inputs that change a semantic retry fingerprint. A
matching replay must reread the originally committed metadata rather than use
the current attempt's candidate. Different semantic operands still mismatch.

`records.rs` requires unpinned zero-link inodes to be retired. Storing the only
wrapped key inline would lose it at final close while old roots can still
exist. A separate ContentMetadata record, retained independently of the inode,
avoids weakening existing orphan semantics. It must not have an inode-delete
cascade or a generic eager delete path.

## Proposed Generic Record Boundary

A ContentMetadata record is keyed by filesystem and stable content FileId and
holds owner inode ID, stable context ID, immutable bounded policy bytes,
optional immutable key commitment, optional bounded wrapped-key bytes and a
record revision. Storage owns the policy/wrapper encoding. State validates
bounds, key/value identity, owner/context associations and permitted changes.

Every regular inode references the same context identity. Inserting the inode,
metadata, dentry/open/pin and mutation result is one declarative transaction.
Metadata records may outlive owner inode retirement, but their original file/
inode/context identities can never be reused by a new inode.

`validate_publish_content` currently checks mutation/base/file/size/generations
only. Extend it to compare PreparedContent's nonsecret context/policy/key
binding with the inode and selected metadata record, with an exact metadata
revision precondition. This adds no decryption or method dispatch to state.

Rewrap requires a dedicated fenced/revision-checked transition that changes
only wrapped bytes. Generic Replace/Delete must not bypass immutable policy,
context/key commitment or retention rules. Cryptographic equivalence of a
rewrapped DEK is verified by the external storage helper; state verifies only
opaque stable values and revisions.

## PostgreSQL Integration

Add a normalized `public.w9pt_fs_state_content_metadata` table and regular-inode
context columns/bindings. Use named constraints and deferred inode-to-context
foreign keys where appropriate; do not cascade context deletion from inodes.
The context retains filesystem ownership through the adapter authority record.

Update schema entities/current code-first schema, checksums/catalog fixtures,
row/key codecs, point reads, bounded scans, retained-byte estimation, writes,
locking/constraint classification, privilege validation and cleanup tests. This
is a direct current development-schema change, not an upgrade/import path.

Reads of inode plus content metadata use one consistent snapshot. A context
scan is explicitly bounded by items and bytes. The database stores wrapped
ciphertext and public identifiers; it never receives plaintext KEKs/DEKs or
imports cipher/compression implementations.

## Cryptographic Key Separation

The external master KEK derives only a wrapping subkey. The selected committed
file DEK derives naming and per-exact-object encryption subkeys. Do not include
the master ID, wrapper bytes or metadata revision in S3 naming, content KDFs or
AAD; otherwise master rewrap would invalidate existing immutable data.

A stable context/key commitment helps a crypto helper detect an incorrect DEK
after unwrapping and preserve it during rewrap. It is generic immutable metadata
to state, not an algorithm decision. File policy remains unchanged on ordinary
writes and rewrap; actual DEK rotation is separate future work.

## Additional Affected Files

- State `records.rs`, `read.rs`, `commit.rs`, `bounded.rs`, `limits.rs`, identity/
  value types, `change.rs`, exports, memory reference and reusable conformance.
- PostgreSQL schema/codec/read/write/commit/validation/sqlstate modules and
  migration/catalog/conformance tests.
- Focused filesystem orchestration helpers for candidate creation, committed
  context reload, per-operation unwrap and administrative rewrap. Do not finish
  unrelated namespace/transport operations in this change.
- Existing S3/test runner feature wiring and optional packages as identified
  in the representation blueprint.

## Tests and Evidence

Extend the representation vectors/model/failure/resource suite and add opaque
state conformance for atomic create, winning-key replay, context mismatch,
stale revisions/fences, rewrap and independent lifetime. Use deliberately fake
opaque policy/envelope bytes in generic state tests; cryptographic correctness
belongs to storage and cross-layer integration tests.

Direct PostgreSQL tests use independently opened clients and public test
masters, proving that new files can be accessed after all local key state is
lost. Require failures before namespace commit, after durable context but before
upload, at each S3 dependency, and around publication/rewrap commit. Observe
same DEK selection, old-or-new content and safe retained keys.

The current proposal introduces no implementation or performance claim.
Dependency feature/MSRV/license/advisory review, cipher cleanup and measured
scratch peaks remain implementation gates. Delta domains now include storage,
filesystem state, PostgreSQL and live integration; `.dev/` needs no bootstrap.
