# Delta for Filesystem State Store

## ADDED Requirements

### Requirement: Opaque File Content Metadata

The system SHALL own a bounded generic ContentMetadata record for each managed
regular file, keyed by filesystem and content FileId. It SHALL contain owner
inode/context identity, immutable opaque storage policy, optional immutable key
commitment, optional wrapped-key bytes and a record revision. State SHALL not
parse algorithms, generate keys or execute cryptography.

#### Scenario: Record is constructed

- WHEN content metadata is supplied
- THEN identifiers, optional-field shape, policy/envelope byte limits and total retained size are checked
- AND interpretation of policy/wrapping bytes remains the storage helper's responsibility

#### Scenario: Inode references its context

- WHEN a regular inode is validated
- THEN its content FileId and context ID match its original owner-bound metadata record
- AND an unrelated file/inode/context cannot be substituted

#### Scenario: Context is read with inode state

- WHEN a content operation requests inode and selected context
- THEN they are returned from one consistent bounded state snapshot
- AND reading them does not expose plaintext KEKs or DEKs

#### Scenario: Metadata records are enumerated

- WHEN a caller scans content metadata
- THEN keyset cursors, item limits and retained-byte limits bound the page
- AND the adapter does not load all file contexts or unwrapped keys

### Requirement: Atomic Content Context Creation

The system SHALL create a regular inode with unpublished empty content and its
metadata context atomically with the namespace/open/pin changes and retained
semantic result. A new context SHALL not be reserved in a separate transaction
or attached to an already existing or different inode.

#### Scenario: Create succeeds

- WHEN candidate opaque metadata is included in a valid fenced create transaction
- THEN the inode, context, directory entry and applicable open/parent/allocation updates commit together
- AND the selected context identity is recoverable through the result and authoritative state

#### Scenario: Create fails before commitment

- WHEN authorization, namespace, bounds or transaction validation rejects creation
- THEN neither a new inode nor a new context record is committed
- AND the caller has no authoritative candidate from which to prepare encrypted S3 content

#### Scenario: Duplicate creation has different random candidates

- WHEN an exact semantic create retry presents a different generated candidate
- THEN ledger replay selects the original committed result and context
- AND random allocation results do not replace the original key or mask different semantic operands

#### Scenario: Creation outcome is ambiguous

- WHEN commit acknowledgement is lost
- THEN the caller resolves the original operation through the authoritative ledger/state before content preparation
- AND no unconfirmed local candidate is treated as the winning file key

#### Scenario: Create is replayed after inode retirement

- WHEN the create result is retained but its inode has since been retired
- THEN exact replay returns the original terminal result without requiring a live-inode context query
- AND it does not recreate the inode, regenerate a file key or prepare new S3 content

#### Scenario: Distinct creators compete for one name

- WHEN two different create operations race
- THEN normal namespace transaction rules select a valid winner or conflict
- AND the losing operation cannot adopt or overwrite the winner's key context as if it were an exact replay

### Requirement: Immutable Context Identity and Rewrap

The system SHALL freeze owner/file/context identity, policy and key commitment
after creation. A dedicated RewrapContentMetadata transition MAY replace only
the opaque wrapped-key envelope/revision under exact revision, authority and
idempotency checks. Generic Replace/Delete SHALL not bypass these rules.

#### Scenario: Rewrap commits

- WHEN an authorized caller submits a new envelope with the matching context revision and writer fence
- THEN wrapped bytes, record revision, change event and terminal result publish atomically
- AND immutable context/policy/commitment, inode content generations and ContentRef do not change

#### Scenario: Rewrap is stale or inconsistent

- WHEN its context ID, revision, authority or immutable binding disagrees
- THEN the transaction rejects or conflicts without replacing the envelope
- AND a plain context without a wrapped key cannot be rewrapped

#### Scenario: Rewrap is retried

- WHEN the same semantic administrative mutation is replayed
- THEN the retained exact result is returned through the normal ledger protocol
- AND the envelope is not regenerated or applied a second time by state

#### Scenario: Underlying key equivalence is checked

- WHEN a rewrap candidate is produced
- THEN the external crypto helper verifies it preserves the DEK commitment
- AND state compares only bounded opaque bindings without claiming to decrypt or cryptographically prove those bytes

#### Scenario: Generic mutation attempts a key replacement

- WHEN ordinary record replacement or deletion would alter policy/key identity or discard content metadata
- THEN the operation is rejected
- AND dedicated transition and retention checks cannot be bypassed

### Requirement: Retained Content Metadata Lifetime

ContentMetadata records SHALL survive inode retirement whenever retained content
may still require them. This change SHALL conservatively retain detached records
until a future reachability-aware collection contract exists.

#### Scenario: File is renamed or hard-linked

- WHEN namespace names change or additional links are created
- THEN the inode retains the same file/context/key association

#### Scenario: Unlinked file remains open

- WHEN its last directory link is removed but open pins remain
- THEN the inode, context and content remain resolvable according to existing open-unlinked semantics

#### Scenario: Last close retires the inode

- WHEN the existing lifetime rules retire an unpinned zero-link inode
- THEN its content-metadata record is retained without requiring the inode to remain
- AND no cascade or generic deletion erases keys for old roots

#### Scenario: Old content is reopened

- WHEN an authorized retained reader resolves an older ContentRef after inode retirement
- THEN its content FileId can still locate the same retained context
- AND opening does not depend on a cached plaintext key

#### Scenario: Key deletion or secure erasure is assumed

- WHEN an inode or S3 object is removed
- THEN no immediate key-GC or crypto-erasure guarantee is inferred
- AND retained metadata, backups/WAL and referenced old content remain explicit lifetime considerations

## MODIFIED Requirements

### Requirement: Bounded Validated Authoritative Records

The system SHALL define checked versioned records for filesystems, inodes,
directories, content metadata, opens, orphans, locks, xattrs, mutations and leases.
Representation policy and wrapped keys SHALL be opaque bounded metadata to state.

#### Scenario: Record is constructed

- WHEN a caller constructs an authoritative record
- THEN names, values, counts, sizes, ranges, generations and kind-specific fields are validated
- AND inconsistent records fail before entering a commit request

#### Scenario: Regular-file content is present

- WHEN an inode carries a ContentRef
- THEN content FileId, size and data generation agree with that reference and its immutable context association
- AND mismatches are rejected as malformed transitions

#### Scenario: Record kind and key disagree

- WHEN a value is paired with a different record family or identity
- THEN validation fails explicitly without reinterpreting the value

### Requirement: Immutable Content Publication Handoff

The system SHALL publish prepared immutable content only through the same
authoritative transaction that updates inode metadata and retained result,
validating its nonsecret context/policy/key binding and observed context revision.

#### Scenario: Prepared content is published

- GIVEN all immutable content dependencies and the root are acknowledged durable
- WHEN PublishContent validates base and matching authoritative context
- THEN ContentRef, size, timestamps, data generation and terminal result commit together
- AND state performs no target-object or cryptographic operation

#### Scenario: Prepared identity or key context is inconsistent

- WHEN mutation, file, base, size, generation, context, policy or key commitment differs from authoritative state
- THEN publication is rejected before metadata mutation
- AND the prior content remains current

#### Scenario: Opaque policy binding is compared

- WHEN a prepared result supplies its policy binding
- THEN state compares exact bounded policy format and bytes with the selected metadata record
- AND state does not decode the policy or invoke a cryptographic hash/KDF to establish equality

#### Scenario: Context or inode revision changes after preparation

- WHEN an authoritative precondition changes before publication, including a concurrent rewrap
- THEN the commit publishes none of the new inode state
- AND the caller reloads/revalidates the committed context without generating another file key

#### Scenario: First content is prepared after defaults change

- WHEN an unpublished inode receives its first content mutation
- THEN its committed context supplies policy and key identity
- AND preparation cannot silently select new process defaults
