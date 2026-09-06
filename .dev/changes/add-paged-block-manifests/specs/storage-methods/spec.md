# Delta for Storage Methods

## ADDED Requirements

### Requirement: Paged Immutable Block Map

The system SHALL represent BlockSplit mappings with a compact immutable file
manifest referencing at most one sparse immutable radix-tree root. The current
profile SHALL use 128 slots per leaf/branch and at most seven page levels, with
32 KiB logical data blocks. PostgreSQL SHALL continue to publish the existing
bounded `ContentRef`; mapping entries SHALL remain in target storage.

The feature SHALL leave `w9pt-fs-state` sources, public APIs, and reusable tests
unchanged. The PostgreSQL adapter SHALL persist the existing content reference,
and the storage repository SHALL remain responsible for method interpretation.

#### Scenario: Paging crosses the state boundary

- WHEN paged content is published through the existing state API
- THEN generic file/base/size/generation/mutation/fencing validation applies without page geometry or storage-method dispatch in the state crate
- AND page-specific round-trip coverage resides in PostgreSQL adapter integration tests

#### Scenario: Large file has many materialized blocks

- WHEN the repository prepares or opens a BlockSplit file with many mapping pages
- THEN the file manifest contains one optional bounded map-root reference
- AND neither that manifest nor the PostgreSQL inode contains a list of all blocks or all leaf pages

#### Scenario: Tree geometry is persisted

- WHEN a root or mapping page is decoded
- THEN the implementation validates its persisted block size, radix profile, level, and aligned range
- AND current process defaults cannot reinterpret its routing

#### Scenario: Sparse file has no materialized blocks

- WHEN a file contains only holes at any supported logical size
- THEN its manifest has no map root
- AND no empty page or zero payload object is required

#### Scenario: Root grows or collapses

- WHEN a mutation changes the highest materialized block
- THEN the result uses the smallest root level covering that index and has checked count/highest-index summaries
- AND the operation does not enumerate unaffected subtrees to determine the new root

### Requirement: Range-Local Mapping Traversal

The system SHALL locate requested BlockSplit ranges through exact page
references while retaining only a bounded active path/frontier. Normal I/O
SHALL NOT flatten the tree, build a per-file page directory, or use target
listing to resolve block versions.

#### Scenario: Cold read touches one block

- WHEN a read within EOF touches one logical block from empty local state
- THEN it loads the compact manifest and at most seven mapping pages
- AND it fetches at most the selected materialized payload and no unrelated subtree

#### Scenario: Range crosses neighboring blocks

- WHEN a read covers multiple blocks in the same leaf or neighboring leaves
- THEN the traversal reuses its active leaf/ancestors while they remain relevant
- AND verified payload buffers and completed page ranges are released as traversal advances

#### Scenario: Read is empty or beyond EOF

- WHEN the checked requested range is empty after EOF clamping
- THEN the repository returns an empty output without mapping-page or payload reads
- AND it may still perform the bounded root validation required by its API

#### Scenario: Missing slot and missing object differ

- WHEN a verified mapping page omits the requested slot
- THEN the repository synthesizes zero bytes
- AND an explicit reference whose object is missing returns a missing-content error rather than being interpreted as a hole

### Requirement: Authenticated Mapping Pages

The system SHALL bind every fetched mapping page to its expected encoded
length, digest, file identity, radix level/range, and locally validated child
summaries. It SHALL validate bounded sorted unique slots and private keys
before following references.

#### Scenario: Page content disagrees with its reference

- WHEN a page has mismatched bytes, length, file, range, level, kind, or summary
- THEN traversal returns a typed format or corruption error
- AND it does not follow references from that invalid page

#### Scenario: Page attempts unbounded or redirected traversal

- WHEN a page contains excessive entries, foreign keys, duplicate slots, invalid child ranges, or a nondecreasing child level
- THEN decoding or contextual validation fails within configured allocation/work limits
- AND cycles or depth escalation cannot cause an unbounded traversal

#### Scenario: Existing page is reused by a later generation

- WHEN a new file version references an unchanged earlier page
- THEN the page is accepted when its exact reference, file/range/profile, and creation provenance validate
- AND it is not rejected merely because its preparation predates the current root generation

### Requirement: Explicit Root and Accessed-Object Validation

The system SHALL distinguish bounded root validation from validation of pages
and payloads reached during an operation. Root loading, `validate_content`,
`sync_content`, and standalone publication checks SHALL NOT implicitly scan
the entire mapping tree or claim to certify unvisited payload integrity.

#### Scenario: Root is reopened or synced

- WHEN a valid content reference is reopened or passes the content-only sync barrier
- THEN the repository validates the compact root and its immediate reference description within root bounds
- AND it does not fetch every descendant page or payload

#### Scenario: Corruption is outside the requested range

- WHEN an unread subtree contains corrupt descendant bytes
- THEN a range operation is not required to discover that corruption through an unrelated scan
- AND accessing that subtree later verifies it and reports the failure

#### Scenario: Retained page describes content beyond EOF

- WHEN a loaded root or page summary places materialized content beyond the selected logical EOF
- THEN validation fails rather than exposing or silently accepting the out-of-range mapping

## MODIFIED Requirements

### Requirement: Persisted Storage Method

The system SHALL persist each file version's storage method, representation,
and method parameters in a self-describing compact manifest rather than infer
them from current configuration.

#### Scenario: Default method changes

- GIVEN an existing file was created with one storage method
- WHEN the configured creation default changes before the file is reopened
- THEN the repository reads the existing file using its persisted method/profile
- AND uses the new default only for newly created files

#### Scenario: Unknown method or profile is encountered

- WHEN a root names an unsupported method, block size, or radix profile
- THEN the repository returns a typed unsupported-format error
- AND does not reinterpret the stored content with another method or geometry

### Requirement: Versioned Checked Persistent Format

The system SHALL encode heads, compact manifests, leaf pages, branch pages,
and payloads in the current bounded major-version-2 format, validating kinds,
lengths, checksums, tags, ordering, and arithmetic before dependent work or
unbounded allocation. It SHALL reject earlier development formats without a
legacy reader, converter, or dual writer.

#### Scenario: Current object is decoded

- WHEN an object has the supported version, kind, canonical fields, consistent lengths, and valid checksum
- THEN it decodes deterministically within its type-specific bound

#### Scenario: Malformed object is decoded

- WHEN an object is truncated, oversized, noncanonical, checksum-invalid, or internally inconsistent
- THEN decoding returns a typed error before following dependent references
- AND no successful preparation or publication is produced from the invalid object

#### Scenario: Earlier development data is encountered

- WHEN a reference/key/envelope belongs to the superseded v1 storage representation
- THEN the new repository rejects it explicitly
- AND adoption requires fresh development state rather than an automatic migration or fallback reader

### Requirement: Fixed Block-Split Method

The system SHALL implement BlockSplit as sparse file-relative 32 KiB canonical
plaintext blocks selected through the current paged immutable mapping. A
materialized block SHALL decode to exactly 32 KiB.

#### Scenario: Full block is overwritten

- WHEN a positioned write completely covers one logical block
- THEN the repository constructs its replacement directly from the input bytes
- AND does not download the superseded payload, although mapping pages may be read

#### Scenario: Partial block is overwritten

- WHEN a positioned write covers only part of a block
- THEN the repository verifies the existing complete block or begins with zeros for a hole
- AND applies the input at the checked within-block position

#### Scenario: Materialized block has an invalid decoded length

- WHEN a referenced block decodes to any size other than 32 KiB
- THEN the repository reports corruption
- AND does not return those bytes as valid content

#### Scenario: Distant write exposes an old partial tail

- WHEN a positioned write extends EOF but retains an old partial final block outside its input range
- THEN preparation verifies that old block's zero padding before exposing the intervening bytes
- AND a full overwrite of the old final block still needs no superseded-payload read

### Requirement: Safe Truncation

The system SHALL implement shrink and extension without resurrecting discarded
content. BlockSplit shrink SHALL detach wholly discarded mapping subtrees
without fetching their descendants, rebuilding only the retained boundary and
necessary ancestors.

#### Scenario: File shrinks inside a materialized block

- WHEN new EOF falls inside the final retained materialized block
- THEN the repository verifies that block and zeroes its discarded tail before preparing its replacement
- AND the new tree omits all mappings wholly beyond EOF

#### Scenario: Large suffix is discarded

- WHEN a shrink removes many complete mapping subtrees
- THEN it removes their references using bounded range/summary traversal
- AND it neither loads the discarded pages/payloads nor scans them to count entries

#### Scenario: File is truncated to zero

- WHEN a BlockSplit file is truncated to zero
- THEN the new manifest has no map root
- AND no old payload or discarded mapping page needs to be fetched

#### Scenario: File extends from a partial final block

- WHEN extension can expose bytes after an old partial materialized EOF
- THEN the old final block's canonical zero padding is verified before the new reference is prepared
- AND untouched new ranges remain holes without payload creation

#### Scenario: Truncated file is extended later

- GIVEN a file previously shrank and detached content
- WHEN it is extended without writing the discarded range
- THEN the newly visible bytes are zero
- AND traversal never falls back to detached references or target listing

#### Scenario: Raw file changes size

- WHEN a Raw file is shrunk, extended, or truncated to zero
- THEN the repository prepares the corresponding complete byte sequence within configured Raw limits
- AND size zero has no payload object

### Requirement: Plaintext Integrity and No-Op Detection

The system SHALL retain BLAKE3-256 canonical plaintext verification and
unchanged-content detection, and SHALL reuse exact references for unchanged
payloads and mapping subtrees. Hash equality SHALL NOT replace authoritative
version publication or conflict handling.

#### Scenario: Prepared block is unchanged

- WHEN a write produces the same canonical block digest as its selected prior reference
- THEN preparation reuses the prior payload reference without a replacement payload upload

#### Scenario: Entire operation is unchanged

- WHEN the resulting logical bytes and EOF are unchanged
- THEN preparation returns the original content reference with content_changed false
- AND performs no payload, page, or manifest PUT

#### Scenario: Subtree is unaffected

- WHEN a mutation changes another range
- THEN pages retained unchanged in the resulting tree reuse their exact references
- AND preparation does not enumerate or rewrite the subtree, allowing only necessary bounded EOF-verification or root-normalization spine reads

#### Scenario: Stored content fails verification

- WHEN loaded plaintext differs from its selected digest
- THEN the repository returns corruption
- AND does not update the expected hash to accept the mismatching bytes

#### Scenario: Hashes match during concurrent work

- WHEN independent operations observe equal content hashes
- THEN hashes may suppress unchanged preparation
- AND publication ordering still depends on the authoritative metadata transaction or standalone CAS

### Requirement: Separate Layout and Representation

The system SHALL represent layout and radix parameters independently from
payload codec, cipher, and hash identifiers. The current representation SHALL
remain identity encoding, no encryption, and BLAKE3-256.

#### Scenario: Current payload is stored

- WHEN Raw or BlockSplit content is prepared
- THEN the root records its method/parameters separately from representation
- AND adding mapping pages does not change plaintext block size or encoding

#### Scenario: Unsupported representation is encountered

- WHEN a root, page profile, or blob names an unsupported representation
- THEN the repository fails explicitly
- AND does not interpret the bytes using an assumed identity fallback

### Requirement: Immutable Preparation and Publication Handoff

The system SHALL prepare immutable payloads, mapping pages, and a compact
manifest separately from authoritative publication. It SHALL use bounded
preflight and ordered preparation, with each changed page location finalized
at most once per preparation identity and attempt.

#### Scenario: Known structural limit is exceeded

- WHEN preflight determines the resulting count, page/root encoding, or planned resource requirement exceeds configured bounds
- THEN preparation returns a typed limit error before any immutable PUT
- AND preflight itself uses bounded traversal and temporary storage

#### Scenario: Content changes across several pages

- WHEN a bounded write changes multiple leaves and ancestors
- THEN preparation uploads changed payloads and pages incrementally in child-before-parent order
- AND releases uploaded payload buffers while retaining only the bounded frontier

#### Scenario: Same page would change several times during a request

- WHEN multiple modified children belong to one ancestor page
- THEN the ancestor is finalized and uploaded once after all those child updates are known
- AND no intermediate bytes compete for the same immutable page key

#### Scenario: Immutable creation is repeated or uncertain

- WHEN a page PUT returns AlreadyExists or an ambiguous outcome
- THEN bounded exact-byte readback must establish the desired immutable content before dependent publication proceeds
- AND mismatching bytes cause collision/corruption rather than replacement

#### Scenario: Content is ready for an inode transaction

- WHEN all reachable new dependencies and the root are acknowledged durable
- THEN the repository returns PreparedContent with the bounded ContentRef and complete preparation identity
- AND does not itself publish inode metadata or a clustered S3 head

#### Scenario: Preparation is interrupted or fails after uploads

- WHEN work fails, is cancelled, or exhausts target-driven recovery before publication
- THEN the old published reference remains authoritative
- AND uploaded objects may remain unreachable without exposing partial content or deleting old reader dependencies

### Requirement: Data-Before-Metadata Durability

The system SHALL enforce durable dependency ordering from payloads through
their leaf pages, branch ancestors, compact manifest, and authoritative
publication. A parent SHALL NOT become an acknowledged dependency of a
successful preparation while any referenced new child's creation is unresolved.

#### Scenario: Child creation fails or remains ambiguous

- WHEN a payload or mapping page is not confirmed durable
- THEN no successful preparation or publication may select a root dependent on that child
- AND the previous published file version remains complete

#### Scenario: Clustered publication succeeds

- WHEN the metadata transaction accepts the prepared root and revalidates the current base and writer authority
- THEN ContentRef, inode size, relevant timestamps/generations, and retained mutation result publish atomically
- AND readers using either selected root never assemble a mixture caused by independently replaced block keys

#### Scenario: Standalone publication is exercised

- WHEN conformance uses ObjectHeadPublisher
- THEN the same dependency ordering precedes its single-key CAS
- AND that head remains a standalone mechanism rather than a second clustered authority

#### Scenario: Content-only sync follows publication

- WHEN content-only sync validates the selected root under the write-through contract
- THEN it requires no hidden background flush or full-tree scan
- AND it does not claim a payload scrub or filesystem metadata durability beyond the selected layer

### Requirement: Bounded Storage Amplification

The system SHALL separately bound request bytes, Raw materialization, compact
root bytes, page bytes/slots/depth, materialized-count quota, per-operation page
work, resident mapping working memory, and retries before corresponding
amplification. It SHALL remove the assumption that the full block map fits in
one allocation or encoded object.

#### Scenario: Large map receives a small request

- WHEN a fixed-size positioned operation accesses a file with many unrelated blocks
- THEN mapping retention is bounded by the active frontier and configured page/depth limits
- AND it does not allocate a collection proportional to all file entries or pages

#### Scenario: Page budget is checked

- WHEN a page is fetched, decoded, copied, or encoded
- THEN the operation accounts for encoded buffers, decoded structures/key capacities, frontier and reconciliation copies
- AND the encoded page cap is not represented as a total heap or process RSS cap

#### Scenario: Work limit would be exceeded

- WHEN the next repository-level page operation would cross the configured work budget
- THEN it fails before that dispatch
- AND no incomplete content reference is successfully prepared or published

#### Scenario: Preflight and preparation repeat immutable reads

- WHEN a mutation uses two bounded passes
- THEN their combined planned work is checked and their live working sets are bounded
- AND the optimization does not retain all first-pass pages or payload buffers

#### Scenario: Logical range or tree arithmetic overflows

- WHEN a request end, index conversion, count, or routing calculation cannot be represented safely
- THEN the repository returns a typed error before the invalid allocation or target mutation
- AND top-level tree coverage is not computed as an overflowing u64 byte endpoint

#### Scenario: Writers repeatedly conflict

- WHEN publication conflicts reach the existing configured retry bound
- THEN the mutation terminates with the documented conflict outcome
- AND retries do not accumulate unbounded attempts or live page frontiers

### Requirement: Deterministic Storage Conformance

The system SHALL provide deterministic conformance for Raw and paged
BlockSplit logical behavior, checked formats, resource bounds, immutable
preparation, publication ordering, and injected failures without requiring a
network runtime. Adapter suites SHALL exercise the same paged repository.

#### Scenario: Logical operation trace is replayed

- WHEN create, positioned read/write, and truncate traces cross leaf and branch boundaries
- THEN results match a byte-vector or sparse interval reference model with exact EOF
- AND identical identities and inputs produce deterministic preparation behavior

#### Scenario: Resources are measured on large maps

- WHEN small-range operations run over synthetic maps exceeding the former flat-map bound
- THEN traces and operation-local allocation accounting remain within the depth/page/frontier contract
- AND authoritative fixture storage is excluded from the repository working-set measurement

#### Scenario: Failure is injected along the dependency graph

- WHEN failure occurs before or after payload, leaf, branch, root, or publication acknowledgement
- THEN independent reopen observes the complete old or complete new selected version
- AND never a partially published mixture

#### Scenario: Independent clients compete

- WHEN separately constructed clients prepare overlapping or disjoint changes from a stale base
- THEN conflicting publication cannot overwrite the current state blindly
- AND the existing re-read/revalidate/reprepare flow preserves a valid serial result

#### Scenario: Adapter conformance executes

- WHEN a supported target or compatibility-only test wrapper runs repository conformance
- THEN it exercises real page boundaries with bounded data and independent clients
- AND retains its existing durability/qualification distinctions
