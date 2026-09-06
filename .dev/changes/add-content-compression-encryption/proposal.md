# Proposal: Content Compression and Per-File Envelope Encryption

Status: approved

## Summary

Add optional LZ4 compression and client-side content encryption using one
caller-supplied master wrapping key and a generated random data key for each
encrypted file. Store each wrapped file key, its stable identity, and pinned
storage policy as bounded opaque filesystem metadata in `w9pt-fs-state`,
persisted by PostgreSQL. `w9pt-fs-storage` generates/wraps/unwraps keys through
explicit caller-owned inputs and owns compression, content encryption, blocks,
root manifests, and S3 mapping pages.

The master key protects only file-key envelopes. Content naming and per-object
encryption keys derive from the committed file data key, so rewrapping a file
key does not change its S3 objects. State and PostgreSQL validate/persist opaque
metadata and transaction bindings without executing cryptographic algorithms
or storage-method logic.

## Motivation

The current representation stores uncompressed plaintext. The initial draft
proposed caller-managed data keys; the user now wants the library to manage
per-file key generation and persist wrapped keys with authoritative file state.
This replaces the earlier exclusion of state/SQL metadata changes while keeping
content layout and cryptographic computation out of those layers.

A generated file key must survive retries, node loss, unlink and retained old
content versions. Choosing it only in local memory is insufficient. The design
therefore makes the file context part of atomic file creation and uses only
the committed winner for subsequent content preparation.

Compression and encryption must also preserve paged preflight's exact-byte
predictions and immutable readback. The proposed content profiles remain LZ4
and deterministic AES-256-SIV, with protected naming to avoid exposing the
current plaintext-derived operation fingerprints in S3 keys.

## Research Completed Before Drafting

The [ZeroFS research](../../research/zerofs-compression-encryption.md) preceded
the initial proposal. The revised design uses the general envelope-encryption
separation between a wrapping key and data keys, with an independent state,
object format, retry protocol and implementation. It does not copy ZeroFS code,
formats, key labels, segment layout, or password tooling.

## Scope

### In Scope

- Independently optional LZ4 payload compression and AES-SIV object protection;
  Identity/None remains a supported explicit policy.
- One external master KEK in steady state, used only to wrap/unwrap generated
  32-byte per-file DEKs. Randomness is supplied through a caller-owned secure
  entropy interface; the library performs no hidden RNG or secret lookup.
- A generic bounded `ContentMetadataRecord` owned by state, with a stable file
  context ID, immutable opaque policy, optional key commitment and wrapped-key
  envelope. The regular inode references its context identity.
- PostgreSQL persistence, exact lookup, bounded pagination, transaction checks,
  change notification, and conservative retention for that metadata family.
- Atomic creation of the unpublished empty inode, namespace/open records and
  content metadata; durable winner selection before encrypted S3 preparation.
- Storage-side context reconstruction from committed metadata; load/unwrap only
  the requested file key and release operation-scoped secrets afterward.
- Prepared-content context/policy/key binding validated with the existing inode,
  base, size/generation, authorization, fencing and mutation-result publication.
- Explicit per-record master-key rewrap with revision/fence/replay protection,
  preserving the same DEK and all S3 content. No automatic rotation daemon.
- Independent block compression, encryption of immutable payload/page/root
  bodies, exact deterministic encoding and authenticated bounded decoding.
- Current v3 content format and updated current development SQL schema/fixtures,
  without old-format readers or database/data conversion paths.
- Deterministic and live conformance for both representation behavior and the
  newly persisted key lifecycle.

### Out of Scope

- Moving block maps into PostgreSQL. S3 paged manifests remain content layout;
  state stores their root reference and authoritative file metadata.
- Plaintext master keys or plaintext DEKs in state, SQL, mutation results, S3
  metadata, or diagnostics.
- Password/Argon2 input, a KMS/HSM client, master-key generation/backup, recovery
  tooling, or any mandatory external secret-management product.
- Actual per-file DEK rotation/re-encryption, policy changes to existing files,
  automatic master rotation, key GC, or claims of crypto-erasure on unlink.
- Zstd/additional ciphers, ZeroFS compatibility, packing, caches, write-back,
  global memory admission, and completion of unrelated semantic-engine work.
- Encrypted PostgreSQL metadata, encrypted standalone mutable heads, hidden
  object sizes/access patterns, or a production qualification claim for SeaweedFS.

## Recorded Decisions

- The user's new direction authorizes state types/tests and PostgreSQL schema
  changes for generic file metadata. Their code must still not interpret
  compression/cipher policy or unwrap keys.
- Every managed regular file has a content-metadata record, including plain
  files. Plain records pin policy without generating or wrapping a DEK.
- Records are separate from inodes so they can survive the existing final-close
  inode-retirement rule. Their identities/policies are immutable; only the
  dedicated rewrap transition may replace the wrapped-key envelope.
- Creation is the existing empty `content: None` lifecycle. Creation with
  initial file data in the same atomic semantic operation is not added here.
- One active master is supplied normally. Explicit rewrap temporarily requires
  old and new masters; replacing a configured master alone cannot unlock records
  still wrapped by the old one.
- The LZ4 writer profile remains frozen little-endian 64-bit encoding with a
  deterministic 64-byte savings threshold. Decoder capability is separate.

## Success Criteria

- Clients need one master wrapping key, not an application-maintained collection
  of manually generated file keys. Each encrypted file selects one durable DEK.
- Concurrent creation, exact replay and ambiguous commit resolution use the
  authoritative winner's metadata; no losing candidate reaches S3 encryption.
- First write/truncate against `content: None` uses the already committed context
  and ignores changed creation defaults.
- State rejects mismatching prepared file-context/policy/key bindings without
  cryptographic computation, and PostgreSQL atomically publishes the content
  reference, metadata updates and terminal result.
- Rewrap changes only the envelope/revision and leaves the DEK identity,
  ciphertext, object names, root reference and data generation unchanged.
- Rename/hard links share the same context; unlink/final close cannot destroy
  keys needed by old roots. Metadata records are retained until future safe GC.
- Raw and paged BlockSplit pass all four representation modes, exact retry,
  tamper, bounded decode, resource and failure-boundary tests.
- No plaintext key crosses persistence/diagnostic boundaries; all normal state
  scans and key loads remain bounded as file count grows.

## Affected Areas

| Area | Impact |
| --- | --- |
| `w9pt-fs-storage` | File-context crypto, LZ4/SIV transforms, protected names, v3 framing, budgets |
| `w9pt-fs-state` | Generic content-metadata record, inode/context binding, reads/scans, commit/rewrap/lifetime rules |
| `w9pt-fs-state-postgres` | Normalized context table, inode columns/FKs, codecs, atomic transitions, conformance |
| Filesystem orchestration | Generate candidate before create; reload committed context before content work; explicit administrative rewrap |
| S3 tests, runner and CI | Required bounded representation and key-lifecycle composition; unchanged provider guards |
| Documentation and fixtures | New key ownership, bootstrap/retry rules, retained-key cost, current-format reset |

## Dependencies

- Implemented paged BlockSplit, state-store, PostgreSQL and S3 foundations.
- Proposed optional packages `lz4_flex` 0.14.0, `aes-siv` 0.8.0, `zeroize` 1.9.0,
  and existing BLAKE3. Verify exact features, MSRV, licenses, advisories and
  cleanup behavior before implementation; no runtime/KMS client enters storage.
- Caller-supplied master material, stable identities and secure entropy.
- Existing mutation-ledger/fencing/transaction contracts, extended for opaque
  context equality and replay of generated allocation results.
- Existing `.dev/` workspace; no bootstrap is needed. Archive dependencies are
  the unarchived storage/paging/state/PostgreSQL deltas, including the latest
  code-first schema revisions.

## Risks

| Risk | Mitigation |
| --- | --- |
| Retry encrypts with a fresh losing DEK | Persist context atomically with inode; reread winner before unwrapping for content |
| Context disappears with inode deletion | Separate retained metadata record, no inode-delete cascade, no eager key GC |
| Master change invalidates S3 references | Derive content keys/tokens only from stable file DEK/context, never wrapping metadata |
| Rewrap changes the underlying DEK | Helper verifies old/new DEK commitment; state freezes binding/policy and CASes only wrapper |
| State acquires algorithm-specific logic | Opaque bounded policy/envelope fields and generic transaction/equality checks |
| Encoded bytes vary across retries | Frozen codec context, deterministic SIV and exact immutable reconciliation |
| New buffers or millions of keys exhaust memory | Exact point reads, paginated scans, operation-scoped keys and measured scratch bounds |
| Bad wrapped/compressed data exposes plaintext | Authenticate bindings, validate commitment, bound decode, redact/clean scratch |
| Large key-table retention is mistaken for a cache leak | Document persistent retention debt and keep RAM independent of total file count |

## Next Step

Review the revised draft and set `Status: approved` before implementation.

## References

- [Research](../../research/zerofs-compression-encryption.md)
- [Analysis](analysis.md)
- [Design](design.md)
- [Blueprint](blueprint.md)
- [Tasks](tasks.md)
- [Storage delta](specs/storage-methods/spec.md)
- [State delta](specs/filesystem-state-store/spec.md)
- [PostgreSQL delta](specs/postgres-state-adapter/spec.md)
- [Live integration delta](specs/seaweedfs-storage-integration/spec.md)
