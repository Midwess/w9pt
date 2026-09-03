# Delta for Storage Methods

## ADDED Requirements

### Requirement: Backend-Neutral Content Repository
The system SHALL provide a file-content repository that distributes logical file bytes to a caller-provided target store without exposing storage layout through the 9P protocol/session contract.

#### Scenario: Filesystem engine prepares content
- GIVEN a caller-supplied opaque file identity and persisted content reference
- WHEN the caller requests a positioned read, prepared write, or prepared truncate
- THEN the repository performs the operation according to the persisted storage method
- AND returns logical bytes or a new prepared content reference without modifying any 9P request type

#### Scenario: Namespace semantics are required
- WHEN a caller needs paths, directory enumeration, inode attributes, links, rename, authorization, or open-unlinked behavior
- THEN those semantics remain the responsibility of a filesystem semantic engine
- AND the content repository does not create a second namespace authority

### Requirement: Persisted Storage Method
The system SHALL persist each file version's storage method and method parameters in a self-describing manifest rather than infer them from current process configuration.

#### Scenario: Default method changes
- GIVEN an existing file was created with one storage method
- WHEN the configured default changes before the file is reopened
- THEN the repository reads the existing file using its persisted method
- AND uses the new default only for newly created files

#### Scenario: Unknown method is encountered
- WHEN a manifest names a storage method or parameter unsupported by the implementation
- THEN the repository returns a typed unsupported-format error
- AND does not reinterpret the payload with another method

### Requirement: Target Object Contract
The system SHALL define a runtime-neutral target contract for exact object reads, range reads, atomic immutable creation, and atomic single-key compare-and-swap using opaque target version tokens.

#### Scenario: Immutable object is created
- WHEN the target acknowledges a put-if-absent operation
- THEN the exact object bytes are durable and visible to subsequent reads
- AND another value cannot replace the same immutable key through that operation

#### Scenario: Conditional publication conflicts
- GIVEN a caller loaded one opaque version of a publication key
- WHEN another writer replaces the key before the caller's compare-and-swap
- THEN the target reports a conflict
- AND does not overwrite the newer value

#### Scenario: Target cannot provide required guarantees
- WHEN a writable publisher is constructed over a target without durable puts or atomic compare-and-swap
- THEN configuration fails explicitly
- AND the repository does not claim concurrent atomic publication

### Requirement: Target-Authoritative State
The system SHALL store every head, manifest, and payload needed to reopen published file content in the configured target prefix without requiring an external database or authoritative local cache.

#### Scenario: Process state is lost
- GIVEN published content exists and all process-local state is discarded
- WHEN a new repository instance opens the same target prefix and file identity
- THEN it reconstructs the current content reference and logical bytes from target objects alone

#### Scenario: Normal directory lookup
- WHEN the repository resolves one opaque file's content
- THEN it reads the file head and referenced objects directly
- AND does not depend on target object listing as a filesystem directory operation

### Requirement: Versioned Checked Persistent Format
The system SHALL encode heads, manifests, and payloads in a bounded, versioned format that validates object kind, lengths, checksums, tags, ordering, and arithmetic before state mutation or unbounded allocation.

#### Scenario: Supported version is decoded
- WHEN a persisted object has a supported major version, known tags, canonical ordering, consistent lengths, and valid checksum
- THEN the repository decodes the object deterministically

#### Scenario: Malformed or future object is decoded
- WHEN an object is truncated, oversized, non-canonical, internally inconsistent, checksum-invalid, or uses an unsupported major version
- THEN decoding returns a typed format or corruption error
- AND no prepared or published state is changed

### Requirement: Raw Whole-File Method
The system SHALL implement the `raw` storage method as one optional immutable payload object containing the complete logical file bytes.

#### Scenario: Raw file is read
- GIVEN a non-empty raw file within configured materialization limits
- WHEN the caller reads any valid positioned range
- THEN the repository fetches and verifies the complete payload
- AND returns only bytes within the requested range and logical EOF

#### Scenario: Raw file is partially written
- GIVEN a published raw file
- WHEN the caller writes a positioned byte range
- THEN the repository verifies and reconstructs the complete resulting file
- AND prepares one new immutable payload and manifest without modifying the old payload

#### Scenario: Raw file exceeds its limit
- WHEN a raw mutation or read would materialize more than the configured raw-file bound
- THEN the operation fails with a typed limit error before the oversized allocation or target transfer

### Requirement: Fixed Block-Split Method
The system SHALL implement `block-split` version 1 as sparse, file-relative 32 KiB logical plaintext blocks with a sorted manifest map from block index to immutable payload reference.

#### Scenario: Full block is overwritten
- WHEN a positioned write completely covers one 32 KiB block
- THEN the repository constructs the new block directly from incoming bytes
- AND does not read the superseded block

#### Scenario: Partial block is overwritten
- WHEN a positioned write covers only part of a block
- THEN the repository verifies the existing complete block or uses zeroes for a hole
- AND applies the incoming bytes at the exact within-block position

#### Scenario: Materialized block is decoded
- WHEN the repository loads a referenced block
- THEN its decoded plaintext is exactly 32 KiB
- AND any other decoded length is treated as corruption

### Requirement: Logical EOF and Sparse Zeroes
The system SHALL treat manifest logical size as authoritative EOF and represent absent or all-zero block-split entries as zero-filled sparse content.

#### Scenario: Final block is partial
- GIVEN logical EOF falls inside a materialized block
- WHEN the block is hashed and stored
- THEN bytes after EOF are canonical zero padding through the 32 KiB boundary
- AND reads never return the padding beyond logical EOF

#### Scenario: Write begins beyond EOF
- WHEN a positioned write begins after current logical EOF
- THEN the gap reads as zeroes
- AND block-split storage does not materialize untouched all-zero blocks in that gap

#### Scenario: Resulting block is all zero
- WHEN a write or truncate produces a complete all-zero block
- THEN the new manifest omits that block entry
- AND later reads synthesize the same zero bytes

### Requirement: Safe Truncation
The system SHALL implement shrink and extension without retaining client-visible stale data.

#### Scenario: Block-split file shrinks inside a block
- WHEN a block-split file is truncated to a position inside its final retained block
- THEN entries wholly beyond new EOF are removed
- AND bytes after new EOF in the retained block are zeroed before its new reference is prepared

#### Scenario: Truncated file is extended later
- GIVEN a file previously shrank and discarded content
- WHEN it is extended without writing the discarded range
- THEN the newly visible range contains zeroes
- AND no discarded bytes reappear

#### Scenario: Raw file changes size
- WHEN a raw file is shrunk, extended, or truncated to zero
- THEN the repository prepares the corresponding complete byte sequence within configured limits
- AND size zero has no payload object

### Requirement: Plaintext Integrity and No-Op Detection
The system SHALL use the version-1 BLAKE3-256 digest of canonical plaintext for stored-content verification and unchanged-content detection.

#### Scenario: Prepared block is unchanged
- GIVEN a block-split write results in the same canonical plaintext digest as the prior block
- WHEN the repository prepares the new manifest
- THEN it reuses the prior payload reference
- AND does not upload a replacement payload for that block

#### Scenario: Stored content fails verification
- WHEN loaded raw or block plaintext does not match its recorded digest
- THEN the repository returns a corruption error
- AND does not update the manifest to accept the mismatching bytes

#### Scenario: Hashes match during concurrent work
- WHEN two operations observe equal content hashes
- THEN hash equality may suppress duplicate content preparation
- AND publication ordering still depends on target revisions and compare-and-swap rather than the hash

### Requirement: Separate Layout and Representation
The system SHALL represent file layout independently from payload codec and cipher identifiers.

#### Scenario: Version-1 payload is stored
- WHEN raw or block-split content is prepared by version 1
- THEN its representation is recorded as identity codec with no encryption
- AND its layout remains explicitly raw or block-split

#### Scenario: Unsupported representation is encountered
- WHEN a manifest or blob names an unsupported codec or cipher
- THEN the repository fails explicitly
- AND does not decode the bytes as identity representation

### Requirement: Immutable Preparation and Publication Handoff
The system SHALL prepare immutable payloads and manifests separately from the authoritative publication transaction.

#### Scenario: Content is prepared for a future inode transaction
- WHEN a write or truncate produces changed content
- THEN the repository uploads all immutable dependencies and returns a validated `ContentRef`
- AND does not claim that inode size, timestamps, or namespace metadata were published

#### Scenario: Preparation is cancelled or fails
- WHEN work terminates before publication
- THEN the old published reference remains authoritative
- AND any newly uploaded immutable objects remain unreachable rather than partially visible

### Requirement: Atomic Object-Head Publication
The system SHALL provide an object-backed publisher that conditionally selects one prepared content reference as the current version of an opaque file.

#### Scenario: Publication succeeds
- GIVEN the expected head version still matches
- WHEN all payload objects and the immutable manifest are durable
- THEN the publisher atomically replaces the head with the new generation and manifest reference
- AND subsequent opens observe the complete new content

#### Scenario: Publication conflicts
- WHEN the expected head version no longer matches
- THEN the publisher loads the newer version and either rebases within the configured retry limit or returns a typed conflict
- AND never silently overwrites the concurrent publication

#### Scenario: Publication response is ambiguous
- WHEN a target failure does not reveal whether compare-and-swap committed
- THEN the publisher reads back the head and compares its generation, manifest, and mutation identity
- AND reports success, conflict, or unresolved ambiguity without blindly applying the mutation twice

### Requirement: Data-Before-Metadata Durability
The system SHALL acknowledge a published content mutation only after its immutable payloads, immutable manifest, and publication record are durable in that order of dependency.

#### Scenario: Failure occurs before head publication
- WHEN any payload or manifest operation fails before the head is replaced
- THEN the previous head remains current
- AND no published metadata references a missing payload

#### Scenario: Content-only sync follows a successful mutation
- GIVEN version 1 uses write-through publication and no write-back cache
- WHEN the caller requests a content-only durability barrier
- THEN the repository confirms the published target state without requiring a background flush
- AND does not claim durability for future inode or namespace metadata outside this layer

### Requirement: Bounded Storage Amplification
The system SHALL validate limits for raw materialization, manifests, stored objects, read results, write inputs, block counts, and publication retries before performing the corresponding amplification.

#### Scenario: Flat block map reaches its limit
- WHEN a block-split mutation would exceed the configured manifest or block-count bound
- THEN preparation returns a typed limit error
- AND does not allocate or encode an oversized manifest

#### Scenario: Logical range overflows
- WHEN offset plus length cannot be represented as a logical file range
- THEN the repository returns a typed range error
- AND performs no target mutation

#### Scenario: Writers repeatedly conflict
- WHEN conditional publication conflicts reach the configured retry limit
- THEN the mutation terminates with a retryable conflict error
- AND does not loop or retain unbounded attempts

### Requirement: Deterministic Storage Conformance
The system SHALL provide deterministic tests for both methods, the target contract, persistent formats, logical byte behavior, publication ordering, and injected failures without requiring S3 or a network runtime.

#### Scenario: Layout operation trace is replayed
- WHEN generated create, read, write, and truncate operations run against a storage method and a byte-vector reference model
- THEN every successful publication has identical logical size and bytes
- AND the same trace produces the same target-operation sequence

#### Scenario: Failure is injected during publication
- WHEN a deterministic target failure occurs before or after each payload, manifest, or head step
- THEN reopening from target-only state observes either the complete old or complete new version
- AND never observes a partial mixture

#### Scenario: Later adapter is added
- WHEN an S3, local-file, or other target adapter is implemented
- THEN it can run the reusable target conformance suite
- AND storage-method tests remain independent of that SDK or runtime

