# Design: Pluggable File Storage Methods

## 1. Boundary and Ownership

The change introduces a file-content repository between a future filesystem semantic engine and a target object store:

```text
w9pt Session
  -> future filesystem semantic engine
       -> w9pt-fs-storage ContentRepository
            -> caller-provided TargetStore
```

`w9pt-fs-storage` owns content layout and persistence. It does not own paths, namespace metadata, inode authorization, QIDs, open handles, append-offset selection, or locks. A caller supplies opaque file and mutation identities and decides when a prepared content reference becomes part of authoritative filesystem metadata.

The existing `w9pt` effect/completion API remains the only protocol boundary. Storage methods are never visible to a 9P client.

## 2. Public Model

The initial conceptual API is:

```rust
pub enum StorageMethod {
    Raw,
    BlockSplit,
}

pub struct ContentRef {
    pub file_id: FileId,
    pub generation: u64,
    pub logical_size: u64,
    pub manifest: ObjectKey,
    pub manifest_hash: Digest,
    pub method: StorageMethod,
}

pub struct PreparedContent {
    pub content: ContentRef,
    pub content_changed: bool,
    pub identity: PreparationIdentity,
}

pub struct ContentRepository<S> {
    target: S,
    limits: StorageLimits,
}
```

The repository exposes create, read, prepare-write, and prepare-truncate operations. Preparation uploads immutable dependencies and returns `PreparedContent`; it never silently changes an authoritative file head. `ContentRef` also has a checked public reconstruction path so an authoritative metadata adapter can persist its fields and recreate the reference after process loss without consulting the optional object head.

Enum dispatch is preferred for the two built-in methods. A public layout-plugin trait is deferred until a third method demonstrates the necessary extension boundary.

## 3. Target Contract

The runtime-neutral asynchronous target contract provides these semantic operations:

```text
get(key, maximum bytes) -> optional exact bytes plus opaque version
get_range(key, half-open byte range) -> optional exact bytes
put_if_absent(key, bytes) -> created or already exists
compare_exchange(key, expected opaque version or absence, bytes)
    -> replaced, conflict, or definitive target failure
```

The Rust interface may use associated futures or generic return-position futures. It must not depend on Tokio or require trait objects in version 1.

Required target guarantees are:

- a successful immutable put is durable before acknowledgment;
- `put_if_absent` atomically creates at most one value for one key;
- compare-and-swap atomically replaces one key only when its expected version matches;
- a successful publication is visible to subsequent reads;
- returned byte ranges are exact or fail explicitly;
- an exact read rejects an object beyond the caller-provided maximum before downloading or allocating the oversized value;
- an ambiguous transport failure can be resolved by reading back the publication key.

Deletion and listing are administrative extensions for later garbage collection, not hot-path requirements.

## 4. Persistent Key Space

All keys live below a caller-configured private prefix:

```text
v1/format
v1/refs/files/<file-id>
v1/manifests/<file-id>/<mutation-id>/<base-generation>/<base-manifest-digest>/<operation-fingerprint>/<attempt>
v1/data/<file-id>/<mutation-id>/<base-generation>/<base-manifest-digest>/<operation-fingerprint>/<attempt>/raw
v1/data/<file-id>/<mutation-id>/<base-generation>/<base-manifest-digest>/<operation-fingerprint>/<attempt>/blocks/<block-index>
```

Only `refs/files/<file-id>` is mutable, and only through compare-and-swap. Every other object is immutable and created with `put_if_absent`.

The target key encoding uses fixed-width lowercase hexadecimal identifiers and checked path construction. Visible filenames never become target keys.

`MutationId` is caller supplied. The repository deterministically fingerprints the complete logical create, positioned-write, or truncate request. Every immutable key binds the file ID, mutation ID, base generation and manifest digest, request fingerprint, and attempt. `attempt` starts at zero and may distinguish repeated preparation within that already collision-safe identity; it is never the only discriminator. A retry on another node derives the same key for the same request and base, while a rebase derives a different key even if its local attempt counter restarts.

Every decoded manifest and blob key must match the repository's exact private prefix and canonical version-1 schema before another target read occurs. A head's recorded mutation identity must match its manifest key. Blob keys must match the file, layout kind, and block index; they may retain an earlier preparation identity when unchanged immutable payload references are reused.

## 5. Checked Object Envelope

Every persisted head, manifest, and data payload uses a checked envelope:

```text
magic
object kind
format major
format minor
payload length
payload checksum
payload
```

The decoder validates the envelope and configured size limit before allocating or decoding nested fields. It rejects unknown object kinds, unsupported major versions, trailing bytes, invalid enum tags, inconsistent lengths, non-canonical ordering, duplicate block indexes, and arithmetic overflow.

Format version 1 implements:

- BLAKE3-256 for canonical plaintext content digests;
- identity payload codec;
- no encryption;
- fixed 32 KiB block size for `BlockSplit`.

The manifest still persists codec and cipher identifiers. Unknown identifiers fail explicitly; they never fall back to identity processing.

## 6. File Head and Manifest

The mutable file head contains:

```text
file ID
generation
manifest key
manifest hash
last mutation ID
```

The canonical manifest key referenced by the head encodes the base content identity, logical-operation fingerprint, and attempt. Head loading parses that key and verifies its mutation component against `last mutation ID` before following the manifest reference.

The immutable manifest contains:

```text
file ID
generation
logical size
storage method
representation identifiers
layout payload
```

The layout payload is one of:

```text
Raw:
  optional BlobRef

BlockSplit:
  block size = 32768
  sorted unique [(block index, BlobRef)]
```

`BlobRef` records target key, canonical plaintext length, stored length, hash algorithm, plaintext digest, codec, and cipher identifiers. A block blob's canonical plaintext length is exactly 32768. An empty raw file has no blob reference.

The configured default method is used only when creating a new file. Reads always dispatch using the manifest. Method migration is not part of version 1.

## 7. Raw Method

### Read

1. Validate and clamp the requested range to logical EOF.
2. Return an empty vector when the offset is at or beyond EOF.
3. For a non-empty file, fetch the complete blob within `max_raw_file_bytes`.
4. Validate its envelope, representation identifiers, exact logical length, and plaintext digest.
5. Return the requested slice.

Fetching the complete raw blob is intentionally simple and provides end-to-end verification. Range-read optimization is deferred because a whole-file digest cannot independently authenticate one arbitrary range.

### Positioned write

1. Check `offset + data.len()` and configured limits.
2. Fetch and verify the complete old file, or begin with an empty vector.
3. Zero-fill a gap when the write starts beyond current EOF.
4. Patch the supplied range and compute the resulting logical size.
5. If bytes and size are unchanged, reuse the previous content reference.
6. Otherwise encode and upload one new immutable raw blob.
7. Encode and upload a new immutable manifest and return its `ContentRef`.

### Truncate

- Shrink by slicing the verified old bytes.
- Extend by appending logical zeroes.
- Represent size zero with no payload blob.
- Reuse the previous reference when the size is unchanged.

## 8. Block-Split Method

### Canonical blocks

Blocks are scoped to a file and indexed by:

```text
block_index = absolute_offset / 32768
within_block = absolute_offset % 32768
```

Every materialized block decodes to exactly 32768 plaintext bytes. Logical size in the manifest is authoritative; final-block padding is never client-visible. Missing entries and omitted all-zero blocks are sparse zeroes.

### Range planning

The planner advances from the current absolute position and remaining length. It never computes `offset + length - 1` before validating an empty range and checked end position. Each span records block index, within-block offset, request-buffer offset, and length.

### Read

1. Clamp the range to EOF and split it into block spans.
2. Synthesize zeroes for absent block entries.
3. Fetch and fully verify every referenced block.
4. Copy only the requested span from each canonical block.
5. Return bytes in logical order.

### Positioned write

1. Split the checked write range into block spans.
2. Use incoming bytes directly for a full-block overwrite.
3. For a partial overwrite, fetch and verify the prior block or start from zeroes.
4. Patch the span into a canonical 32 KiB buffer.
5. Omit an all-zero result.
6. Reuse the old `BlobRef` when the new plaintext digest matches it.
7. Upload every other result under an attempt-specific immutable key.
8. Build a sorted sparse block map, update logical size, upload the manifest, and return the new reference.

A write beyond EOF does not materialize the gap. Missing blocks and untouched bytes in a newly reached block remain zero.

### Truncate

- Extension updates logical size only.
- Shrink removes entries wholly beyond new EOF.
- When new EOF falls inside a retained block, fetch that block, zero its discarded tail, and either upload the changed block or omit it if it becomes all zero.
- Truncate to zero produces an empty block map.

## 9. Preparation and Publication

Preparation guarantees that every data object is acknowledged durable before its manifest is uploaded. It returns a content reference only after the manifest is also durable.

`PreparedContent` carries the mutation ID, base content identity, operation fingerprint, and attempt used for every immutable key. The object publisher accepts this complete preparation and rejects a caller that tries to publish it under another mutation or against another base.

The provisional `ObjectHeadPublisher` provides:

```text
create file head if absent
load published content and opaque target version
publish replacement if expected target version matches
read back an ambiguous result
```

A convenience mutation flow may:

1. load the current head and target version;
2. prepare a logical operation against that content;
3. compare-and-swap the head;
4. on a definitive conflict, load the new content and reapply the operation with a newly derived base-bound identity;
5. stop at `max_publish_retries` with a typed conflict.

Disjoint concurrent writes survive rebasing. Overlapping writes are ordered by successful publication. A future semantic engine may instead serialize per inode and publish the `ContentRef` with inode metadata in one transaction.

After an ambiguous compare-and-swap, an exact matching head proves success. A differing later head does not by itself prove that the ambiguous CAS failed: the desired head may have committed and then been superseded. Without retained lineage or a result ledger, that case returns unresolved ambiguity and is never fed into the conflict-rebase loop.

## 10. Durability and Cancellation

Version 1 is write-through. A successful target put and head CAS are durable by contract. Content-only sync therefore confirms that no preparation or publication error remains; it does not need a background flush.

This is not enough to claim full filesystem metadata durability. The future engine must publish inode and namespace metadata durably before advertising `DurableMetadata`.

Cancellation can stop work before publication when practical. Once the head or a future inode root is published, cancellation cannot roll it back. Every caller-facing operation still returns one terminal result.

## 11. Limits

`StorageLimits` includes at least:

- maximum raw logical bytes;
- maximum manifest bytes;
- maximum stored object bytes;
- maximum blocks represented by one flat manifest;
- maximum read result bytes;
- maximum write input bytes;
- maximum publication retries.

All limits are validated at repository construction and rechecked before allocation, target reads, encoding, or retry. Exact target reads receive the applicable bound. Encoders precompute and validate key, payload, envelope, and collection lengths before allocating their output. Flat-manifest exhaustion is a typed limit failure; version 1 never silently allocates a paged index.

## 12. Errors

The storage crate distinguishes:

- invalid configuration;
- invalid or overflowing logical range;
- missing file, head, manifest, or payload;
- unsupported format, method, codec, cipher, or hash;
- malformed or non-canonical persisted object;
- plaintext digest mismatch/corruption;
- configured resource limit;
- target operation failure;
- publication conflict/retry exhaustion;
- ambiguous publication that cannot be resolved safely.

Mapping these errors to `FilesystemError` and Linux errno values belongs to the future filesystem engine.

## 13. Testing Model

A deterministic in-memory target implements opaque versions, immutable creation, compare-and-swap, exact range reads, operation tracing, and failure injection before/after each state transition.

The test suite covers:

- independent persistent-format golden vectors and malformed encodings;
- every significant block boundary and arithmetic-overflow case;
- raw and block-split byte-vector model equivalence;
- sparse writes, all-zero blocks, EOF padding, shrink, and later extension;
- unchanged-digest upload suppression;
- corruption of heads, manifests, raw objects, and blocks;
- crash/failure after data upload, manifest upload, and head publication;
- deterministic disjoint and overlapping CAS schedules;
- reopen from target state without process-local metadata;
- target contract conformance and configured limits.

Property tests generate operation traces rather than relying only on encode/decode round trips.

## 14. Deferred Evolution

Packed segments can later replace one-object-per-block storage behind a new representation/index layer. Paged block maps can replace flat manifests in a later format. Compression, encryption, keyed content identity, caches, snapshots, and GC each require separate proposals so their failure and compatibility contracts remain reviewable.
