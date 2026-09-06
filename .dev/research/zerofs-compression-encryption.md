# ZeroFS Compression and Encryption Research

Researched: 2026-09-06, before drafting `add-content-compression-encryption`.

Proposal direction was subsequently revised at the user's request: one external
master KEK now wraps generated per-file DEKs stored as generic state metadata
in PostgreSQL. The ZeroFS observations below remain research evidence; the
updated adoption decisions are reflected in the existing proposal.

## Evidence Scope

This review uses ZeroFS's current public documentation and repository overview.
It did not run ZeroFS or independently verify its implementation. Repository
`main` was observed at `4b260e6d66b2ddc9754919eacb01f1c1fd72238b`; the published
documentation is not asserted to be built from that exact revision.

Existing `.dev/research/9p-s3-filesystem.md` contains older source-based
observations. Current public documentation is preferred for the statements
below, particularly key storage and codec defaults. No implementation code,
tests, cryptographic labels, byte layout, or distinctive internal decomposition
is imported into the w9pt design. ZeroFS offers AGPL and commercial licensing;
the repository's independent-implementation rule remains applicable.
[ZeroFS licensing](https://www.zerofs.net/docs/licensing)

## Documented Behavior

### Compression

ZeroFS processes file data as independent 32 KiB extents. It supports Zstd and
LZ4, with Zstd level 3 documented as the default. Compression happens before
encryption. Codec configuration changes affect subsequent writes; earlier
encodings remain readable because each frame describes its representation.
Its documentation says compaction moves existing encoded frames without
recompressing them. This supports selective reads without a file-wide
decompression stream.
[Configuration: compression](https://www.zerofs.net/docs/configuration#compression)

### Encryption and Keys

ZeroFS documents XChaCha20-Poly1305 with fresh random nonces for data frames.
Associated data binds frames to their file/location and storage context.
Encryption is mandatory in its deployment model. A randomly generated data
key is protected by a password-derived Argon2id wrapping key; password changes
rewrap the data key rather than rewriting file contents. Current documentation
places the wrapped key in a separate object, and describes separate derived
keys for data and metadata. Object names, sizes, and some metadata remain
visible: its encryption is not a claim to hide every property of stored data.
[Encryption and security](https://www.zerofs.net/docs/encryption)

### Storage and Durability

Encoded extents are packed into immutable segment objects while an object-
backed LSM stores filesystem metadata and references. Reads locate a frame and
fetch its byte range. Writes may remain buffered before a durability barrier;
the flush path uploads data before making metadata durable. Thus segment
buffering and its flush protocol are part of ZeroFS's correctness model, not
just a compression setting.
[Architecture](https://www.zerofs.net/docs/architecture),
[Storage engine](https://www.zerofs.net/docs/storage-engine)

### Cache Confidentiality

The raw object cache retains stored encoded bytes, while the decoded metadata
cache has a different confidentiality boundary. Storage encryption alone does
not establish that every local cache is encrypted. Any future w9pt cache must
state which representation it retains and separately bound its memory.
[Caching](https://www.zerofs.net/docs/caching)

## Implications for w9pt

These are independent design conclusions derived from w9pt's existing code:

| Concern | w9pt decision |
| --- | --- |
| Selective I/O | Transform individual canonical blocks; keep the existing paged lookup and logical offsets |
| Compression order | Compress before encryption; verify and authenticate before decompression on reads |
| Codec metadata | Persist actual per-payload encoding; never infer it from current defaults |
| Authority | Retain PostgreSQL metadata publication and existing ContentRef fields |
| Retry model | Keep exact encoded-byte equality for preflight, upload, AlreadyExists, and ambiguous readback |
| Key ownership | The application supplies one master KEK and entropy; storage generates per-file DEKs and state persists their wrapped envelopes |
| Metadata confidentiality | Encrypt immutable pages and root bodies as well as payloads; document plaintext PostgreSQL metadata and standalone heads |
| Memory | Bound codec output/workspace, decrypted buffers, encryption copies, and map working memory |

The most consequential difference is deterministic preparation.
`expected_map_page` hashes the complete encoded object, and both preparation
passes must produce the same root. Generating fresh random ciphertext each
pass would violate that contract. Using randomized encryption would require
stable, recoverable attempt randomness or a different preparation/readback
protocol. That is additional state and scope.

For the first w9pt proposal, deterministic AES-SIV fits the existing contract.
RFC 5297 explicitly defines deterministic authenticated encryption; this is
not ordinary AES-GCM with a reused nonce. The draft uses a standard library
implementation and independent vectors, not a cipher implementation derived
from ZeroFS. Exact retry equality also requires a frozen compression encoder
profile, not merely a decodable compression format.
[RFC 5297, deterministic mode](https://www.rfc-editor.org/rfc/rfc5297.html#section-4)

Current w9pt object keys embed an unkeyed fingerprint of operation input. Body
encryption alone would preserve an offline guessing signal for predictable
writes. The draft must replace that public component with a keyed preparation
token while carrying full semantic provenance inside the protected body.

## Codec and Dependency Findings

LZ4 is selected for the initial draft because the bounded block API fits the
existing memory-focused design. Zstd remains a documented alternative for a
future profile, especially when compression ratio matters more than codec
workspace. No compressed form is guaranteed to save space on media data.

Candidate package metadata checked through the official Cargo registry:

| Package | Version | Relevant choices |
| --- | --- | --- |
| `lz4_flex` | 0.14.0 | Disable defaults; enable safe/checked block APIs; no frame or external dictionary support |
| `aes-siv` | 0.8.0 | Disable default RNG feature; enable allocation and zeroization support; use deterministic SIV interface |
| `zeroize` | 1.9.0 | Owned key/scratch cleanup with separately redacted diagnostics |
| `blake3` | 1.8.7 already resolved | Existing integrity plus domain-separated key derivation/keyed naming |

Registry metadata declares MIT for `lz4_flex`, and MIT/Apache-2.0 alternatives
for the crypto utilities. The proposed versions fit Rust 1.94.1. This metadata
check is not a completed dependency audit or security certification; exact
features, transitive packages, MSRV, advisories, and key-schedule cleanup must
be checked during implementation.
[LZ4 package](https://crates.io/crates/lz4_flex/0.14.0),
[AES-SIV package](https://crates.io/crates/aes-siv/0.8.0),
[zeroize package](https://crates.io/crates/zeroize/1.9.0)

The inspected LZ4 compressor has architecture-dependent paths for larger
inputs. A library version pin alone is insufficient evidence of identical
encoded output on every target. Freeze and test an encoder profile, initially
little-endian 64-bit, and reject encoding where that profile cannot be
reproduced; decoding is a separate capability.

RustSec advisory RUSTSEC-2026-0041 covers older LZ4 block decoders that could
expose old/uninitialized buffer contents. The proposed version is beyond its
patched ranges, but malformed-offset and reused-buffer regression tests remain
necessary. Neither a safe-decode feature nor an AEAD tag is a substitute for
bounded, validated decompression.
[RustSec advisory](https://rustsec.org/advisories/RUSTSEC-2026-0041.html)

## What Is Not Adopted

The proposal does not adopt ZeroFS's segment format, metadata LSM, random-nonce
frame construction, password file, caches, compatibility rules, or flush
protocol. The revised design extends state with bounded opaque file-context
metadata while keeping cryptographic execution in storage/orchestration.
Password processing remains an embedding-application concern or separate work.
