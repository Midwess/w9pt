# Delta for Storage Methods

## ADDED Requirements

### Requirement: Independent Payload Compression

The system SHALL support an optional fixed LZ4 block profile independently
from encryption, applying it to canonical 32 KiB BlockSplit payloads or bounded
whole Raw payloads. It SHALL record actual Identity/LZ4 encoding explicitly and
shall not introduce cross-payload history or external dictionaries.

#### Scenario: Compressible payload is prepared

- WHEN LZ4 policy produces a representation at least 64 bytes smaller than canonical plaintext
- THEN preparation selects LZ4 and records exact canonical and stored lengths
- AND logical block size, offsets and EOF remain unchanged

#### Scenario: Compression saves insufficient space

- WHEN valid compression saves fewer than 64 bytes
- THEN preparation stores actual Identity encoding
- AND it still applies the file's selected encryption policy

#### Scenario: Codec execution fails

- WHEN compression encounters a resource or execution error
- THEN preparation returns an explicit failure
- AND does not confuse the failure with the incompressible-data fallback

#### Scenario: Compressed payload is read

- WHEN a selected LZ4 payload is decoded
- THEN decoding consumes the complete bounded encoded block and produces exactly the expected canonical length
- AND invalid offsets, truncated input, invalid trailing data or excess output cause an error

#### Scenario: Decoder receives an excessive length

- WHEN object metadata or compressed input requests a length beyond the trusted reference or configured limit
- THEN decoding fails before the oversized output allocation
- AND the decoder does not allocate from an embedded size prefix

#### Scenario: Encoder profile is unavailable

- WHEN a write requires the frozen LZ4 profile on a target that cannot reproduce it
- THEN the repository returns an unsupported-encoder-profile error before PUTs
- AND successful decoding alone does not authorize that target to produce a different encoding under the same profile

#### Scenario: Known file policy is read without codec support

- WHEN a build without LZ4 support reads a known Lz4BlockV1-policy file whose accessed payload uses actual Identity encoding
- THEN it can parse the known policy and read that payload subject to its other key/integrity checks
- AND an actual LZ4 payload or preparation requiring LZ4 encoding reports the unavailable implementation instead of guessing a codec

### Requirement: Authenticated Client-Side Content Encryption

The system SHALL support deterministic AES-256-SIV protection of immutable
payload, leaf/branch page, and compact manifest bodies using a standard RFC
5297 implementation. It SHALL authenticate object context and the complete
bounded body before parsing sensitive provenance, decompressing, or returning
plaintext.

#### Scenario: Encrypted file is prepared

- WHEN encryption is enabled with an available key
- THEN all newly created immutable payload/page/root bodies for that file are encrypted and authenticated before target upload
- AND compression, if selected, occurs before encryption

#### Scenario: Object context is altered

- WHEN ciphertext is substituted across file, block, page range, kind, prefix, key domain, or authenticated header context
- THEN read validation fails
- AND the object is not treated as valid merely because its bytes form a decodable payload

#### Scenario: Working cipher key is derived

- WHEN an immutable object is encoded or decoded
- THEN its SIV working key is derived from the selected file DEK and complete exact object key with the reviewed domain/key/purpose separation
- AND same-object retries reproduce that key while different object keys do not share one global SIV working key

#### Scenario: Authentication fails

- WHEN the selected key or ciphertext/tag is incorrect
- THEN the repository returns a redacted authentication error
- AND performs no decompression or plaintext return from that object

#### Scenario: Cipher is unsupported or unavailable

- WHEN an object or selected policy requires an unknown or compiled-out cipher
- THEN the repository returns an explicit unsupported error
- AND does not fall back to None encryption

#### Scenario: Empty encrypted file is opened

- WHEN a file has no data payload but its manifest is encrypted
- THEN opening it still requires successful root authentication with the selected key
- AND absence of data blocks does not bypass encryption policy

### Requirement: Managed Per-File Envelope Keys

The system SHALL generate a random data key for each encrypted file using
caller-supplied secure entropy, wrap it with one externally supplied master KEK,
and return bounded opaque candidate metadata. Only the authoritative committed
file context SHALL be unwrapped for content operations.

#### Scenario: Encrypted file context is generated

- WHEN candidate generation succeeds
- THEN the helper returns pinned policy, stable file/context binding, key commitment and a wrapped DEK
- AND it releases the candidate plaintext key without returning a storage-ready secret handle

#### Scenario: Entropy or wrapping fails

- WHEN the supplied entropy source or wrapping operation fails
- THEN candidate creation fails explicitly before state commit or S3 content upload
- AND no fixed or plaintext fallback key is used

#### Scenario: Durable winner is resolved

- WHEN file creation commits, replays or resolves an ambiguous commit
- THEN orchestration reloads the committed inode/context record and unwraps that exact winner
- AND it does not use a retry's independently generated losing candidate

#### Scenario: First content operation follows creation

- WHEN write or truncate begins against an inode whose content is still unpublished
- THEN it uses the already committed policy and file key
- AND does not generate another DEK or choose changed creation defaults

#### Scenario: Plain file is created

- WHEN encryption is None
- THEN its generic context pins storage policy without a DEK or wrapped-key envelope
- AND secure entropy or a master key is not required for plain content

#### Scenario: Context or master is unavailable

- WHEN a required context is missing, belongs to another file, or cannot be authenticated with the supplied master
- THEN content access returns an explicit context/key/authentication error
- AND no other master, plaintext policy, or sparse-hole interpretation is substituted

### Requirement: Wrapping and Content Key Separation

The master KEK SHALL protect only per-file key envelopes. S3 naming and
per-object encryption keys SHALL derive from the selected file DEK and stable
context, excluding master identity, wrapping bytes and state revision.

#### Scenario: File key is unwrapped

- WHEN the storage helper opens a committed key envelope
- THEN it authenticates filesystem/file/owner/context/policy binding and verifies the immutable DEK commitment
- AND raw keys are retained only in a bounded redacted operation context

#### Scenario: Master key is changed through explicit rewrap

- WHEN the helper unwraps with the old KEK and rewraps the same DEK with a new KEK
- THEN the candidate preserves stable context, policy and key commitment
- AND S3 keys, ciphertext, ContentRefs and data generations remain unchanged

#### Scenario: Master configuration changes without rewrap

- WHEN records still require the old master but only a new master is supplied
- THEN those records fail to unwrap explicitly
- AND changing configuration is not presented as automatic rotation or recovery

#### Scenario: Many files exist

- WHEN a small set of files is accessed
- THEN only their contexts are loaded/unwrapped under bounded operation budgets
- AND no all-files plaintext keyring is populated

### Requirement: Protected Preparation Identity

The system SHALL preserve public semantic preparation identities while using
a domain-separated keyed token in encrypted-mode target names. Full semantic
provenance SHALL be stored inside authenticated encrypted bodies and validated
against the exact token/location after decryption.

#### Scenario: Storage observer inspects protected names

- WHEN an observer lists encrypted-mode object keys or reads their public headers/provider metadata
- THEN these fields contain no raw unkeyed operation fingerprint or canonical plaintext digest
- AND naming does not provide the v2 offline plaintext-fingerprint test

#### Scenario: Root provenance is needed

- WHEN the repository or standalone publisher validates a protected root
- THEN it obtains full provenance from the checked body and recomputes the token
- AND preserves file, mutation, base, fingerprint, attempt and result-generation checks

#### Scenario: Encoding policy differs

- WHEN the same semantic operation is prepared with a different file DEK/context or immutable codec/cipher policy
- THEN the storage preparation token differs
- AND representations cannot silently compete for the same immutable key

#### Scenario: Only wrapping metadata changes

- WHEN the same file DEK is rewrapped under a different master or record revision
- THEN its content preparation token and S3 encryption context remain unchanged
- AND master wrapping metadata never enters those content identities

#### Scenario: Object provenance disagrees with its key

- WHEN authenticated body provenance does not reproduce the selected token/location
- THEN validation returns an identity/corruption error before using dependent data

### Requirement: Reproducible Encoding Context

The system SHALL use one deterministic representation pipeline and frozen
encoding context for preflight, actual upload and exact retry. It SHALL not
invoke fresh randomness independently during object encoding.

#### Scenario: Preflight and upload encode the same object

- WHEN they use the same canonical bytes, provenance, policy and key
- THEN complete stored bytes, sizes and resulting references are identical

#### Scenario: Independent clients retry the same preparation

- WHEN clients reconstruct the same complete encoding context
- THEN they produce identical desired object bytes for exact immutable reconciliation
- AND the absence of shared RAM does not change the representation

#### Scenario: Encoder dependency changes output

- WHEN a dependency or build target cannot reproduce the frozen encoded vectors
- THEN it cannot write under that same encoder profile
- AND implementation must use an explicitly different reviewed profile/current format rather than silently weakening equality checks

### Requirement: Explicit Confidentiality and Secret Handling

The system SHALL document what client-side encryption protects and SHALL use
redacted secret types and cleanup for storage-owned sensitive material. It
SHALL not claim that encrypted object bodies imply complete metadata secrecy,
process-memory erasure, or authenticated standalone root selection.

#### Scenario: Secrets are formatted or released

- WHEN master/file-key/derived context or an error is formatted or dropped
- THEN diagnostics omit secret material and owned secret buffers are cleaned through the reviewed zeroization path
- AND the contract does not claim to erase caller buffers, compiler temporaries, registers or host swap

#### Scenario: Observer inspects remaining metadata

- WHEN encryption protects a file's immutable objects
- THEN documented names, sizes, counts, access patterns, sparse allocation, PostgreSQL metadata and standalone head fields can remain visible
- AND compression-length leakage remains an explicit limitation

#### Scenario: Standalone head is used

- WHEN a standalone publisher resolves a root from its clear checked head
- THEN that head remains a trusted publication input under the private-prefix contract
- AND its checksum is not described as a cryptographic defense against hostile root replacement or rollback

#### Scenario: State boundary is inspected

- WHEN this feature is implemented
- THEN state and PostgreSQL persist bounded opaque file policy and wrapped keys with generic identity/revision/lifetime validation
- AND neither layer executes cryptography or persists plaintext masters/DEKs; storage retains content-method interpretation

## MODIFIED Requirements

### Requirement: Persisted Storage Method

The system SHALL pin method/profile and representation policy in generic file
state at creation and record matching policy in immutable storage metadata.
Existing-file content operations SHALL use the committed file context.

#### Scenario: Creation defaults change

- WHEN an existing file is first written, reopened or modified by a differently configured process
- THEN the committed file policy and DEK identity remain in force
- AND changed defaults apply only to new file creation

#### Scenario: Actual payload codec differs within a file

- WHEN LZ4 policy stored a payload as Identity due to the savings rule
- THEN it is decoded according to its actual checked descriptor
- AND no codec is guessed from process defaults

#### Scenario: Unsupported method or profile appears

- WHEN storage interprets an unsupported policy or required profile
- THEN it returns an explicit unsupported error without reinterpreting bytes
- AND state/SQL code continues treating the policy as bounded opaque metadata

### Requirement: Separate Layout and Representation

The system SHALL keep Raw/BlockSplit layout independent from actual payload
codec and object protection. Identity/None, LZ4/None, Identity/SIV and LZ4/SIV
SHALL be supported when their implementations and required keys are available.

#### Scenario: Representation is selected

- WHEN compression or encryption is enabled
- THEN logical offsets, canonical block size, sparse omission and EOF semantics are unchanged
- AND selecting a transform does not add a new storage method

#### Scenario: Metadata pages are protected

- WHEN an encrypted paged file is prepared
- THEN immutable pages and roots are independently encrypted with the file policy
- AND this first profile does not compress metadata pages or roots

#### Scenario: No transformation was requested

- WHEN the caller uses existing default constructors
- THEN the current format uses Identity/None without requiring keys
- AND plain managed files still retain a generic pinned context without requiring cryptographic execution in state

### Requirement: Versioned Checked Persistent Format

The system SHALL use one current major-version-3 format for heads, roots,
mapping pages and payloads, separating canonical plaintext, actual codec bytes,
protected body and complete stored object lengths. It SHALL reject earlier
development formats without compatibility readers or automatic conversion.

#### Scenario: Protected object header is decoded

- WHEN bounded routing fields identify a supported cipher and allowed key
- THEN the header/body context is authenticated before sensitive body parsing
- AND fields outside protection expose no plaintext checksum

#### Scenario: Lengths disagree

- WHEN a declared body, actual codec, canonical plaintext or complete stored length violates its reference or bound
- THEN decoding fails explicitly before corresponding amplification or data return

#### Scenario: Stored digests are reconstructed

- WHEN page/root references are persisted or reopened
- THEN their digests identify the complete stored object bytes including protection
- AND BlobRef plaintext integrity retains its distinct canonical-digest meaning

#### Scenario: Earlier development objects are encountered

- WHEN a key or envelope belongs to v1 or v2
- THEN the current reader rejects it
- AND operators recreate development state rather than relying on a legacy reader or automatic migration

### Requirement: Target-Authoritative State

The system SHALL store immutable content dependencies and any standalone
publication heads in the target prefix. Encrypted content additionally requires
the retained committed file-context metadata and caller-supplied master KEK.
Clustered root selection SHALL remain authoritative in the state database.

#### Scenario: Local state is discarded

- WHEN a new client receives the authoritative ContentRef, committed file metadata and correct master
- THEN it can reconstruct the file DEK and selected target content without a local key cache

#### Scenario: Target objects remain but context is lost

- WHEN ciphertext exists without its required wrapped-key metadata
- THEN reopening fails explicitly even if the master KEK is known
- AND an S3 head alone is not described as DEK recovery

#### Scenario: Clustered inode selects content

- WHEN state publishes a ContentRef with the matching context binding
- THEN it selects content without target listing or a second mutable S3 authority

### Requirement: Authenticated Mapping Pages

The system SHALL bind every accessed page to its selected stored length/digest,
checked protected provenance, file, range, level and local child summaries.
Encrypted pages SHALL authenticate before their inner records or references
are used. Traversal SHALL remain bounded and lazy.

#### Scenario: Encrypted page is visited

- WHEN a selected path reaches a page
- THEN its protection, provenance and canonical page structure are verified
- AND unrelated descendants are not fetched for a whole-tree audit

#### Scenario: Reused page originated in an earlier generation

- WHEN its exact authenticated reference is retained by a newer root
- THEN verification uses its own creation context and the file's pinned policy
- AND it is not invalidated solely because the current parent generation differs

#### Scenario: Page redirects or expands traversal

- WHEN slots, levels, ranges, identity, keys, summaries or sizes violate the tree contract
- THEN validation returns a typed error within work/allocation bounds
- AND the invalid page cannot introduce cycles, foreign references, or unbounded descent

#### Scenario: Reference is missing

- WHEN an explicit page or payload reference cannot be fetched
- THEN reading fails
- AND only an absent slot in verified mapping metadata represents a hole

### Requirement: Explicit Root and Accessed-Object Validation

The system SHALL distinguish bounded checked root opening from validation of
descendants reached during an operation. Protected roots SHALL authenticate
without requiring a recursive scan for ordinary open, validation, sync or
standalone publication.

#### Scenario: Protected root is opened or synced

- WHEN its exact stored reference and required key are available
- THEN root authentication and immediate metadata validation run within root bounds
- AND no implicit payload scrub or whole-tree scan occurs

#### Scenario: Corruption lies outside the accessed range

- WHEN a descendant outside the request is corrupt
- THEN a lazy operation is not required to discover it through an unrelated scan
- AND later access authenticates/validates that descendant and reports failure

#### Scenario: Root policy cannot be authenticated

- WHEN a root has a missing key, invalid tag or inconsistent protected policy/provenance
- THEN no successful root validation or plaintext read is reported

#### Scenario: Retained page describes content beyond EOF

- WHEN a checked root or accessed page summary places materialized content beyond the selected logical EOF
- THEN validation fails despite successful decryption/authentication
- AND no out-of-range mapping is exposed or silently accepted

#### Scenario: Child policy disagrees with the selected root

- WHEN an accessed object's protection or body policy differs from the committed file context and matching authenticated root
- THEN the repository rejects the mismatch
- AND it does not accept a child solely because its self-declared policy can be decoded

### Requirement: Plaintext Integrity and No-Op Detection

The system SHALL use canonical plaintext BLAKE3 for integrity, zero omission
and unchanged-content detection, with those digests protected inside encrypted
metadata when encryption is selected. Ciphertext identity SHALL not replace
logical content identity or authoritative publication ordering.

#### Scenario: Logical operation is unchanged

- WHEN bytes and EOF match the selected base
- THEN the original content reference is returned without payload/page/root PUTs
- AND different creation defaults do not force a policy change or rewrite

#### Scenario: Materialized block becomes zero

- WHEN a BlockSplit write or truncate produces a canonical all-zero block
- THEN its mapping entry is omitted before compression/encryption
- AND later reads synthesize the expected zero bytes

#### Scenario: Decrypted and decoded content mismatches

- WHEN canonical length, digest or final padding differs from the selected reference
- THEN the repository reports corruption
- AND it does not update metadata to accept the mismatching content

#### Scenario: Unchanged pages or payloads are reused

- WHEN they remain valid under the file's pinned policy
- THEN their exact references are reused without re-encryption
- AND unrelated content is not visited merely to apply current defaults

### Requirement: Immutable Preparation and Publication Handoff

The system SHALL prepare complete transformed immutable objects separately
from authoritative publication using the deterministic context reconstructed from the committed file metadata.
Preflight SHALL predict exact transformed references and known resource limits;
all new child dependencies SHALL be confirmed durable before parents and root.

#### Scenario: Transformed size exceeds a known limit

- WHEN preflight computes an oversized object/root/page or planned workspace requirement
- THEN it returns a typed error before immutable PUTs
- AND compression cannot bypass canonical plaintext limits

#### Scenario: Preparation crosses page boundaries

- WHEN a request changes multiple leaves/ancestors
- THEN transformed payloads and pages are prepared incrementally with a bounded frontier
- AND each changed page location is finalized once under its complete context

#### Scenario: Immutable creation is repeated or ambiguous

- WHEN AlreadyExists or ambiguous creation is encountered
- THEN exact stored bytes must match the deterministic desired object before dependent work can succeed
- AND semantic decoding equivalence alone does not excuse an immutable byte mismatch

#### Scenario: Encoding or preparation fails after earlier uploads

- WHEN a key, authentication, codec, target or resource failure stops the request
- THEN it returns no successful incomplete PreparedContent
- AND the old published root remains authoritative while abandoned objects may remain unreachable

#### Scenario: Protected content is published

- WHEN all new dependencies and the root are confirmed durable
- THEN the existing metadata transaction publishes ContentRef with size/generations/attributes and retained result
- AND publication verifies the matching nonsecret context/policy/key binding and context revision without performing crypto in state or SQL

### Requirement: Bounded Storage Amplification

The system SHALL bound representation scratch/workspace and per-operation keys in addition
to canonical requests, Raw materialization, roots, pages, map/frontier memory,
counts, page work and retries. Bounds SHALL include encryption/compression
overhead and both preparation passes before corresponding amplification.

#### Scenario: Small range belongs to a large file

- WHEN a protected/compressed paged file is read at a small range
- THEN only the relevant mapping pages and payloads are decoded
- AND transform memory does not grow with the entire file or map

#### Scenario: Decoder controls are applied

- WHEN a stored payload is decompressed
- THEN output capacity and codec workspace are bounded independently of stored compressed length
- AND invalid data cannot reveal prior contents of reused output buffers as successful plaintext

#### Scenario: Transform buffers overlap in lifetime

- WHEN canonical, compressed, protected, decoded or readback buffers coexist
- THEN their actual capacities/workspace are charged or conservatively reserved by the appropriate operation budgets
- AND the encoded object cap is not described as a total RAM or process RSS cap

#### Scenario: Range or size calculation overflows

- WHEN any request, codec bound, protected overhead, key length or allocation calculation is not representable
- THEN the operation returns a typed error before the invalid allocation or target mutation

#### Scenario: Recovery work repeats

- WHEN preflight, preparation, publication conflict handling or immutable readback repeats work
- THEN existing bounded work/retry policies apply to the complete transformed operation
- AND attempts do not accumulate an unbounded payload/page/secret cache

### Requirement: Deterministic Storage Conformance

The system SHALL verify the representation matrix, protected framing,
primitive integration, resource bounds and failure semantics through
deterministic storage-owned tests. Generic state and adapter tests SHALL cover
opaque key-metadata selection, publication, rewrap and retention.

#### Scenario: Policy matrix executes

- WHEN Raw and BlockSplit run with each supported compression/encryption combination
- THEN byte-vector or sparse models agree on bytes, offsets, EOF, no-op behavior, tail zeroing and page transitions

#### Scenario: Known-answer and encoding vectors execute

- WHEN primitive, protected-object and encoder-profile vectors run
- THEN exact keys, tokens, AAD, stored bytes and references match independently specified expected values
- AND successful round trips alone are not accepted as the complete evidence

#### Scenario: Corruption and key failures are injected

- WHEN stored data, authenticated context, keys or compressed input are invalid
- THEN explicit errors prevent plaintext exposure and incomplete publication
- AND secret diagnostics remain redacted

#### Scenario: Failure interrupts publication

- WHEN failure occurs before or after transformed payload, page, root or publication acknowledgement
- THEN independently reopened readers observe a complete selected old or new version
- AND exact retries preserve the existing collision and ambiguity contracts

#### Scenario: Resource evidence executes

- WHEN fixed-range operations run against large synthetic maps and adversarial encoded input
- THEN measured operation accounting remains within representation/map limits
- AND fixture backing memory or provider buffers are not mislabeled as repository working memory
